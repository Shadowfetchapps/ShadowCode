//! Routing picks a registered configuration once when a task is queued. It
//! never changes permissions or retries a failed request against another host.
use crate::{
    config::{Config, ModelConfig},
    model_registry,
    store::Store,
};
use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PURPOSES: [&str; 8] = [
    "planner",
    "coder",
    "reviewer",
    "tester",
    "architecture",
    "small_edits",
    "vision",
    "local",
];
pub fn purpose(value: &str, mode: &str) -> Result<&'static str> {
    Ok(match value {
        "" => match mode {
            "plan" => "planner",
            "review" => "reviewer",
            _ => "coder",
        },
        "planner" | "planning" | "plan" | "researcher" => "planner",
        "coder" | "coding" | "code" | "debugger" => "coder",
        "reviewer" | "review" => "reviewer",
        "tester" | "test" => "tester",
        "architecture" => "architecture",
        "small_edits" => "small_edits",
        "vision" => "vision",
        "local" => "local",
        _ => anyhow::bail!("Unknown task purpose: {value}"),
    })
}
pub fn validate(value: &Value) -> Result<()> {
    let object = value.as_object().ok_or_else(|| {
        anyhow::anyhow!(
            "routing must be a group of settings, for example `routing: {{enabled: false}}`"
        )
    })?;
    for (key, value) in object {
        if key == "enabled" {
            ensure!(value.is_boolean(), "routing.enabled must be a boolean");
        } else {
            ensure!(
                PURPOSES.contains(&key.as_str()),
                "routing.{key} is not a routing purpose; use one of {}",
                PURPOSES.join(", ")
            );
            ensure!(
                value.as_str().is_some_and(|value| value.len() <= 1024),
                "routing.{key} must be a model id (text of at most 1024 bytes)"
            );
        }
    }
    Ok(())
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Decision {
    pub purpose: String,
    pub source: String,
    pub requested: String,
    pub model_id: String,
    pub model_name: String,
    pub provider: String,
    pub context_limit: usize,
    pub fallback_reason: Option<String>,
    /// `local` (this computer) or `cloud`; decides handoff consent.
    pub inference: String,
    /// `vendor_cli`, `local_llamacpp`, or `native_http`.
    pub route: String,
    /// Plan → Implement → Review only: the roles that run on this computer.
    /// A second opinion on the task asks before sending a change a local
    /// implement role wrote to a cloud reviewer.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_roles: Option<Vec<String>>,
}

/// Where a model configuration runs, for records and consent.
pub fn route_of(model: &ModelConfig) -> (&'static str, &'static str) {
    let local = crate::cli_agent::handoff::is_local(model);
    let route = if crate::cli_agent::is_cli_provider(&model.provider) {
        crate::cli_agent::picker::ROUTE_VENDOR
    } else if model.provider == "llamacpp" {
        crate::cli_agent::picker::ROUTE_LOCAL
    } else {
        "native_http"
    };
    (if local { "local" } else { "cloud" }, route)
}
pub fn select(
    store: &Store,
    config: &Config,
    override_model: Option<ModelConfig>,
    role: &str,
) -> Result<(ModelConfig, Decision)> {
    let mut decision = Decision {
        purpose: role.into(),
        ..Default::default()
    };
    let model = if let Some(model) = override_model {
        decision.source = "explicit".into();
        decision.requested = model.default.clone();
        model
    } else {
        let requested = config
            .routing
            .get(role)
            .and_then(Value::as_str)
            .unwrap_or("");
        // "mock" was the legacy per-role placeholder, not an installed model.
        if config.routing["enabled"] != true || matches!(requested, "" | "default" | "mock") {
            decision.source = "default".into();
            decision.requested = config.model.default.clone();
            config.model.clone()
        } else {
            decision.requested = requested.into();
            match model_registry::resolve(store, requested, &config.model) {
                Ok(model) if model.provider != "mock" => {
                    decision.source = "purpose".into();
                    model
                }
                result => {
                    // A local route never silently becomes a cloud one.
                    let requested_local = requested.starts_with("local:gguf:")
                        || requested.starts_with("llamacpp:")
                        || result
                            .as_ref()
                            .is_ok_and(crate::cli_agent::handoff::is_local);
                    anyhow::ensure!(
                        !requested_local || crate::cli_agent::handoff::is_local(&config.model),
                        "The {role} route points to a model on this computer that is not available ({}); ShadowCode will not fall back to a cloud model. Choose another model.",
                        match &result {
                            Err(error) => error.to_string(),
                            Ok(_) => "offline preview".into(),
                        }
                    );
                    decision.source = "fallback".into();
                    decision.fallback_reason = Some(match result {
                        Err(error) => error.to_string(),
                        _ => "The routed model is an offline preview".into(),
                    });
                    config.model.clone()
                }
            }
        }
    };
    model_registry::validate(&model)?;
    decision.model_id = model.default.clone();
    decision.model_name = model.name.clone();
    decision.provider = model.provider.clone();
    decision.context_limit = model.context_limit;
    let (inference, route) = route_of(&model);
    decision.inference = inference.into();
    decision.route = route.into();
    Ok((model, decision))
}
pub fn view(store: &Store, config: &Config) -> Result<Value> {
    let mut table = serde_json::Map::new();
    let mut decisions = serde_json::Map::new();
    for role in PURPOSES {
        let (_, decision) = select(store, config, None, role)?;
        table.insert(role.into(), json!(decision.model_id));
        decisions.insert(role.into(), json!(decision));
    }
    Ok(
        json!({"enabled":config.routing["enabled"]==true,"default":config.model.default,"default_name":config.model.name,"table":table,"config":config.routing,"decisions":decisions,"models":model_registry::catalog(store,&config.model)?}),
    )
}
