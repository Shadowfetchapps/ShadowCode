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
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio::sync::{broadcast, Mutex};

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
    /// Codex's boolean login-status fallback establishes authentication, not
    /// billing. Only the structured account types establish these two routes;
    /// an absent or newer, unrecognized type cannot imply a subscription.
    fn billing_unverified(&self) -> bool {
        self.vendor == Vendor::Codex
            && !matches!(
                self.account
                    .as_ref()
                    .and_then(|account| account.auth_mode.as_deref()),
                Some("chatgpt" | "apiKey")
            )
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
        if self.billing_unverified() && self.fetched_at > 0.0 {
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

#[derive(Default)]
pub struct VendorCatalog {
    entries: Mutex<HashMap<Vendor, VendorStatus>>,
    /// Last persisted usage per vendor (loaded at startup).
    persisted: Mutex<HashMap<Vendor, UsageRow>>,
    store: Option<Arc<Store>>,
    events: Option<broadcast::Sender<Value>>,
    logins: auth::Logins,
    /// Offline mode: no vendor process is started for status, models or
    /// usage; rows keep what is already known and read "Offline".
    offline: std::sync::atomic::AtomicBool,
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
                // Rows are newest first; keep the newest per vendor.
                persisted.entry(vendor).or_insert(row);
            }
        }
        Self {
            persisted: Mutex::new(persisted),
            store: Some(store),
            events: Some(events),
            ..Default::default()
        }
    }

    /// Follow the configured network mode (set whenever config is read).
    pub fn set_offline(&self, offline: bool) {
        self.offline
            .store(offline, std::sync::atomic::Ordering::Relaxed);
    }
    pub fn is_offline(&self) -> bool {
        self.offline.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn logins(&self) -> &auth::Logins {
        &self.logins
    }

    /// Broadcast a non-session event (`account.login`, `usage.updated`).
    /// The broadcast is a wakeup; login lines are also kept in `logins()`.
    pub fn broadcast(&self, kind: &str, payload: Value) {
        if let Some(events) = &self.events {
            let _ = events.send(json!({
                "type": kind,
                "ts": crate::now(),
                "session_id": null,
                "task_id": null,
                "payload": payload,
            }));
        }
    }

    /// Forget the cached status of a vendor so the next refresh re-probes
    /// (after a sign-in). Persisted usage is kept.
    pub async fn clear(&self, vendor: Vendor) {
        self.entries.lock().await.remove(&vendor);
    }
    pub async fn forget_status(&self, vendor: Vendor) {
        self.clear(vendor).await;
    }

    /// Forget everything tied to the vendor login (disconnect): cached
    /// status, persisted usage, and native session ids of conversations.
    pub async fn forget(&self, vendor: Vendor) -> anyhow::Result<()> {
        self.entries.lock().await.remove(&vendor);
        self.persisted.lock().await.remove(&vendor);
        if let Some(store) = &self.store {
            store.delete_usage_snapshots(vendor.id())?;
            store.clear_session_meta_prefix(&crate::store::keys::native_session(vendor.id()))?;
        }
        Ok(())
    }

    /// Merge a rate-limit snapshot pushed during a Codex turn
    /// (`account/rateLimits/updated`) and return the row usage for `model`.
    pub async fn apply_rate_limits(
        &self,
        vendor: Vendor,
        snapshot: &Value,
        model: &str,
    ) -> UsageSnapshot {
        let now = crate::now();
        let mut entries = self.entries.lock().await;
        let Some(entry) = entries.get_mut(&vendor) else {
            return UsageSnapshot::from_codex(&json!({"rateLimits": snapshot}), Some(model), now);
        };
        let raw = entry.usage_raw.get_or_insert_with(|| json!({}));
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
        let payload = raw.clone();
        entry.usage_at = now;
        let usage = entry.usage_for(model, now);
        let row = UsageRow {
            vendor: vendor.id().into(),
            account: entry.account_key(),
            pool: PERSISTED_POOL.into(),
            fetched_at: now,
            payload,
        };
        drop(entries);
        self.persist(vendor, row).await;
        usage
    }

    /// Usage to report with `limit.reached`: the cached numbers when known.
    pub async fn limit_usage(&self, vendor: Vendor, model: &str, detail: &str) -> UsageSnapshot {
        match self.entries.lock().await.get(&vendor) {
            Some(entry) if entry.usage_raw.is_some() => entry
                .usage_for(model, crate::now())
                .with_limit_reached(detail),
            _ => UsageSnapshot::limit_reached(&vendor.provider(), detail),
        }
    }

    async fn persist(&self, vendor: Vendor, row: UsageRow) {
        let mut persisted = self.persisted.lock().await;
        if let Some(store) = &self.store {
            // A different account on the same CLI: its old numbers are not
            // this account's usage.
            if persisted
                .get(&vendor)
                .is_some_and(|old| old.account != row.account)
            {
                let _ = store.delete_usage_snapshots(vendor.id());
            }
            let _ = store.upsert_usage_snapshot(&row);
        }
        persisted.insert(vendor, row);
    }

    async fn drop_persisted(&self, vendor: Vendor) {
        self.persisted.lock().await.remove(&vendor);
        if let Some(store) = &self.store {
            let _ = store.delete_usage_snapshots(vendor.id());
        }
    }

    /// Accounts JSON from what is already known, without probing: cached
    /// status, or persisted usage marked "Last checked …".
    pub async fn status_cached_json(&self) -> Value {
        let entries = self.entries.lock().await;
        let persisted = self.persisted.lock().await;
        let mut map = serde_json::Map::new();
        for vendor in Vendor::ALL {
            let status = entries
                .get(&vendor)
                .cloned()
                .unwrap_or_else(|| VendorStatus::unchecked(vendor, persisted.get(&vendor)));
            map.insert(vendor.id().to_owned(), status.to_doctor_json());
        }
        Value::Object(map)
    }

    pub async fn cached(&self, vendor: Vendor) -> Option<VendorStatus> {
        self.entries.lock().await.get(&vendor).cloned()
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
        let now = crate::now();
        let previous = self.entries.lock().await.get(&vendor).cloned();
        if self.is_offline() {
            // No helper network activity offline: report what is known.
            let mut status = match previous {
                Some(status) => status,
                None => {
                    let persisted = self.persisted.lock().await;
                    VendorStatus::unchecked(vendor, persisted.get(&vendor))
                }
            };
            status.availability = Availability::Unavailable;
            status.detail =
                "Offline mode: cloud models need the network. Switch network mode to Online in Settings."
                    .into();
            return status;
        }
        if let Some(existing) = &previous {
            let fresh = now - existing.fetched_at < MIN_REFRESH_SECS;
            let backing_off = existing.next_allowed > now;
            let binary_changed = existing.binary_stamp != binary_stamp(vendor, config);
            if !binary_changed && (backing_off || (!force && fresh)) {
                return existing.clone();
            }
        }
        let mut status = probe_vendor(vendor, config, now).await;
        if status.usage_raw.is_some() {
            status.usage_at = now;
        }
        if status.error.is_some() {
            let failures = previous.as_ref().map(|p| p.failures + 1).unwrap_or(1);
            status.failures = failures;
            status.next_allowed =
                now + (30.0 * 2f64.powi(failures.min(7) as i32)).min(MAX_BACKOFF_SECS);
            // Keep the last known usage/models so stale data stays visible.
            if status.usage_raw.is_none() {
                if let Some(previous) = previous.as_ref().filter(|p| p.usage_raw.is_some()) {
                    status.usage_raw = previous.usage_raw.clone();
                    status.usage_at = previous.usage_at;
                } else if let Some(row) = self.persisted.lock().await.get(&vendor) {
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
            self.persist(vendor, row).await;
        } else if matches!(status.availability, Availability::SignIn) || status.api_key_login() {
            // Signed out (or switched to an API key): stored plan numbers no
            // longer describe this login.
            self.drop_persisted(vendor).await;
        }
        self.entries.lock().await.insert(vendor, status.clone());
        status
    }

    /// Refresh every enabled vendor concurrently.
    pub async fn refresh_all(&self, config: &CliAgentsConfig, force: bool) -> Vec<VendorStatus> {
        let futures = Vendor::ALL
            .into_iter()
            .map(|vendor| self.refresh(vendor, config, force));
        futures_util::future::join_all(futures).await
    }

    /// Doctor-style map `{vendor: {...}}` for the Accounts page.
    pub async fn status_json(&self, config: &CliAgentsConfig, force: bool) -> Value {
        let mut map = serde_json::Map::new();
        for status in self.refresh_all(config, force).await {
            map.insert(status.vendor.id().to_owned(), status.to_doctor_json());
        }
        Value::Object(map)
    }

    /// Picker rows for every enabled vendor: one row per discovered model,
    /// or a single Default/Sign in/Setup required row when none are known.
    pub async fn picker_rows(&self, config: &CliAgentsConfig, force: bool) -> Vec<PickerTarget> {
        let now = crate::now();
        let mut rows = Vec::new();
        for status in self.refresh_all(config, force).await {
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
        parts.push("Billing unverified · API charges may apply".into());
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

async fn probe_claude(binary: &Path, status: &mut VendorStatus) {
    status.accepts_images = true; // documented image source blocks
    status.usage_note = Some(
        "Claude Code does not expose plan usage to other apps; open claude.ai to see it".into(),
    );
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
            status.models = claude_models(binary).await;
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

/// Claude Code has no model-list command. The row set is its own default
/// plus the aliases the installed CLI documents in `--help` for `--model`.
async fn claude_models(binary: &Path) -> Vec<VendorModel> {
    let mut models = vec![VendorModel {
        id: "default".into(),
        label: "Default".into(),
        is_default: true,
        vision: true,
    }];
    let help = doctor::help_text(binary, None).await.unwrap_or_default();
    for alias in ["fable", "opus", "sonnet", "haiku"] {
        if help.contains(&format!("'{alias}'")) {
            models.push(VendorModel {
                id: alias.into(),
                label: format!("{} (alias)", capitalize(alias)),
                is_default: false,
                vision: true,
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
                    auth_mode: probe.auth_methods.first().cloned(),
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
            status.error = Some(error.to_string());
            status.availability = Availability::Unavailable;
            status.detail = format!("{} did not respond: {error}", vendor.product_label());
            if vendor == Vendor::Grok {
                // Cheaper documented fallback.
                if let Some((logged_in, models)) = doctor::grok_models(binary, None).await {
                    status.models = models
                        .into_iter()
                        .map(|m| VendorModel {
                            id: m.id,
                            label: m.label,
                            is_default: m.current,
                            vision: false,
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
            },
            VendorModel {
                id: "gpt-5.6-luna".into(),
                label: "GPT-5.6-Luna".into(),
                is_default: false,
                vision: true,
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
}
