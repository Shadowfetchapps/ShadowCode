//! Unified execution-target catalog. Rows are keyed by stable IDs
//! (`provider`, `account`, `model`, `route`), never by display name.
use super::{usage::UsageSnapshot, Vendor};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const GROUP_SUBSCRIPTIONS: &str = "subscriptions";
pub const GROUP_LOCAL: &str = "local";
pub const ROUTE_VENDOR: &str = "vendor_cli";
pub const ROUTE_LOCAL: &str = "local_llamacpp";
/// Subtitle for a vendor row whose CLI is signed in with an API key.
pub const API_KEY_SUBTITLE: &str = "API key login · billed per token";
/// Authentication succeeded, but the current login's billing was not verified.
pub const UNVERIFIED_BILLING_SUBTITLE: &str = "Cloud · billing unverified";
pub const UNVERIFIED_BILLING_DETAIL: &str = "Could not verify how this CLI login is billed. API charges may apply; subscription usage is unavailable.";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Ready,
    SignIn,
    SetupRequired,
    Unavailable,
}

impl Availability {
    pub fn label(self) -> &'static str {
        match self {
            Self::Ready => "Ready",
            Self::SignIn => "Sign in",
            Self::SetupRequired => "Setup required",
            Self::Unavailable => "Unavailable",
        }
    }
    pub fn from_doctor(state: &str) -> Self {
        match state {
            "ready" => Self::Ready,
            "not_logged_in" => Self::SignIn,
            "not_installed" | "disabled" => Self::SetupRequired,
            _ => Self::Unavailable,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PickerTarget {
    pub id: String,
    pub provider: String,
    pub account: String,
    pub model: String,
    pub route: String,
    pub group: String,
    pub name: String,
    pub subtitle: String,
    /// The catalog's explicit billing observation; independent of route/group.
    #[serde(default)]
    pub billing: Option<String>,
    pub inference: String,
    pub availability: Availability,
    pub availability_label: String,
    pub reason: String,
    pub featured: bool,
    pub vision: bool,
    pub tools: bool,
    /// The runtime's own default model.
    pub is_default: bool,
    pub usage: UsageSnapshot,
}

impl PickerTarget {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "provider": self.provider,
            "account": self.account,
            "model": self.model,
            "route": self.route,
            "group": self.group,
            "name": self.name,
            "subtitle": self.subtitle,
            "billing": self.billing,
            "inference": self.inference,
            "availability": self.availability,
            "availability_label": self.availability_label,
            "reason": self.reason,
            "featured": self.featured,
            "vision": self.vision,
            "tools": self.tools,
            "is_default": self.is_default,
            "usage": self.usage,
        })
    }
}

pub fn target_id(vendor: Vendor, model: &str) -> String {
    let model = model.trim();
    if model.is_empty() || model == "default" {
        vendor.provider()
    } else {
        format!("{}:{model}", vendor.provider())
    }
}

pub fn vendor_target(
    vendor: Vendor,
    model: &str,
    model_label: &str,
    availability: Availability,
    reason: &str,
    usage: UsageSnapshot,
    vision: bool,
) -> PickerTarget {
    let model = if model.trim().is_empty() {
        "default"
    } else {
        model.trim()
    };
    // "Auto" is a real router only where the runtime offers one (Cursor);
    // everywhere else the runtime's own default model is called Default.
    let auto = model == "auto" || (model == "default" && vendor.supports_auto_model());
    let name = if auto {
        format!("{} · Auto", vendor.product_label())
    } else if model == "default" {
        format!("{} · Default", vendor.product_label())
    } else {
        format!("{} · {model_label}", vendor.product_label())
    };
    PickerTarget {
        id: target_id(vendor, model),
        provider: vendor.provider(),
        account: format!("account:{}", vendor.id()),
        model: model.to_owned(),
        route: ROUTE_VENDOR.into(),
        group: GROUP_SUBSCRIPTIONS.into(),
        name,
        subtitle: "Cloud · subscription".into(),
        billing: None,
        inference: "cloud".into(),
        availability,
        availability_label: availability.label().into(),
        reason: reason.to_owned(),
        featured: vendor.featured(),
        vision,
        tools: true,
        is_default: auto || model == "default",
        usage,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_stable_and_not_display_names() {
        assert_eq!(target_id(Vendor::Codex, "default"), "cli:codex");
        assert_eq!(target_id(Vendor::Cursor, "auto"), "cli:cursor:auto");
        assert_eq!(
            target_id(Vendor::Cursor, "gpt-5.3-codex"),
            "cli:cursor:gpt-5.3-codex"
        );
        let a = vendor_target(
            Vendor::Cursor,
            "sonnet-4",
            "Sonnet 4",
            Availability::Ready,
            "Ready",
            UsageSnapshot::unavailable("Cursor"),
            false,
        );
        let b = vendor_target(
            Vendor::Claude,
            "sonnet-4",
            "Sonnet 4",
            Availability::Ready,
            "Ready",
            UsageSnapshot::unavailable("Claude Code"),
            false,
        );
        assert_ne!(a.id, b.id);
        assert!(!a.id.contains("Sonnet"));
    }
}
