//! Subscription usage snapshots from official runtime data.
//!
//! A snapshot describes what a provider actually reported: one or more limit
//! windows, the quota pool a model row belongs to, plan name, credits only
//! when the provider states them, and when the data was fetched. Unknown
//! stays unknown: no invented percents, message counts, or "unlimited".
//! Context-window usage and per-turn token counts are separate measurements
//! and never appear here.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const STATE_OK: &str = "ok";
pub const STATE_STALE: &str = "stale";
pub const STATE_UNAVAILABLE: &str = "unavailable";
pub const STATE_LOCAL: &str = "local";
pub const STATE_LIMIT_REACHED: &str = "limit_reached";

/// Snapshots older than this are shown as "Last checked …".
pub const STALE_AFTER_SECS: f64 = 30.0 * 60.0;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageWindow {
    /// Human label derived from the window length ("Weekly", "5-hour").
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
    pub window_minutes: Option<u64>,
    /// Unix seconds when the window resets, when reported.
    pub resets_at: Option<f64>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageCredits {
    pub has_credits: bool,
    pub unlimited: bool,
    pub balance: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct UsageSnapshot {
    pub state: String,
    /// One line for the picker row.
    pub label: String,
    /// Additional lines for the tooltip / expandable row.
    pub detail: Vec<String>,
    pub plan: Option<String>,
    /// Quota pool id this row draws from (e.g. `codex`, `gpt-reserve`).
    pub pool: Option<String>,
    /// True when the pool is shared by several models of the account.
    pub pool_shared: bool,
    pub windows: Vec<UsageWindow>,
    /// Primary window remaining percent, for compact displays.
    pub remaining_percent: Option<f64>,
    pub credits: Option<UsageCredits>,
    pub limit_reached: bool,
    /// Unix seconds of the last successful provider refresh.
    pub last_refresh: Option<f64>,
    pub provider_usage_url: Option<String>,
}

impl UsageSnapshot {
    fn base(state: &str, label: String, provider: &str) -> Self {
        Self {
            state: state.into(),
            label,
            detail: Vec::new(),
            plan: None,
            pool: None,
            pool_shared: false,
            windows: Vec::new(),
            remaining_percent: None,
            credits: None,
            limit_reached: false,
            last_refresh: None,
            provider_usage_url: usage_url(provider),
        }
    }
    /// The provider exposes no machine-readable allowance.
    pub fn unavailable(provider: &str) -> Self {
        let name = product_name(provider);
        Self::base(
            STATE_UNAVAILABLE,
            format!("Usage unavailable · Open {name} usage"),
            provider,
        )
    }
    /// Same as `unavailable`, with the reason kept for the tooltip.
    pub fn unavailable_because(provider: &str, reason: &str) -> Self {
        let mut snap = Self::unavailable(provider);
        if !reason.trim().is_empty() {
            snap.detail.push(reason.trim().to_owned());
        }
        snap
    }
    /// The CLI is signed in with an API key: every turn is billed per token
    /// by the provider. No plan allowance exists to show.
    pub fn api_key_login(provider: &str) -> Self {
        let mut snap = Self::base(
            STATE_UNAVAILABLE,
            "API key login · billed per token".into(),
            provider,
        );
        snap.detail.push(
            "This CLI is signed in with an API key, not a subscription; the provider bills each request.".into(),
        );
        snap
    }
    /// The runtime reported a plan limit but no numbers (or none are known).
    pub fn limit_reached(provider: &str, detail: &str) -> Self {
        let mut snap = Self::base(STATE_LIMIT_REACHED, "Plan limit reached".into(), provider);
        snap.limit_reached = true;
        if !detail.trim().is_empty() {
            snap.detail.push(detail.trim().to_owned());
        }
        snap
    }
    /// Mark a snapshot as having hit the plan limit, keeping any numbers.
    pub fn with_limit_reached(mut self, detail: &str) -> Self {
        self.state = STATE_LIMIT_REACHED.into();
        self.limit_reached = true;
        if !self.label.starts_with("Plan limit reached") {
            self.label = "Plan limit reached".into();
        }
        if !detail.trim().is_empty() {
            self.detail.insert(0, detail.trim().to_owned());
        }
        self
    }
    pub fn local() -> Self {
        Self::base(
            STATE_LOCAL,
            "Runs on this computer · No subscription quota".into(),
            "",
        )
    }
    /// A previously fetched snapshot whose refresh failed: keep the numbers,
    /// say when they were last checked.
    pub fn mark_stale(mut self, now: f64) -> Self {
        if self.state == STATE_OK || self.state == STATE_LIMIT_REACHED {
            self.state = STATE_STALE.into();
        }
        if let Some(fetched) = self.last_refresh {
            let age = format_age(fetched, now);
            self.detail.insert(0, format!("Last checked {age}"));
            if self.state == STATE_STALE {
                self.label = format!("{} · Last checked {age}", self.label);
            }
        }
        self
    }
    pub fn is_stale(&self, now: f64) -> bool {
        match self.last_refresh {
            Some(fetched) => now - fetched > STALE_AFTER_SECS,
            None => false,
        }
    }
    pub fn to_json(&self) -> Value {
        json!(self)
    }

    /// Map the Codex app-server `account/rateLimits/read` result (or an
    /// `account/rateLimits/updated` snapshot) to the pool that applies to
    /// `model`. `rateLimitsByLimitId` groups pools; a pool whose
    /// `normalModelSlug` matches the model wins, otherwise the default
    /// `rateLimits` snapshot (shared by the account's other models).
    pub fn from_codex(rate_limits: &Value, model: Option<&str>, now: f64) -> Self {
        let mut pool_snapshot = rate_limits.get("rateLimits").cloned();
        let mut pool_id = None;
        let mut dedicated = false;
        if let (Some(model), Some(pools)) = (model, rate_limits["rateLimitsByLimitId"].as_object())
        {
            for (id, snapshot) in pools {
                if snapshot["normalModelSlug"].as_str() == Some(model) {
                    pool_snapshot = Some(snapshot.clone());
                    pool_id = Some(
                        snapshot["limitName"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .unwrap_or(id)
                            .to_owned(),
                    );
                    dedicated = true;
                    break;
                }
            }
        }
        let Some(snapshot) = pool_snapshot.filter(|s| s.is_object()) else {
            let mut snap = Self::unavailable("cli:codex");
            snap.last_refresh = Some(now);
            snap.detail
                .push("Codex did not report rate limits for this account".into());
            return snap;
        };
        let pool_id = pool_id.or_else(|| {
            snapshot["limitName"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| snapshot["limitId"].as_str())
                .map(str::to_owned)
        });
        let mut windows = Vec::new();
        for key in ["primary", "secondary"] {
            let window = &snapshot[key];
            let Some(used) = window["usedPercent"].as_f64() else {
                continue;
            };
            let minutes = window["windowDurationMins"].as_u64();
            windows.push(UsageWindow {
                label: window_label(minutes),
                used_percent: used,
                remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                window_minutes: minutes,
                resets_at: window["resetsAt"].as_f64(),
            });
        }
        let credits = snapshot["credits"].as_object().map(|c| UsageCredits {
            has_credits: c
                .get("hasCredits")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            unlimited: c.get("unlimited").and_then(Value::as_bool).unwrap_or(false),
            balance: c.get("balance").and_then(Value::as_str).map(str::to_owned),
        });
        let plan = snapshot["planType"].as_str().map(str::to_owned);
        let limit_reached = snapshot["rateLimitReachedType"].as_str().is_some()
            || rate_limits["ordinaryUsageAllowed"].as_bool() == Some(false)
            || windows.first().is_some_and(|w| w.used_percent >= 100.0);
        if windows.is_empty() {
            let mut snap = Self::unavailable("cli:codex");
            snap.last_refresh = Some(now);
            snap.plan = plan;
            snap.detail
                .push("Codex reported no limit window for this pool".into());
            return snap;
        }
        let primary = &windows[0];
        let mut label = format!(
            "{:.0}% remaining · {}",
            primary.remaining_percent, primary.label
        );
        if !dedicated {
            label.push_str(" · Shared plan usage");
        }
        if limit_reached {
            label = format!("Plan limit reached · {}", primary.label);
        }
        let mut detail = Vec::new();
        if let Some(plan) = &plan {
            detail.push(format!("ChatGPT plan: {plan}"));
        }
        for window in &windows {
            let mut line = format!("{}: {:.0}% used", window.label, window.used_percent);
            if let Some(reset) = window.resets_at {
                line.push_str(&format!(" · resets in {}", format_until(reset, now)));
            }
            detail.push(line);
        }
        if let Some(pool) = &pool_id {
            detail.push(if dedicated {
                format!("Dedicated pool: {pool}")
            } else {
                format!("Shared pool: {pool} (all models on this account)")
            });
        }
        if let Some(credits) = &credits {
            if credits.has_credits || credits.unlimited {
                detail.push(match (&credits.balance, credits.unlimited) {
                    (_, true) => "Credits: unlimited (reported by Codex)".into(),
                    (Some(balance), false) => format!("Credits: {balance} (reported by Codex)"),
                    (None, false) => "Credits available (reported by Codex)".into(),
                });
            }
        }
        Self {
            state: if limit_reached {
                STATE_LIMIT_REACHED.into()
            } else {
                STATE_OK.into()
            },
            label,
            detail,
            plan,
            pool: pool_id,
            pool_shared: !dedicated,
            remaining_percent: Some(primary.remaining_percent),
            windows,
            credits,
            limit_reached,
            last_refresh: Some(now),
            provider_usage_url: usage_url("cli:codex"),
        }
    }
}

impl UsageSnapshot {
    /// A plan snapshot pushed during a turn, in the vendor's own shape.
    pub fn from_vendor(
        vendor: super::Vendor,
        snapshot: &Value,
        plan: Option<&str>,
        model: &str,
        now: f64,
    ) -> Self {
        match vendor {
            super::Vendor::Claude => Self::from_claude(snapshot, plan, now),
            _ => Self::from_codex(&json!({ "rateLimits": snapshot }), Some(model), now),
        }
    }

    /// Map Claude Code's `rate_limit_event.rate_limit_info` (stream-json).
    /// The CLI reads it from the claude.ai rate-limit response headers:
    /// `unifiedWindows` holds the 5-hour and weekly windows with
    /// `utilization` as a fraction; without it only the limiting window
    /// (`rateLimitType`, `utilization`) is known. `status` is `allowed`,
    /// `allowed_warning` or `rejected`. All models of the plan share it.
    pub fn from_claude(info: &Value, plan: Option<&str>, now: f64) -> Self {
        const PROVIDER: &str = "cli:claude";
        let window = |kind: &str, value: &Value| {
            let used = value["utilization"].as_f64().filter(|u| u.is_finite())? * 100.0;
            let used = used.max(0.0);
            Some(UsageWindow {
                label: claude_window_label(kind),
                used_percent: used,
                remaining_percent: (100.0 - used).clamp(0.0, 100.0),
                window_minutes: match kind {
                    "five_hour" => Some(300),
                    k if k.starts_with("seven_day") => Some(10080),
                    _ => None,
                },
                resets_at: value["resetsAt"].as_f64(),
            })
        };
        let mut windows: Vec<UsageWindow> = Vec::new();
        let mut kinds: Vec<&str> = Vec::new();
        if let Some(unified) = info["unifiedWindows"].as_object() {
            for (kind, value) in unified {
                if let Some(w) = window(kind, value) {
                    windows.push(w);
                    kinds.push(kind);
                }
            }
        }
        let limiting = info["rateLimitType"].as_str().unwrap_or("");
        if !limiting.is_empty() && !kinds.contains(&limiting) {
            if let Some(w) = window(limiting, info) {
                windows.push(w);
                kinds.push(limiting);
            }
        }
        // Shortest window first (5-hour, then weekly ones).
        let mut order: Vec<usize> = (0..windows.len()).collect();
        order.sort_by_key(|&i| (windows[i].window_minutes.unwrap_or(u64::MAX), kinds[i]));
        let windows: Vec<UsageWindow> = order.iter().map(|&i| windows[i].clone()).collect();
        let kinds: Vec<&str> = order.iter().map(|&i| kinds[i]).collect();
        let status = info["status"].as_str().unwrap_or("");
        let overage = info["isUsingOverage"] == true;
        let limit_reached = status == "rejected" && !overage;
        let plan = plan.map(str::to_owned);
        if windows.is_empty() {
            let mut snap = if limit_reached {
                Self::limit_reached(
                    PROVIDER,
                    "Claude Code reported that the plan limit is reached",
                )
            } else {
                let mut snap = Self::unavailable(PROVIDER);
                snap.detail
                    .push("Claude Code reported no usage figures for this plan".into());
                snap
            };
            snap.last_refresh = Some(now);
            snap.plan = plan;
            if let Some(reset) = info["resetsAt"].as_f64() {
                snap.detail
                    .push(format!("Resets in {}", format_until(reset, now)));
            }
            return snap;
        }
        // The limiting window when Claude names it, otherwise the fullest.
        let primary = kinds
            .iter()
            .position(|k| *k == limiting)
            .unwrap_or_else(|| {
                (0..windows.len())
                    .max_by(|&a, &b| windows[a].used_percent.total_cmp(&windows[b].used_percent))
                    .unwrap_or(0)
            });
        let head = &windows[primary];
        let label = if limit_reached {
            format!("Plan limit reached · {}", head.label)
        } else {
            format!("{:.0}% remaining · {}", head.remaining_percent, head.label)
        };
        let mut detail = Vec::new();
        if let Some(plan) = &plan {
            detail.push(format!("Claude plan: {plan}"));
        }
        for window in &windows {
            let mut line = format!("{}: {:.0}% used", window.label, window.used_percent);
            if let Some(reset) = window.resets_at {
                line.push_str(&format!(" · resets in {}", format_until(reset, now)));
            }
            detail.push(line);
        }
        if status == "allowed_warning" {
            detail.push("Claude Code warns that this plan is close to its limit".into());
        }
        if overage {
            detail.push("Using extra usage beyond the plan (reported by Claude Code)".into());
        }
        detail.push("Shared by all Claude models on this plan".into());
        Self {
            state: if limit_reached {
                STATE_LIMIT_REACHED.into()
            } else {
                STATE_OK.into()
            },
            label,
            detail,
            plan,
            pool: None,
            pool_shared: true,
            remaining_percent: Some(head.remaining_percent),
            windows,
            credits: None,
            limit_reached,
            last_refresh: Some(now),
            provider_usage_url: usage_url(PROVIDER),
        }
    }
}

/// Display name of a Claude plan window (`rateLimitType` and the keys of
/// `unifiedWindows`).
pub fn claude_window_label(kind: &str) -> String {
    match kind {
        "five_hour" => "5-hour".into(),
        "seven_day" => "Weekly".into(),
        "seven_day_opus" => "Weekly (Opus)".into(),
        "seven_day_sonnet" => "Weekly (Sonnet)".into(),
        "seven_day_overage_included" => "Weekly incl. extra usage".into(),
        "overage" => "Extra usage".into(),
        "" => "Plan".into(),
        other => other.replace('_', " "),
    }
}

fn product_name(provider: &str) -> &str {
    match provider {
        "cli:codex" | "Codex" => "Codex",
        "cli:claude" | "Claude Code" => "Claude",
        "cli:cursor" | "Cursor" => "Cursor",
        "cli:antigravity" | "Antigravity" => "Antigravity",
        "cli:grok" | "Grok" => "Grok",
        other => other,
    }
}

fn usage_url(provider: &str) -> Option<String> {
    Some(
        match provider {
            "Codex" | "cli:codex" => "https://chatgpt.com/#settings",
            "Claude Code" | "cli:claude" => "https://claude.ai/settings/usage",
            "Cursor" | "cli:cursor" => "https://cursor.com/dashboard",
            "Antigravity" | "cli:antigravity" => {
                "https://antigravity.google/docs/cli/commands/usage/" // plan usage is shown there
            }
            "Grok" | "cli:grok" => "https://grok.com/",
            _ => return None,
        }
        .into(),
    )
}

fn window_label(minutes: Option<u64>) -> String {
    match minutes {
        Some(m) if m % 10080 == 0 => {
            if m == 10080 {
                "Weekly".into()
            } else {
                format!("{}-week", m / 10080)
            }
        }
        Some(m) if m % 1440 == 0 => {
            if m == 1440 {
                "Daily".into()
            } else {
                format!("{}-day", m / 1440)
            }
        }
        Some(m) if m % 60 == 0 => format!("{}-hour", m / 60),
        Some(m) => format!("{m}-minute"),
        None => "Current window".into(),
    }
}

pub fn format_age(ts: f64, now: f64) -> String {
    let delta = (now - ts).max(0.0);
    if delta < 60.0 {
        "just now".into()
    } else if delta < 3600.0 {
        format!("{}m ago", (delta / 60.0) as u64)
    } else if delta < 86400.0 {
        format!("{}h ago", (delta / 3600.0) as u64)
    } else {
        format!("{}d ago", (delta / 86400.0) as u64)
    }
}

pub fn format_until(ts: f64, now: f64) -> String {
    let delta = (ts - now).max(0.0);
    if delta < 60.0 {
        "under a minute".into()
    } else if delta < 3600.0 {
        format!("{}m", (delta / 60.0) as u64)
    } else if delta < 86400.0 {
        let hours = (delta / 3600.0) as u64;
        let minutes = ((delta % 3600.0) / 60.0) as u64;
        format!("{hours}h {minutes}m")
    } else {
        let days = (delta / 86400.0) as u64;
        let hours = ((delta % 86400.0) / 3600.0) as u64;
        format!("{days}d {hours}h")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_fixture() -> Value {
        json!({
            "ordinaryUsageAllowed": true,
            "rateLimits": {
                "limitId": "codex", "limitName": null, "normalModelSlug": null,
                "primary": {"usedPercent": 98, "windowDurationMins": 10080, "resetsAt": 1790454596},
                "secondary": null,
                "credits": {"hasCredits": false, "unlimited": false, "balance": "0"},
                "planType": "pro", "rateLimitReachedType": null
            },
            "rateLimitsByLimitId": {
                "codex": {"limitId": "codex", "primary": {"usedPercent": 98, "windowDurationMins": 10080, "resetsAt": 1790454596}, "planType": "pro"},
                "base_model_inference": {"limitId": "base_model_inference", "limitName": "gpt-reserve", "normalModelSlug": "gpt-5.6-luna",
                    "primary": {"usedPercent": 0, "windowDurationMins": 10080, "resetsAt": 1790768732}, "planType": "pro"}
            }
        })
    }

    #[test]
    fn unknown_usage_is_not_a_percent() {
        let snap = UsageSnapshot::unavailable("cli:cursor");
        assert_eq!(snap.state, STATE_UNAVAILABLE);
        assert!(snap.remaining_percent.is_none());
        assert!(snap.windows.is_empty());
        assert!(snap
            .label
            .starts_with("Usage unavailable · Open Cursor usage"));
        assert!(!snap.label.contains('%'));
        assert!(!snap.label.to_lowercase().contains("unlimited"));
        let local = UsageSnapshot::local();
        assert_eq!(local.state, STATE_LOCAL);
        assert!(local.label.contains("No subscription quota"));
    }

    #[test]
    fn codex_shared_pool_and_dedicated_pool() {
        let now = 1790163932.0;
        let shared = UsageSnapshot::from_codex(&codex_fixture(), Some("gpt-6-astra"), now);
        assert_eq!(shared.state, STATE_OK);
        assert!(shared.pool_shared);
        assert_eq!(shared.remaining_percent, Some(2.0));
        assert_eq!(shared.windows[0].label, "Weekly");
        assert!(shared.label.contains("2% remaining"));
        assert!(shared.label.contains("Shared plan usage"));
        assert_eq!(shared.plan.as_deref(), Some("pro"));
        assert!(shared.detail.iter().any(|d| d.contains("resets in")));
        // Credits with hasCredits=false are not advertised.
        assert!(!shared.detail.iter().any(|d| d.contains("Credits")));
        let dedicated = UsageSnapshot::from_codex(&codex_fixture(), Some("gpt-5.6-luna"), now);
        assert!(!dedicated.pool_shared);
        assert_eq!(dedicated.pool.as_deref(), Some("gpt-reserve"));
        assert_eq!(dedicated.remaining_percent, Some(100.0));
        assert!(!dedicated.label.contains("Shared"));
    }

    #[test]
    fn codex_limit_reached_and_missing_data() {
        let now = 1790163932.0;
        let mut fixture = codex_fixture();
        fixture["rateLimits"]["rateLimitReachedType"] = json!("rate_limit_reached");
        let reached = UsageSnapshot::from_codex(&fixture, None, now);
        assert!(reached.limit_reached);
        assert_eq!(reached.state, STATE_LIMIT_REACHED);
        assert!(reached.label.starts_with("Plan limit reached"));
        let missing = UsageSnapshot::from_codex(&json!({"rateLimits": null}), None, now);
        assert_eq!(missing.state, STATE_UNAVAILABLE);
        assert!(missing.remaining_percent.is_none());
    }

    #[test]
    fn stale_snapshots_keep_numbers_and_say_when() {
        let now = 1790163932.0;
        let snap = UsageSnapshot::from_codex(&codex_fixture(), None, now - 7200.0);
        assert!(snap.is_stale(now));
        let stale = snap.mark_stale(now);
        assert_eq!(stale.state, STATE_STALE);
        assert_eq!(stale.remaining_percent, Some(2.0));
        assert!(stale.label.contains("Last checked 2h ago"));
        assert_eq!(window_label(Some(300)), "5-hour");
        assert_eq!(window_label(Some(1440)), "Daily");
        assert_eq!(window_label(None), "Current window");
    }
}
