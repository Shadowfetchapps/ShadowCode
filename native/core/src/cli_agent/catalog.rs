//! Vendor catalog: the one place that knows, per subscription runtime, whether
//! it is Ready / Sign in / Setup required / Unavailable, which models it
//! offers, what it accepts on the wire, and what usage it reported.
//!
//! Every fact comes from an official interface of the installed runtime
//! (`codex app-server`, `claude auth status`, Cursor and Grok ACP handshakes,
//! `agy models`). Refreshes are bounded: a vendor is re-probed at most once per
//! `MIN_REFRESH_SECS` unless forced, and failures back off exponentially. The
//! catalog never marks a vendor Ready because a binary or credential file
//! exists, and never invents usage.
use super::{
    acp_probe, auth, codex_probe, doctor,
    picker::{self, Availability, PickerTarget},
    resolve_binary,
    usage::UsageSnapshot,
    CliAgentsConfig, Vendor,
};
use crate::store::{Store, UsageRow};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime},
};
use tokio::sync::broadcast;

/// Persisted usage rows keep the whole official payload under this pool key;
/// per-model pools are derived from it when rows are built.
pub const PERSISTED_POOL: &str = "*";

pub const MIN_REFRESH_SECS: f64 = 5.0 * 60.0;
const MAX_BACKOFF_SECS: f64 = 60.0 * 60.0;
const PROBE_TIMEOUT: Duration = Duration::from_secs(40);

#[derive(Clone, Debug, PartialEq, Eq)]
struct BinaryStamp {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    file_id: (u64, u64),
}

fn binary_stamp(vendor: Vendor, config: &CliAgentsConfig) -> Option<BinaryStamp> {
    let path = if vendor == Vendor::Antigravity {
        super::antigravity_server::installation(config.binary(vendor)).map(|i| i.server)
    } else {
        resolve_binary(config.binary(vendor))
    }?;
    let metadata = fs::metadata(&path).ok()?;
    #[cfg(unix)]
    use std::os::unix::fs::MetadataExt;
    Some(BinaryStamp {
        path,
        len: metadata.len(),
        modified: metadata.modified().ok(),
        #[cfg(unix)]
        file_id: (metadata.dev(), metadata.ino()),
    })
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VendorModel {
    /// Exact id the runtime accepts (Codex model id, ACP modelId, agy slug,
    /// Claude alias). `auto` / `default` mean the runtime's own choice.
    pub id: String,
    pub label: String,
    pub is_default: bool,
    /// The catalog says this model takes image input (Codex
    /// `inputModalities`); protocol-level support is separate.
    pub vision: bool,
    /// The runtime says whether this model takes a reasoning effort (Claude
    /// Code `supportsEffort`). `None`: not reported per model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<bool>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AccountInfo {
    pub email: Option<String>,
    pub plan: Option<String>,
    /// `chatgpt`, `apiKey`, `cursor_login`, `grok.com`, …
    pub auth_mode: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct VendorStatus {
    pub vendor: Vendor,
    pub availability: Availability,
    pub detail: String,
    pub version: Option<String>,
    pub binary: Option<PathBuf>,
    /// In-memory identity of the executable that supplied these capabilities.
    #[serde(skip)]
    binary_stamp: Option<BinaryStamp>,
    pub account: Option<AccountInfo>,
    pub models: Vec<VendorModel>,
    /// The runtime's protocol accepts image bytes from ShadowCode.
    pub accepts_images: bool,
    /// Approval prompts reach ShadowCode (false: the runtime applies its own
    /// permission settings and never asks).
    pub asks_approval: bool,
    /// The CLI documents a reasoning-effort flag (`claude --effort`);
    /// `None` when not checked.
    #[serde(skip)]
    pub effort_flag: Option<bool>,
    pub fetched_at: f64,
    pub error: Option<String>,
    /// Raw provider usage payload (Codex rate limits) used to derive per-model
    /// snapshots; None when the provider exposes nothing.
    #[serde(skip)]
    pub usage_raw: Option<Value>,
    /// When `usage_raw` was fetched (a probe or a push during a turn).
    #[serde(skip)]
    pub usage_at: f64,
    /// Why usage is unavailable, when it is.
    pub usage_note: Option<String>,
    #[serde(skip)]
    pub failures: u32,
    #[serde(skip)]
    pub next_allowed: f64,
}

impl VendorStatus {
    fn setup_required(vendor: Vendor, configured: &str, now: f64) -> Self {
        Self {
            vendor,
            availability: Availability::SetupRequired,
            detail: if vendor == Vendor::Antigravity {
                format!("Not installed. {}", vendor.install_hint())
            } else {
                format!(
                    "Not installed: `{configured}` was not found on PATH. {}",
                    vendor.install_hint()
                )
            },
            version: None,
            binary: None,
            binary_stamp: None,
            account: None,
            models: Vec::new(),
            accepts_images: false,
            asks_approval: vendor.asks_approval(),
            effort_flag: None,
            fetched_at: now,
            error: None,
            usage_raw: None,
            usage_at: 0.0,
            usage_note: None,
            failures: 0,
            next_allowed: now,
        }
    }
    /// Nothing probed yet in this process; only persisted usage is known.
    fn unchecked(vendor: Vendor, persisted: Option<&UsageRow>) -> Self {
        let mut status = Self::setup_required(vendor, vendor.binary(), 0.0);
        status.availability = Availability::Unavailable;
        status.detail = "Not checked yet".into();
        status.fetched_at = 0.0;
        if let Some(row) = persisted {
            status.usage_raw = Some(row.payload.clone());
            status.usage_at = row.fetched_at;
            if !row.account.is_empty() {
                status.account = Some(AccountInfo {
                    email: Some(row.account.clone()),
                    plan: None,
                    auth_mode: None,
                });
            }
        }
        status
    }
    /// The CLI is signed in with an API key (Codex `apiKey`, Claude auth
    /// method other than claude.ai): turns are billed per token.
    pub fn api_key_login(&self) -> bool {
        let Some(mode) = self.account.as_ref().and_then(|a| a.auth_mode.as_deref()) else {
            return false;
        };
        match self.vendor {
            Vendor::Codex => mode == "apiKey",
            Vendor::Claude => mode != "claude.ai",
            _ => false,
        }
    }
    /// ACP reports login readiness, not account billing. Codex's boolean
    /// login-status fallback likewise cannot establish subscription billing.
    fn billing_unverified(&self) -> bool {
        match self.vendor {
            Vendor::Cursor | Vendor::Antigravity | Vendor::Grok => true,
            Vendor::Codex => !matches!(
                self.account
                    .as_ref()
                    .and_then(|account| account.auth_mode.as_deref()),
                Some("chatgpt" | "apiKey")
            ),
            Vendor::Claude => false,
        }
    }
    fn billing(&self) -> &'static str {
        if self.api_key_login() {
            "api_key"
        } else if self.billing_unverified() {
            "unknown"
        } else {
            "subscription"
        }
    }
    /// Account identity used to key persisted usage (never a display name).
    pub fn account_key(&self) -> String {
        self.account
            .as_ref()
            .and_then(|a| a.email.clone())
            .or_else(|| {
                self.usage_raw
                    .as_ref()
                    .and_then(|raw| raw["accountId"].as_str().map(str::to_owned))
            })
            .unwrap_or_default()
    }
    fn disabled(vendor: Vendor, now: f64) -> Self {
        let mut status = Self::setup_required(vendor, vendor.binary(), now);
        status.availability = Availability::Unavailable;
        status.detail =
            "Disabled in Settings › Advanced; ShadowCode will not start this runtime".into();
        status
    }
    /// Usage for one model row of this vendor.
    pub fn usage_for(&self, model: &str, now: f64) -> UsageSnapshot {
        let provider = self.vendor.provider();
        if self.api_key_login() {
            return UsageSnapshot::api_key_login(&provider);
        }
        if self.vendor == Vendor::Codex && self.billing_unverified() && self.fetched_at > 0.0 {
            // Do not attach the previous login's plan numbers to a newly
            // probed, unidentified login. Before any probe, persisted receipts
            // remain explicitly stale historical observations as before.
            return UsageSnapshot::unavailable_because(
                &provider,
                picker::UNVERIFIED_BILLING_DETAIL,
            );
        }
        let snap = match (&self.usage_raw, self.vendor) {
            (Some(raw), Vendor::Codex) => {
                let model = if model.is_empty() || model == "default" || model == "auto" {
                    self.models
                        .iter()
                        .find(|m| m.is_default)
                        .map(|m| m.id.as_str())
                } else {
                    Some(model)
                };
                UsageSnapshot::from_codex(raw, model, self.usage_at)
            }
            (Some(raw), Vendor::Claude) => UsageSnapshot::from_claude(
                raw,
                self.account.as_ref().and_then(|a| a.plan.as_deref()),
                self.usage_at,
            ),
            _ => UsageSnapshot::unavailable_because(
                &provider,
                self.usage_note.as_deref().unwrap_or(""),
            ),
        };
        // Never probed in this process (persisted only), or old: say when.
        if snap.last_refresh.is_some() && (self.fetched_at == 0.0 || snap.is_stale(now)) {
            snap.mark_stale(now)
        } else {
            snap
        }
    }
    /// Compact doctor-style JSON kept for the Accounts page and Doctor.
    pub fn to_doctor_json(&self) -> Value {
        let state = match self.availability {
            Availability::Ready => "ready",
            Availability::SignIn => "not_logged_in",
            Availability::SetupRequired => "not_installed",
            Availability::Unavailable => "unavailable",
        };
        let status = match self.availability {
            Availability::Ready => "pass",
            Availability::Unavailable => "info",
            _ => "warn",
        };
        json!({
            "id": format!("cli-{}", self.vendor.id()),
            "label": if self.vendor == Vendor::Antigravity {
                "Antigravity (Google's ACP agent)".to_owned()
            } else {
                format!("{} ({} CLI)", self.vendor.product_label(), self.vendor.binary())
            },
            "product": self.vendor.product_label(),
            "state": state,
            "status": status,
            "availability": self.availability,
            "availability_label": self.availability.label(),
            "detail": self.detail,
            "version": self.version,
            "binary": self.binary,
            "fix": match self.availability {
                Availability::SignIn => self.vendor.login_hint(),
                Availability::SetupRequired => self.vendor.install_hint(),
                _ => "",
            },
            "account": self.account,
            "models": self.models,
            "accepts_images": self.accepts_images,
            "asks_approval": self.asks_approval,
            "fetched_at": self.fetched_at,
            "error": self.error,
            "usage_note": self.usage_note,
            "login_command": self.vendor.login_command(),
            "logout_command": self.vendor.logout_command(),
            "shared_cli_note": self.vendor.shared_cli_note(),
            "billing": self.billing(),
            "usage": self.usage_for("default", crate::now()),
            // Antigravity's agent server is installed by ShadowCode on request.
            "install": (self.vendor == Vendor::Antigravity).then(|| {
                super::antigravity_server::install_status(
                    &self.binary.as_ref().map(|b| b.display().to_string()).unwrap_or_default(),
                )
            }),
        })
    }
}

/// Only short cache/database operations hold this mutex. No provider future,
/// filesystem catalog scan or other await runs while it is held. Keeping the
/// status and persisted observation together makes disconnect/invalidation
/// atomic with publication; separate async locks left a persist-after-forget gap.
#[derive(Default)]
struct CatalogState {
    entries: HashMap<Vendor, VendorStatus>,
    persisted: HashMap<Vendor, UsageRow>,
    identities: HashMap<Vendor, ProbeIdentity>,
    revisions: HashMap<Vendor, u64>,
    configured: bool,
    offline: bool,
}

#[derive(Clone, PartialEq, Eq)]
struct ProbeIdentity {
    enabled: bool,
    configured_binary: String,
    binary: Option<BinaryStamp>,
}

impl ProbeIdentity {
    fn new(vendor: Vendor, config: &CliAgentsConfig) -> Self {
        Self {
            enabled: config.vendor_enabled(vendor),
            configured_binary: config.binary(vendor).into(),
            binary: binary_stamp(vendor, config),
        }
    }
}

impl CatalogState {
    fn invalidate(&mut self, vendor: Vendor) {
        *self.revisions.entry(vendor).or_default() += 1;
        self.entries.remove(&vendor);
    }
    fn observe(&mut self, vendor: Vendor, identity: ProbeIdentity) {
        if self.identities.get(&vendor) != Some(&identity) {
            self.invalidate(vendor);
            self.identities.insert(vendor, identity);
        }
    }
    fn status(&self, vendor: Vendor) -> VendorStatus {
        let mut status = self
            .entries
            .get(&vendor)
            .cloned()
            .unwrap_or_else(|| VendorStatus::unchecked(vendor, self.persisted.get(&vendor)));
        if self
            .identities
            .get(&vendor)
            .is_some_and(|identity| !identity.enabled)
        {
            return VendorStatus::disabled(vendor, crate::now());
        }
        if self.offline {
            status.availability = Availability::Unavailable;
            status.detail = "Offline mode: cloud models need the network. Switch network mode to Online in Settings.".into();
        }
        status
    }
}

#[derive(Default)]
pub struct VendorCatalog {
    state: Mutex<CatalogState>,
    store: Option<Arc<Store>>,
    events: Option<broadcast::Sender<Value>>,
    logins: auth::Logins,
    #[cfg(test)]
    publication_signal: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

impl VendorCatalog {
    /// In-memory only (tests, probes).
    pub fn new() -> Self {
        Self::default()
    }

    /// Catalog backed by the profile database: persisted usage snapshots are
    /// loaded now, so "Last checked …" is known before the first refresh.
    pub fn with_store(store: Arc<Store>, events: broadcast::Sender<Value>) -> Self {
        let mut persisted = HashMap::new();
        for row in store.usage_snapshots().unwrap_or_default() {
            if let Some(vendor) = Vendor::parse(&row.vendor) {
                persisted.entry(vendor).or_insert(row);
            }
        }
        Self {
            state: Mutex::new(CatalogState {
                persisted,
                ..Default::default()
            }),
            store: Some(store),
            events: Some(events),
            ..Default::default()
        }
    }

    /// Called synchronously when the host reads current configuration, before
    /// handing it to async work. Later calls carrying an old Config cannot
    /// reinstall that configuration or publish its outstanding probe.
    /// The cached state. It is only a cache, so a panic while it was held
    /// (a bug elsewhere) must not make every later picker read, task start or
    /// sign-in panic too.
    fn state(&self) -> std::sync::MutexGuard<'_, CatalogState> {
        self.state.lock().unwrap_or_else(|poisoned| {
            self.state.clear_poison();
            poisoned.into_inner()
        })
    }

    pub fn configure(&self, config: &CliAgentsConfig, offline: bool) {
        let identities: Vec<_> = Vendor::ALL
            .into_iter()
            .map(|vendor| (vendor, ProbeIdentity::new(vendor, config)))
            .collect();
        let mut state = self.state();
        if state.offline != offline {
            for vendor in Vendor::ALL {
                *state.revisions.entry(vendor).or_default() += 1;
            }
            state.offline = offline;
        }
        for (vendor, identity) in identities {
            state.observe(vendor, identity);
        }
        state.configured = true;
    }

    /// Follow the configured network mode (set whenever config is read).
    pub fn set_offline(&self, offline: bool) {
        let mut state = self.state();
        if state.offline != offline {
            for vendor in Vendor::ALL {
                *state.revisions.entry(vendor).or_default() += 1;
            }
            state.offline = offline;
        }
    }
    pub fn is_offline(&self) -> bool {
        self.state().offline
    }
    pub fn logins(&self) -> &auth::Logins {
        &self.logins
    }
    /// Broadcast a non-session event (`account.login`, `usage.updated`).
    /// The broadcast is a wakeup; login lines are also kept in `logins()`.
    pub fn broadcast(&self, kind: &str, payload: Value) {
        if let Some(events) = &self.events {
            let _ = events.send(json!({
                "type": kind, "ts": crate::now(), "session_id": null,
                "task_id": null, "payload": payload,
            }));
        }
    }

    /// Forget the cached status of a vendor so the next refresh re-probes
    /// (after a sign-in). Persisted usage is kept.
    pub async fn clear(&self, vendor: Vendor) {
        self.state().invalidate(vendor);
    }
    pub async fn forget_status(&self, vendor: Vendor) {
        self.clear(vendor).await;
    }
    /// Forget everything tied to the vendor login (disconnect): cached
    /// status, persisted usage, and native session ids of conversations.
    pub async fn forget(&self, vendor: Vendor) -> anyhow::Result<()> {
        let mut state = self.state();
        state.invalidate(vendor);
        state.persisted.remove(&vendor);
        if let Some(store) = &self.store {
            store.delete_usage_snapshots(vendor.id())?;
            store.clear_session_meta_prefix(&crate::store::keys::native_session(vendor.id()))?;
        }
        Ok(())
    }

    /// Merge a rate-limit snapshot pushed during a turn (Codex
    /// `account/rateLimits/updated`, Claude Code `rate_limit_event`) and
    /// return the row usage for `model`.
    pub async fn apply_rate_limits(
        &self,
        vendor: Vendor,
        snapshot: &Value,
        model: &str,
    ) -> UsageSnapshot {
        let now = crate::now();
        let mut state = self.state();
        let Some(entry) = state.entries.get_mut(&vendor) else {
            return UsageSnapshot::from_vendor(vendor, snapshot, None, model, now);
        };
        let payload = if vendor == Vendor::Claude {
            // Each event describes every plan window Claude knows about.
            entry.usage_raw = Some(snapshot.clone());
            snapshot.clone()
        } else {
            let raw = entry.usage_raw.get_or_insert_with(|| json!({}));
            // What an earlier probe stored may be anything the CLI sent.
            if !raw.is_object() {
                *raw = json!({});
            }
            let limit_id = snapshot["limitId"].as_str().unwrap_or("codex").to_owned();
            let default_id = raw["rateLimits"]["limitId"]
                .as_str()
                .unwrap_or("codex")
                .to_owned();
            if limit_id == default_id {
                raw["rateLimits"] = snapshot.clone();
            }
            if !raw["rateLimitsByLimitId"].is_object() {
                raw["rateLimitsByLimitId"] = json!({});
            }
            raw["rateLimitsByLimitId"][limit_id.as_str()] = snapshot.clone();
            raw.clone()
        };
        entry.usage_at = now;
        let usage = entry.usage_for(model, now);
        let row = UsageRow {
            vendor: vendor.id().into(),
            account: entry.account_key(),
            pool: PERSISTED_POOL.into(),
            fetched_at: now,
            payload,
        };
        self.persist(&mut state, vendor, row);
        usage
    }
    /// Usage to report with `limit.reached`: the cached numbers when known.
    pub async fn limit_usage(&self, vendor: Vendor, model: &str, detail: &str) -> UsageSnapshot {
        match self.state().entries.get(&vendor) {
            Some(entry) if entry.usage_raw.is_some() => entry
                .usage_for(model, crate::now())
                .with_limit_reached(detail),
            _ => UsageSnapshot::limit_reached(&vendor.provider(), detail),
        }
    }
    fn persist(&self, state: &mut CatalogState, vendor: Vendor, row: UsageRow) {
        if let Some(store) = &self.store {
            if state
                .persisted
                .get(&vendor)
                .is_some_and(|old| old.account != row.account)
            {
                let _ = store.delete_usage_snapshots(vendor.id());
            }
            let _ = store.upsert_usage_snapshot(&row);
        }
        state.persisted.insert(vendor, row);
    }
    fn drop_persisted(&self, state: &mut CatalogState, vendor: Vendor) {
        state.persisted.remove(&vendor);
        if let Some(store) = &self.store {
            let _ = store.delete_usage_snapshots(vendor.id());
        }
    }

    /// Accounts JSON from what is already known, without probing: cached
    /// status, or persisted usage marked "Last checked …".
    pub async fn status_cached_json(&self) -> Value {
        let state = self.state();
        statuses_json(&Vendor::ALL.map(|vendor| state.status(vendor)))
    }
    pub async fn cached(&self, vendor: Vendor) -> Option<VendorStatus> {
        let state = self.state();
        state
            .entries
            .contains_key(&vendor)
            .then(|| state.status(vendor))
    }

    /// Snapshot only: does not start provider processes or refresh network
    /// metadata. Targets and account rows use the same cache observation.
    pub async fn picker_cached(&self, config: &CliAgentsConfig) -> (Vec<PickerTarget>, Value) {
        let statuses = self.snapshot_statuses(config);
        let json = statuses_json(&statuses);
        (picker_rows_from(&statuses, config), json)
    }

    fn snapshot_statuses(&self, config: &CliAgentsConfig) -> Vec<VendorStatus> {
        let identities: Vec<_> = Vendor::ALL
            .into_iter()
            .map(|vendor| (vendor, ProbeIdentity::new(vendor, config)))
            .collect();
        {
            let mut state = self.state();
            identities
                .into_iter()
                .map(|(vendor, identity)| {
                    if !state.configured {
                        state.observe(vendor, identity.clone());
                    }
                    if state.identities.get(&vendor) != Some(&identity) {
                        return changed_configuration(vendor);
                    }
                    state.status(vendor)
                })
                .collect::<Vec<_>>()
        }
    }

    /// The post-login observation retains its owned children through cleanup.
    /// The supervisor awaits this operation; it must not select/drop it during
    /// cancellation because kill-on-drop alone does not await process reaping.
    pub(super) async fn refresh_for_login(
        &self,
        vendor: Vendor,
        config: &CliAgentsConfig,
        cancel: tokio_util::sync::CancellationToken,
        deadline: tokio::time::Instant,
        publication: super::probe_lifecycle::PublicationGate,
    ) -> VendorStatus {
        super::probe_lifecycle::scope_with_gate(
            cancel,
            deadline,
            publication,
            self.refresh(vendor, config, true),
        )
        .await
    }

    /// Refresh one vendor unless a recent probe exists. `force` skips the
    /// freshness window but never the failure backoff (connect/disconnect
    /// clear the entry first, so they always probe).
    pub async fn refresh(
        &self,
        vendor: Vendor,
        config: &CliAgentsConfig,
        force: bool,
    ) -> VendorStatus {
        if login_probe_stopped() {
            return interrupted_login_check(vendor);
        }
        let now = crate::now();
        let identity = ProbeIdentity::new(vendor, config);
        let (previous, revision) = {
            let mut state = self.state();
            if !state.configured {
                state.observe(vendor, identity.clone());
            }
            if state.identities.get(&vendor) != Some(&identity) {
                return changed_configuration(vendor);
            }
            if state.offline || !identity.enabled {
                return state.status(vendor);
            }
            let previous = state.entries.get(&vendor).cloned();
            if let Some(existing) = &previous {
                let fresh = now - existing.fetched_at < MIN_REFRESH_SECS;
                let backing_off = existing.next_allowed > now;
                if backing_off || (!force && fresh) {
                    return existing.clone();
                }
            }
            let revision = state.revisions.entry(vendor).or_default();
            *revision += 1;
            (previous, *revision)
        };
        let mut status = probe_vendor(vendor, config, now).await;
        // Controlled helpers have already reaped their child before returning.
        // A cancelled/expired observation cannot persist account or quota data.
        if login_probe_stopped() {
            return interrupted_login_check(vendor);
        }
        // An executable replaced while its old process was probing is not a
        // current capability observation, even without another config read.
        let unchanged_binary = identity.binary == binary_stamp(vendor, config);
        #[cfg(test)]
        if let Some(signal) = self.publication_signal.lock().unwrap().take() {
            let _ = signal.send(());
        }
        let mut state = self.state();
        // Lock order: catalog state -> per-login publication gate. Cancellation
        // uses login registry -> gate; publication never takes the registry.
        // No await occurs under either lock. An already published observation
        // may precede a later cancellation; an accepted cancellation that wins
        // the gate prevents any following account/quota commit.
        let commit = || {
            if !unchanged_binary || state.identities.get(&vendor) != Some(&identity) {
                return changed_configuration(vendor);
            }
            if state.offline || state.revisions.get(&vendor) != Some(&revision) {
                return state.status(vendor);
            }
            if status.usage_raw.is_some() {
                status.usage_at = now;
            }
            if status.error.is_some() {
                let failures = previous.as_ref().map(|p| p.failures + 1).unwrap_or(1);
                status.failures = failures;
                status.next_allowed =
                    now + (30.0 * 2f64.powi(failures.min(7) as i32)).min(MAX_BACKOFF_SECS);
                if status.usage_raw.is_none() {
                    if let Some(previous) = previous.as_ref().filter(|p| p.usage_raw.is_some()) {
                        status.usage_raw = previous.usage_raw.clone();
                        status.usage_at = previous.usage_at;
                    } else if let Some(row) = state.persisted.get(&vendor) {
                        status.usage_raw = Some(row.payload.clone());
                        status.usage_at = row.fetched_at;
                    }
                }
                if let Some(previous) = previous {
                    if status.models.is_empty() {
                        status.models = previous.models;
                    }
                }
            } else if let Some(raw) = status.usage_raw.clone() {
                let row = UsageRow {
                    vendor: vendor.id().into(),
                    account: status.account_key(),
                    pool: PERSISTED_POOL.into(),
                    fetched_at: now,
                    payload: raw,
                };
                self.persist(&mut state, vendor, row);
            } else if matches!(status.availability, Availability::SignIn) || status.api_key_login()
            {
                self.drop_persisted(&mut state, vendor);
            } else if vendor == Vendor::Claude && status.availability == Availability::Ready {
                // Claude reports plan usage only during turns: keep the
                // last report of the same account; it turns stale with age.
                let account = status.account_key();
                if let Some(previous) = previous
                    .as_ref()
                    .filter(|p| p.usage_raw.is_some() && p.account_key() == account)
                {
                    status.usage_raw = previous.usage_raw.clone();
                    status.usage_at = previous.usage_at;
                } else if let Some(row) = state
                    .persisted
                    .get(&vendor)
                    .filter(|r| r.account == account)
                {
                    status.usage_raw = Some(row.payload.clone());
                    status.usage_at = row.fetched_at;
                }
            }
            state.entries.insert(vendor, status.clone());
            status
        };
        match super::probe_lifecycle::current() {
            Some(control) => control
                .publish(commit)
                .unwrap_or_else(|_| interrupted_login_check(vendor)),
            None => commit(),
        }
    }
    /// Refresh every enabled vendor concurrently.
    pub async fn refresh_all(&self, config: &CliAgentsConfig, force: bool) -> Vec<VendorStatus> {
        futures_util::future::join_all(
            Vendor::ALL
                .into_iter()
                .map(|vendor| self.refresh(vendor, config, force)),
        )
        .await;
        // A fast probe's returned value can be superseded while another
        // vendor is still pending. Publish the current coherent cache.
        self.snapshot_statuses(config)
    }
    /// Doctor-style map `{vendor: {...}}` for the Accounts page.
    pub async fn status_json(&self, config: &CliAgentsConfig, force: bool) -> Value {
        statuses_json(&self.refresh_all(config, force).await)
    }
    /// Picker rows for every enabled vendor: one row per discovered model,
    /// or a single Default/Sign in/Setup required row when none are known.
    pub async fn picker_rows(&self, config: &CliAgentsConfig, force: bool) -> Vec<PickerTarget> {
        picker_rows_from(&self.refresh_all(config, force).await, config)
    }
}

fn login_probe_stopped() -> bool {
    super::probe_lifecycle::current().is_some_and(|control| control.is_stopped())
}

fn interrupted_login_check(vendor: Vendor) -> VendorStatus {
    let mut status = VendorStatus::unchecked(vendor, None);
    status.detail = "Account check cancelled or timed out; sign-in state is unconfirmed".into();
    status.error = Some(status.detail.clone());
    status
}

fn changed_configuration(vendor: Vendor) -> VendorStatus {
    let mut status = VendorStatus::unchecked(vendor, None);
    status.detail = "Runtime configuration changed; refresh to check the current runtime".into();
    status
}
fn statuses_json(statuses: &[VendorStatus]) -> Value {
    Value::Object(
        statuses
            .iter()
            .map(|status| (status.vendor.id().to_owned(), status.to_doctor_json()))
            .collect(),
    )
}
fn picker_rows_from(statuses: &[VendorStatus], config: &CliAgentsConfig) -> Vec<PickerTarget> {
    let now = crate::now();
    let mut rows = Vec::new();
    for status in statuses {
        let vendor = status.vendor;
        if !config.vendor_enabled(vendor) {
            continue;
        }
        let ready = status.availability == Availability::Ready;
        let api_key = status.api_key_login();
        if ready && !status.models.is_empty() {
            for model in &status.models {
                let usage = status.usage_for(&model.id, now);
                // A pool at its plan limit cannot take a turn; the row
                // stays visible with the reason.
                let (availability, reason) = if usage.limit_reached {
                    (Availability::Unavailable, usage.label.clone())
                } else {
                    (Availability::Ready, status.detail.clone())
                };
                let mut row = picker::vendor_target(
                    vendor,
                    &model.id,
                    &model.label,
                    availability,
                    &reason,
                    usage,
                    status.accepts_images && model.vision,
                );
                row.is_default = model.is_default;
                row.reasoning = model.effort;
                row.billing = Some(status.billing().into());
                if api_key {
                    row.subtitle = picker::API_KEY_SUBTITLE.into();
                } else if status.billing_unverified() {
                    row.subtitle = picker::UNVERIFIED_BILLING_SUBTITLE.into();
                }
                rows.push(row);
            }
        } else {
            let usage = if ready {
                status.usage_for("default", now)
            } else {
                UsageSnapshot::unavailable_because(&vendor.provider(), &status.detail)
            };
            let mut row = picker::vendor_target(
                vendor,
                "default",
                "Default",
                status.availability,
                &status.detail,
                usage,
                status.accepts_images,
            );
            row.billing = Some(status.billing().into());
            if api_key {
                row.subtitle = picker::API_KEY_SUBTITLE.into();
            } else if status.billing_unverified() {
                row.subtitle = picker::UNVERIFIED_BILLING_SUBTITLE.into();
            }
            rows.push(row);
        }
    }
    rows
}

fn version_of(binary: &Path) -> impl std::future::Future<Output = Option<String>> + '_ {
    doctor::version(binary, None)
}

async fn probe_vendor(vendor: Vendor, config: &CliAgentsConfig, now: f64) -> VendorStatus {
    if !config.vendor_enabled(vendor) {
        return VendorStatus::disabled(vendor, now);
    }
    let configured = config.binary(vendor);
    let binary = if vendor == Vendor::Antigravity {
        super::antigravity_server::installation(configured).map(|i| i.server)
    } else {
        resolve_binary(configured)
    };
    let Some(binary) = binary else {
        return VendorStatus::setup_required(vendor, configured, now);
    };
    // The Antigravity server is ~1 GB and has no cheap `--version`; its
    // version comes from the ACP handshake.
    let version = if vendor == Vendor::Antigravity {
        None
    } else {
        version_of(&binary).await
    };
    if login_probe_stopped() {
        return interrupted_login_check(vendor);
    }
    let mut status = VendorStatus {
        vendor,
        availability: Availability::Unavailable,
        detail: String::new(),
        version,
        binary: Some(binary.clone()),
        binary_stamp: binary_stamp(vendor, config),
        account: None,
        models: Vec::new(),
        accepts_images: false,
        asks_approval: vendor.asks_approval(),
        effort_flag: None,
        fetched_at: now,
        error: None,
        usage_raw: None,
        usage_at: 0.0,
        usage_note: None,
        failures: 0,
        next_allowed: now,
    };
    match vendor {
        Vendor::Codex => probe_codex(&binary, &mut status).await,
        Vendor::Claude => probe_claude(&binary, &mut status).await,
        Vendor::Cursor => probe_acp_vendor(&binary, &["acp"], &mut status).await,
        Vendor::Grok => probe_acp_vendor(&binary, &["agent", "stdio"], &mut status).await,
        Vendor::Antigravity => {
            let args = super::antigravity_server::launch_args();
            let args: Vec<&str> = args.iter().map(String::as_str).collect();
            probe_acp_vendor(&binary, &args, &mut status).await
        }
    }
    status
}

fn ready_detail(status: &VendorStatus) -> String {
    let version = status.version.as_deref().unwrap_or("version unknown");
    let mut parts = vec![format!("Ready · {version}")];
    if status.api_key_login() {
        parts.push("API key login · billed per token".into());
    } else if status.billing_unverified() {
        parts.push(if status.vendor == Vendor::Codex {
            "Billing unverified · API charges may apply".into()
        } else {
            "Billing not reported by this CLI".into()
        });
    }
    if let Some(account) = &status.account {
        if let Some(plan) = &account.plan {
            parts.push(format!("plan {plan}"));
        }
        if let Some(email) = &account.email {
            parts.push(email.clone());
        }
    }
    parts.join(" · ")
}

async fn probe_codex(binary: &Path, status: &mut VendorStatus) {
    match codex_probe::probe(binary, None, PROBE_TIMEOUT).await {
        Ok(probe) => {
            status.accepts_images = true; // documented `localImage` input
            status.models = codex_probe::models_from_list(&probe.models)
                .into_iter()
                .map(|m| VendorModel {
                    id: m.id,
                    label: m.label,
                    is_default: m.is_default,
                    vision: m.vision,
                    effort: None,
                })
                .collect();
            if probe.logged_in() {
                status.account = Some(AccountInfo {
                    email: probe.email(),
                    plan: probe.plan_type(),
                    auth_mode: Some(probe.auth_mode()),
                });
                if probe.subscription_login() {
                    status.usage_raw = probe.rate_limits.clone();
                    if status.usage_raw.is_none() {
                        status.usage_note =
                            Some("Codex did not report rate limits for this login".into());
                    }
                } else if status.api_key_login() {
                    status.usage_note = Some(
                        "Signed in with an API key: usage is billed per token, not a plan allowance".into(),
                    );
                } else {
                    status.usage_note = Some(picker::UNVERIFIED_BILLING_DETAIL.into());
                }
                status.availability = Availability::Ready;
                status.detail = ready_detail(status);
            } else {
                status.availability = Availability::SignIn;
                status.detail = "Installed, not signed in".to_owned();
            }
            for (method, error) in probe.errors {
                status.detail.push_str(&format!(" · {method}: {error}"));
            }
        }
        Err(error) => {
            // Fall back to the documented login status command; models and
            // usage stay unknown rather than guessed.
            let state = doctor::codex_login_status(binary, None).await;
            status.error = Some(format!("app-server probe failed: {error}"));
            status.usage_note = Some("Codex app-server did not answer; usage not refreshed".into());
            match state {
                doctor::LoginState::LoggedIn => {
                    status.availability = Availability::Ready;
                    status.usage_note = Some(picker::UNVERIFIED_BILLING_DETAIL.into());
                    status.detail =
                        format!("{} (app-server unavailable: {error})", ready_detail(status));
                }
                doctor::LoginState::NotLoggedIn => {
                    status.availability = Availability::SignIn;
                    status.detail = "Installed, not signed in".into();
                }
                doctor::LoginState::Unknown => {
                    status.availability = Availability::Unavailable;
                    status.detail = format!("Codex did not respond: {error}");
                }
            }
        }
    }
}

/// Why Claude rows show no numbers until a task ran.
const CLAUDE_USAGE_NOTE: &str =
    "Claude Code reports plan usage while it runs a task; the figures appear after the next Claude Code task";

async fn probe_claude(binary: &Path, status: &mut VendorStatus) {
    status.accepts_images = true; // documented image source blocks
    status.usage_note = Some(CLAUDE_USAGE_NOTE.into());
    let (state, auth_method) = doctor::claude_auth_status(binary, None).await;
    match state {
        doctor::LoginState::LoggedIn => {
            status.availability = Availability::Ready;
            status.account = Some(AccountInfo {
                email: None,
                plan: None,
                auth_mode: auth_method,
            });
            if status.api_key_login() {
                status.usage_note = Some(
                    "Signed in with an API key: usage is billed per token, not a plan allowance"
                        .into(),
                );
            }
            let help = doctor::help_text(binary, None).await.unwrap_or_default();
            status.effort_flag = Some(help.contains("--effort"));
            match super::claude_probe::probe(binary, None, PROBE_TIMEOUT).await {
                Ok(probe) if !probe.models.is_empty() => {
                    status.models = claude_models(&probe);
                    if let Some(account) = status.account.as_mut() {
                        account.email = probe.email;
                        account.plan = probe.subscription;
                    }
                }
                // Older Claude Code without the SDK initialize request.
                _ => status.models = claude_alias_models(&help),
            }
            status.detail = ready_detail(status);
        }
        doctor::LoginState::NotLoggedIn => {
            status.availability = Availability::SignIn;
            status.detail = "Installed, not signed in".to_owned();
        }
        doctor::LoginState::Unknown => {
            status.availability = Availability::Unavailable;
            status.error = Some("`claude auth status` did not report a login state".into());
            status.detail = "Claude Code did not report its login state".into();
        }
    }
}

/// The rows of Claude Code's own model picker, from the SDK `initialize`
/// answer. `default` always comes first.
fn claude_models(probe: &super::claude_probe::ClaudeProbe) -> Vec<VendorModel> {
    let mut models: Vec<VendorModel> = probe
        .models
        .iter()
        .map(|m| VendorModel {
            id: m.id.clone(),
            label: if m.is_default {
                "Default".into()
            } else {
                m.label.clone()
            },
            is_default: m.is_default,
            vision: true,
            effort: Some(m.effort),
        })
        .collect();
    if !models.iter().any(|m| m.is_default) {
        models.insert(
            0,
            VendorModel {
                id: "default".into(),
                label: "Default".into(),
                is_default: true,
                vision: true,
                effort: None,
            },
        );
    }
    models
}

/// Older Claude Code has no model list: its own default plus the aliases
/// the installed CLI documents in `--help` for `--model`.
fn claude_alias_models(help: &str) -> Vec<VendorModel> {
    let mut models = vec![VendorModel {
        id: "default".into(),
        label: "Default".into(),
        is_default: true,
        vision: true,
        effort: None,
    }];
    for alias in ["fable", "opus", "sonnet", "haiku"] {
        if help.contains(&format!("'{alias}'")) {
            models.push(VendorModel {
                id: alias.into(),
                label: format!("{} (alias)", capitalize(alias)),
                is_default: false,
                vision: true,
                effort: None,
            });
        }
    }
    models
}

fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

async fn probe_acp_vendor(binary: &Path, args: &[&str], status: &mut VendorStatus) {
    let vendor = status.vendor;
    let workspace = std::env::temp_dir();
    status.usage_note = Some(match vendor {
        Vendor::Cursor => "Cursor reports the plan tier only, not remaining allowance".into(),
        Vendor::Antigravity => {
            "Antigravity's agent server reports no plan usage to other apps".into()
        }
        _ => "Grok reports per-session tokens only, not plan allowance".into(),
    });
    let antigravity = vendor == Vendor::Antigravity;
    match acp_probe::probe_vendor(binary, args, &workspace, None, PROBE_TIMEOUT, antigravity).await
    {
        Ok(probe) => {
            if antigravity && status.version.is_none() {
                status.version = probe.agent_version.clone();
            }
            status.accepts_images = probe.accepts_images;
            status.models = probe
                .models
                .iter()
                .map(|m| VendorModel {
                    id: m.id.clone(),
                    label: m.label.clone(),
                    is_default: m.current || (m.id == "auto" && probe.current_model.is_none()),
                    vision: probe.accepts_images,
                    // The session's effort option covers low, medium, high.
                    effort: Some(
                        ["low", "medium", "high"]
                            .iter()
                            .all(|level| probe.effort_levels.iter().any(|l| l == level)),
                    ),
                })
                .collect();
            if status.models.iter().all(|m| !m.is_default) {
                if let Some(first) = status.models.first_mut() {
                    first.is_default = true;
                }
            }
            let login_error = probe
                .session_error
                .as_deref()
                .or(match probe.authenticated {
                    Some(false) => Some("authenticate rejected"),
                    _ => None,
                });
            if probe.session_started && probe.authenticated != Some(false) {
                status.availability = Availability::Ready;
                status.account = Some(AccountInfo {
                    email: None,
                    plan: None,
                    auth_mode: probe.authenticated_method.clone(),
                });
                status.detail = ready_detail(status);
                if let Some(email) = cursor_email(vendor, binary).await {
                    if let Some(account) = status.account.as_mut() {
                        account.email = Some(email);
                    }
                    status.detail = ready_detail(status);
                }
                if let Some(tier) = cursor_tier(vendor, binary).await {
                    if let Some(account) = status.account.as_mut() {
                        account.plan = Some(tier.clone());
                    }
                    status.usage_note = Some(format!(
                        "Cursor reports the plan tier ({tier}) but not the remaining allowance"
                    ));
                    status.detail = ready_detail(status);
                }
            } else if let Some(error) = login_error {
                let lower = error.to_ascii_lowercase();
                if lower.contains("login") || lower.contains("auth") || lower.contains("sign") {
                    status.availability = Availability::SignIn;
                    status.detail = format!("Installed, not signed in ({error})");
                } else {
                    status.availability = Availability::Unavailable;
                    status.error = Some(error.to_owned());
                    status.detail = format!(
                        "{} could not open a session: {error}",
                        vendor.product_label()
                    );
                }
            } else {
                status.availability = Availability::Unavailable;
                status.error = Some("no session".into());
                status.detail = format!("{} did not open a session", vendor.product_label());
            }
        }
        Err(error) => {
            let incompatible = error.is::<acp_probe::UnsupportedProtocolVersion>();
            status.error = Some(error.to_string());
            status.availability = Availability::Unavailable;
            status.detail = if incompatible {
                format!(
                    "{} runtime is incompatible: {error}",
                    vendor.product_label()
                )
            } else {
                format!("{} did not respond: {error}", vendor.product_label())
            };
            if vendor == Vendor::Grok && !incompatible {
                // Cheaper documented fallback for ordinary probe failures only.
                // A model list cannot override observed protocol incompatibility.
                if let Some((logged_in, models)) = doctor::grok_models(binary, None).await {
                    status.models = models
                        .into_iter()
                        .map(|m| VendorModel {
                            id: m.id,
                            label: m.label,
                            is_default: m.current,
                            vision: false,
                            effort: None,
                        })
                        .collect();
                    status.availability = if logged_in {
                        Availability::Ready
                    } else {
                        Availability::SignIn
                    };
                    status.detail = if logged_in {
                        ready_detail(status)
                    } else {
                        "Installed, not signed in".into()
                    };
                    status.error = None;
                }
            }
        }
    }
}

/// `cursor-agent about` prints "Subscription Tier  <tier>"; display only,
/// never turned into a remaining-allowance number.
async fn cursor_tier(vendor: Vendor, binary: &Path) -> Option<String> {
    if vendor != Vendor::Cursor {
        return None;
    }
    let text = doctor::short_text(binary, &["about"], None).await?;
    parse_cursor_tier(&text)
}

pub(crate) fn parse_cursor_tier(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("Subscription Tier"))
        .map(|rest| rest.trim().to_owned())
        .filter(|tier| !tier.is_empty() && tier.len() < 40)
}

/// `cursor-agent status` prints the signed-in email; used for display only.
async fn cursor_email(vendor: Vendor, binary: &Path) -> Option<String> {
    if vendor != Vendor::Cursor {
        return None;
    }
    let text = doctor::short_text(binary, &["status"], None).await?;
    text.lines()
        .find_map(|line| line.split("Logged in as").nth(1))
        .map(|rest| rest.trim().trim_end_matches('.').to_owned())
        .filter(|s| s.contains('@'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_billing_is_unreported_without_losing_provider_usage_notes() {
        for (vendor, method, note) in [
            (
                Vendor::Cursor,
                "cursor_login",
                "Cursor reports a plan tier only",
            ),
            (
                Vendor::Antigravity,
                "oauth-personal",
                "Antigravity reports no plan usage",
            ),
            (
                Vendor::Grok,
                "cached_token",
                "Grok reports session tokens only",
            ),
        ] {
            let mut status = VendorStatus::unchecked(vendor, None);
            status.availability = Availability::Ready;
            status.fetched_at = crate::now();
            status.account = Some(AccountInfo {
                email: None,
                plan: Some("provider-reported tier".into()),
                auth_mode: Some(method.into()),
            });
            status.usage_note = Some(note.into());
            assert_eq!(status.billing(), "unknown");
            assert!(!status.api_key_login());
            let usage = status.usage_for("default", crate::now());
            assert_eq!(usage.detail, vec![note.to_owned()]);
            assert!(usage.remaining_percent.is_none());
            assert!(ready_detail(&status).contains("Billing not reported by this CLI"));
        }
    }

    #[test]
    fn known_codex_and_claude_billing_observations_remain_distinct() {
        for (vendor, method, billing) in [
            (Vendor::Codex, "chatgpt", "subscription"),
            (Vendor::Codex, "apiKey", "api_key"),
            (Vendor::Claude, "claude.ai", "subscription"),
            (Vendor::Claude, "api_key", "api_key"),
        ] {
            let mut status = VendorStatus::unchecked(vendor, None);
            status.account = Some(AccountInfo {
                email: None,
                plan: None,
                auth_mode: Some(method.into()),
            });
            assert_eq!(status.billing(), billing);
        }
    }

    #[cfg(target_os = "linux")]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_login_waiting_to_publish_cannot_save_ready_or_usage() {
        for expires_while_waiting in [false, true] {
            use super::super::probe_lifecycle::{self, PublicationGate};
            use std::os::unix::fs::PermissionsExt;
            use tokio_util::sync::CancellationToken;
            let root = tempfile::tempdir().unwrap();
            let binary = root.path().join("codex");
            fs::write(&binary, r#"#!/usr/bin/env python3
import json, os, pathlib, sys, time
root=pathlib.Path(__file__).parent
if sys.argv[1:] == ['--version']:
    print('codex-cli fixture'); sys.exit(0)
for line in sys.stdin:
    m=json.loads(line)
    if 'id' not in m: continue
    method=m.get('method')
    if method=='initialize': result={}
    elif method=='account/read': result={'account':{'type':'chatgpt','email':'fixture@example.invalid','planType':'pro'}}
    elif method=='account/rateLimits/read': result={'rateLimits':{'primary':{'usedPercent':20}}}
    elif method=='model/list':
        (root/'probe-ready').write_text(str(os.getpid()))
        until=time.monotonic()+8
        while not (root/'release-probe').exists() and time.monotonic()<until: time.sleep(.005)
        result={'data':[{'id':'fixture-model','displayName':'Fixture model'}]}
    else: result={}
    print(json.dumps({'id':m['id'],'result':result}),flush=True)
"#).unwrap();
            fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
            let config = CliAgentsConfig {
                codex_binary: binary.display().to_string(),
                ..Default::default()
            };
            let store = Arc::new(Store::open(&root.path().join("state.sqlite")).unwrap());
            let (events, _) = broadcast::channel(16);
            let catalog = Arc::new(VendorCatalog::with_store(store.clone(), events));
            catalog.configure(&config, false);
            let (sender, receiver) = std::sync::mpsc::channel();
            *catalog.publication_signal.lock().unwrap() = Some(sender);
            let cancel = CancellationToken::new();
            let gate = PublicationGate::default();
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            let worker = {
                let catalog = catalog.clone();
                let cancel = cancel.clone();
                let gate = gate.clone();
                tokio::spawn(async move {
                    probe_lifecycle::scope_with_gate(
                        cancel,
                        deadline,
                        gate,
                        catalog.refresh(Vendor::Codex, &config, true),
                    )
                    .await
                })
            };
            let probe_ready = tokio::time::timeout(Duration::from_secs(2), async {
                while !root.path().join("probe-ready").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .is_ok();
            let passed_prelock_check = if probe_ready {
                let state = catalog.state.lock().unwrap();
                fs::write(root.path().join("release-probe"), b"").unwrap();
                // Exact test-only signal: the real probe finished and passed the
                // stopped check. Its cache commit now waits on the mutex we own.
                let reached = receiver.recv_timeout(Duration::from_secs(2)).is_ok();
                if expires_while_waiting && reached {
                    std::thread::sleep(
                        deadline.saturating_duration_since(tokio::time::Instant::now())
                            + Duration::from_millis(1),
                    );
                } else {
                    gate.cancel(&cancel);
                }
                drop(state);
                reached
            } else {
                gate.cancel(&cancel);
                false
            };
            fs::write(root.path().join("release-probe"), b"").unwrap();
            let result = tokio::time::timeout(Duration::from_secs(2), worker)
                .await
                .unwrap()
                .unwrap();
            assert!(
                probe_ready && passed_prelock_check,
                "actual probe reached pre-publication barrier"
            );
            let cached = catalog.cached(Vendor::Codex).await;
            let usage_rows = store.usage_snapshots().unwrap();
            eprintln!(
                "{}",
                json!({"expired_while_waiting":expires_while_waiting,"publication_result":result.availability,
                "cached_availability":cached.as_ref().map(|s|s.availability),"persisted_usage_rows":usage_rows.len()})
            );
            assert_eq!(result.availability, Availability::Unavailable);
            assert!(cached.is_none(), "cancelled check saved Ready");
            assert!(usage_rows.is_empty(), "cancelled check persisted quota");
        }
    }

    #[test]
    fn usage_for_codex_rows_uses_pools_and_others_stay_unavailable() {
        let now = crate::now();
        let mut status = VendorStatus::setup_required(Vendor::Codex, "codex", now);
        status.availability = Availability::Ready;
        status.models = vec![
            VendorModel {
                id: "gpt-6-astra".into(),
                label: "GPT-6-Astra".into(),
                is_default: true,
                vision: true,
                effort: None,
            },
            VendorModel {
                id: "gpt-5.6-luna".into(),
                label: "GPT-5.6-Luna".into(),
                is_default: false,
                vision: true,
                effort: None,
            },
        ];
        status.usage_raw = Some(json!({
            "rateLimits": {"limitId":"codex","primary":{"usedPercent":40,"windowDurationMins":10080},"planType":"pro"},
            "rateLimitsByLimitId": {
                "codex": {"limitId":"codex","primary":{"usedPercent":40,"windowDurationMins":10080},"planType":"pro"},
                "base_model_inference": {"limitId":"base_model_inference","limitName":"gpt-reserve","normalModelSlug":"gpt-5.6-luna","primary":{"usedPercent":5,"windowDurationMins":10080}}
            }
        }));
        // Usage numbers alone do not establish that the current login uses a
        // subscription. This fixture models a successful structured probe.
        assert_eq!(status.to_doctor_json()["billing"], "unknown");
        assert!(status.usage_for("default", now).remaining_percent.is_none());
        status.account = Some(AccountInfo {
            email: None,
            plan: Some("pro".into()),
            auth_mode: Some("chatgpt".into()),
        });
        assert_eq!(
            status.usage_for("default", now).remaining_percent,
            Some(60.0)
        );
        assert_eq!(
            status.usage_for("gpt-5.6-luna", now).remaining_percent,
            Some(95.0)
        );
        assert!(status.usage_for("gpt-6-astra", now).pool_shared);
        status.account.as_mut().unwrap().auth_mode = Some("unrecognized-mode".into());
        assert_eq!(status.to_doctor_json()["billing"], "unknown");
        assert!(status
            .usage_for("gpt-5.6-luna", now)
            .remaining_percent
            .is_none());
        let cursor = VendorStatus::setup_required(Vendor::Cursor, "cursor-agent", now);
        assert_eq!(cursor.usage_for("auto", now).state, "unavailable");
        let doctor = cursor.to_doctor_json();
        assert_eq!(doctor["state"], "not_installed");
        assert_eq!(doctor["availability_label"], "Setup required");
    }

    #[test]
    fn cursor_tier_is_read_from_about() {
        let about = "About Cursor CLI\n\nCLI Version         2026.09.15\nModel               Auto\nSubscription Tier   Free\nOS                  linux (x64)\n";
        assert_eq!(super::parse_cursor_tier(about).as_deref(), Some("Free"));
        assert_eq!(super::parse_cursor_tier("no tier here"), None);
    }

    #[tokio::test]
    async fn a_rate_limit_push_over_unexpected_stored_usage_does_not_panic() {
        let catalog = VendorCatalog::new();
        for stored in [json!("rate limits unavailable"), json!([1, 2]), json!(7)] {
            let mut status = VendorStatus::unchecked(Vendor::Codex, None);
            status.usage_raw = Some(stored);
            catalog.state().entries.insert(Vendor::Codex, status);
            let snapshot =
                json!({"limitId":"codex","primary":{"usedPercent":55.0,"windowDurationMins":300}});
            catalog
                .apply_rate_limits(Vendor::Codex, &snapshot, "gpt-6-astra")
                .await;
            let raw = catalog.state().entries[&Vendor::Codex]
                .usage_raw
                .clone()
                .unwrap();
            assert_eq!(raw["rateLimits"], snapshot);
            assert_eq!(raw["rateLimitsByLimitId"]["codex"], snapshot);
        }
    }

    #[test]
    fn a_panic_while_the_catalog_is_held_does_not_disable_it() {
        let catalog = std::sync::Arc::new(VendorCatalog::new());
        let held = catalog.clone();
        let _ = std::thread::spawn(move || {
            let _state = held.state.lock().unwrap();
            panic!("a bug while the catalog was held");
        })
        .join();
        assert!(catalog.state.is_poisoned());
        catalog.configure(&CliAgentsConfig::default(), false);
        assert!(!catalog.state.is_poisoned());
    }
}
