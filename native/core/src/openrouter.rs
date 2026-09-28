//! OpenRouter: pay-per-token access to many hosted models through one API key
//! (https://openrouter.ai/docs). This is the path for people without a
//! subscription CLI. It is always shown as "API key · billed per token" in its
//! own picker group, never as a subscription.
//!
//! - The model list comes from the public `GET /api/v1/models` and is cached
//!   in the state directory; offline mode never fetches it.
//! - The key lives in the profile secrets file (`OPENROUTER_API_KEY`), is
//!   checked with `GET /api/v1/key` before it is saved, and is never returned
//!   to the window.
//! - Turns run on ShadowCode's own agent loop against the OpenAI-compatible
//!   `POST /api/v1/chat/completions`, so ShadowCode's permissions, approvals,
//!   checkpoints and review all apply.
use crate::config::{self, ModelConfig};
use crate::paths::{atomic_write, AppPaths};
use anyhow::{bail, ensure, Context, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const PROVIDER: &str = "openrouter";
pub const KEY_ENV: &str = "OPENROUTER_API_KEY";
pub const ID_PREFIX: &str = "api:openrouter:";
pub const KEYS_URL: &str = "https://openrouter.ai/keys";
pub const ACTIVITY_URL: &str = "https://openrouter.ai/activity";
const DEFAULT_BASE: &str = "https://openrouter.ai/api/v1";
/// The public list is about 1 MB today.
const MAX_CATALOG_BYTES: usize = 16_000_000;
const MAX_KEY_BYTES: usize = 64_000;
/// Refresh the cached list at most this often unless asked.
const CATALOG_TTL_SECS: f64 = 6.0 * 3600.0;
/// Upper bound for the agent's context budget on hosted models.
const CONTEXT_CAP: u64 = 200_000;
const CONTEXT_FALLBACK: u64 = 32_768;

/// API base. `SHADOWCODE_OPENROUTER_BASE` exists for tests and must be HTTPS
/// or loopback.
pub fn base_url() -> String {
    std::env::var("SHADOWCODE_OPENROUTER_BASE")
        .ok()
        .map(|v| v.trim().trim_end_matches('/').to_owned())
        .filter(|v| v.starts_with("https://") || crate::models::is_loopback_endpoint(v))
        .unwrap_or_else(|| DEFAULT_BASE.to_owned())
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Model {
    pub id: String,
    pub name: String,
    pub context_length: u64,
    /// USD per token, when OpenRouter publishes a fixed price.
    pub prompt_price: Option<f64>,
    pub completion_price: Option<f64>,
    pub tools: bool,
    pub vision: bool,
    /// Lists `reasoning` among its supported parameters (the composer's
    /// effort control applies). Older cached lists read as false.
    #[serde(default)]
    pub reasoning: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Catalog {
    pub fetched_at: f64,
    pub models: Vec<Model>,
}

fn valid_slug(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | ':'))
}

fn price(value: &Value) -> Option<f64> {
    let parsed = match value {
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Number(n) => n.as_f64(),
        _ => None,
    }?;
    // Negative prices mark routers whose cost depends on the model chosen.
    (parsed.is_finite() && parsed >= 0.0).then_some(parsed)
}

fn has(list: &Value, item: &str) -> bool {
    list.as_array()
        .is_some_and(|items| items.iter().any(|v| v.as_str() == Some(item)))
}

/// Text-generating models from a `GET /models` response, in OpenRouter's order.
pub fn parse_models(value: &Value) -> Vec<Model> {
    let mut seen = std::collections::HashSet::new();
    value["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| {
            let id = row["id"].as_str()?.trim();
            if !valid_slug(id) || !seen.insert(id.to_owned()) {
                return None;
            }
            let arch = &row["architecture"];
            if arch["output_modalities"].is_array() && !has(&arch["output_modalities"], "text") {
                return None;
            }
            let name = row["name"]
                .as_str()
                .map(|n| n.split_whitespace().collect::<Vec<_>>().join(" "))
                .filter(|n| !n.is_empty() && n.chars().count() <= 120)
                .unwrap_or_else(|| id.to_owned());
            Some(Model {
                id: id.to_owned(),
                name,
                context_length: row["context_length"]
                    .as_u64()
                    .or_else(|| row["top_provider"]["context_length"].as_u64())
                    .unwrap_or(CONTEXT_FALLBACK),
                prompt_price: price(&row["pricing"]["prompt"]),
                completion_price: price(&row["pricing"]["completion"]),
                tools: has(&row["supported_parameters"], "tools"),
                vision: has(&arch["input_modalities"], "image"),
                reasoning: has(&row["supported_parameters"], "reasoning"),
            })
        })
        .collect()
}

fn cache_path(state_dir: &Path) -> PathBuf {
    state_dir.join("openrouter-models.json")
}

pub fn cached(paths: &AppPaths) -> Option<Catalog> {
    cached_in(&paths.state)
}

fn cached_in(state_dir: &Path) -> Option<Catalog> {
    let bytes = std::fs::read(cache_path(state_dir)).ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn model_in(state_dir: &Path, slug: &str) -> Option<Model> {
    cached_in(state_dir)?
        .models
        .into_iter()
        .find(|m| m.id == slug)
}

fn client() -> Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("ShadowCode/", env!("CARGO_PKG_VERSION")))
        .build()?)
}

pub(crate) async fn read_capped(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(
            bytes.len() + chunk.len() <= limit,
            "OpenRouter response exceeded {} MB",
            limit / 1_000_000
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

pub fn is_fresh(catalog: &Catalog) -> bool {
    crate::now() - catalog.fetched_at < CATALOG_TTL_SECS && !catalog.models.is_empty()
}

/// The cached list, refreshed from OpenRouter when stale or when `force`.
pub async fn refresh(paths: &AppPaths, force: bool) -> Result<Catalog> {
    if !force {
        if let Some(cached) = cached(paths) {
            if is_fresh(&cached) {
                return Ok(cached);
            }
        }
    }
    let response = client()?
        .get(format!("{}/models", base_url()))
        .send()
        .await
        .context("Could not reach OpenRouter")?;
    let status = response.status();
    ensure!(
        status.is_success(),
        "OpenRouter's model list returned HTTP {}",
        status.as_u16()
    );
    let body: Value = serde_json::from_slice(&read_capped(response, MAX_CATALOG_BYTES).await?)
        .context("OpenRouter returned an unreadable model list")?;
    let models = parse_models(&body);
    ensure!(!models.is_empty(), "OpenRouter returned no text models");
    let catalog = Catalog {
        fetched_at: crate::now(),
        models,
    };
    atomic_write(
        &cache_path(&paths.state),
        &serde_json::to_vec(&catalog)?,
        false,
    )?;
    Ok(catalog)
}

pub fn key(paths: &AppPaths) -> Option<String> {
    config::secret(paths, KEY_ENV)
        .ok()
        .flatten()
        .filter(|k| !k.trim().is_empty())
}

/// `GET /key`: label, credits used and limit for the key. Never includes the
/// key itself.
pub async fn key_info(key: &str) -> Result<Value> {
    let response = client()?
        .get(format!("{}/key", base_url()))
        .bearer_auth(key)
        .send()
        .await
        .context("Could not reach OpenRouter")?;
    let status = response.status();
    if matches!(status.as_u16(), 401 | 403) {
        bail!("OpenRouter rejected this key ({})", status.as_u16());
    }
    ensure!(
        status.is_success(),
        "OpenRouter's key check returned HTTP {}",
        status.as_u16()
    );
    let body: Value = serde_json::from_slice(&read_capped(response, MAX_KEY_BYTES).await?)
        .context("OpenRouter returned an unreadable key check")?;
    let data = &body["data"];
    // The key's limit is not the account balance: a key can have $5 of its
    // limit left while the account has no credits, and then every paid
    // request fails with HTTP 402. `GET /credits` reports the balance.
    let credits_remaining = account_credits(key).await;
    Ok(json!({
        "label": data["label"].as_str().map(|l| crate::redaction::redact_text(l).text).unwrap_or_default(),
        "usage": data["usage"].as_f64().unwrap_or(0.0),
        "limit": data["limit"].as_f64(),
        "limit_remaining": data["limit_remaining"].as_f64(),
        "is_free_tier": data["is_free_tier"].as_bool().unwrap_or(false),
        "credits_remaining": credits_remaining,
    }))
}

/// `GET /credits`: credits bought minus credits used on the account, in US
/// dollars. `None` when OpenRouter does not answer it.
async fn account_credits(key: &str) -> Option<f64> {
    let response = client()
        .ok()?
        .get(format!("{}/credits", base_url()))
        .bearer_auth(key)
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body: Value =
        serde_json::from_slice(&read_capped(response, MAX_KEY_BYTES).await.ok()?).ok()?;
    let data = &body["data"];
    let remaining = data["total_credits"].as_f64()? - data["total_usage"].as_f64()?;
    remaining.is_finite().then_some(remaining)
}

/// Check a key with OpenRouter, then store it in the profile secrets file.
/// An empty key removes the stored one.
pub async fn save_key(paths: &AppPaths, key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() {
        return config::set_secret(paths, KEY_ENV, "");
    }
    ensure!(
        key.len() <= 512 && !key.chars().any(|c| c.is_whitespace() || c.is_control()),
        "That does not look like an OpenRouter API key"
    );
    key_info(key).await?;
    config::set_secret(paths, KEY_ENV, key)
}

/// Settings › Accounts status. `key_check` runs `GET /key` when a key is set.
pub async fn status(paths: &AppPaths, offline: bool, key_check: bool) -> Value {
    let key = key(paths);
    let catalog = cached(paths);
    let (info, key_error) = match (&key, offline, key_check) {
        (Some(key), false, true) => match key_info(key).await {
            Ok(info) => (Some(info), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        },
        _ => (None, None),
    };
    json!({
        "key_set": key.is_some(),
        "key": info,
        "key_error": key_error,
        "models": catalog.as_ref().map_or(0, |c| c.models.len()),
        "tool_models": catalog.as_ref().map_or(0, |c| c.models.iter().filter(|m| m.tools).count()),
        "fetched_at": catalog.as_ref().map(|c| c.fetched_at),
        "offline": offline,
        "keys_url": KEYS_URL,
        "activity_url": ACTIVITY_URL,
    })
}

fn per_million(price: Option<f64>) -> Option<String> {
    let value = price? * 1_000_000.0;
    Some(if value == 0.0 {
        "free".into()
    } else if value < 0.1 {
        format!("${value:.3}")
    } else {
        format!("${value:.2}")
    })
}

fn usage(model: &Model) -> Value {
    let (input, output) = (
        per_million(model.prompt_price),
        per_million(model.completion_price),
    );
    let label = match (&input, &output) {
        (Some(i), Some(o)) if i == "free" && o == "free" => "API key · free".to_owned(),
        (Some(i), Some(o)) => format!("API key · {i}/M in · {o}/M out"),
        _ => "API key · billed per token".to_owned(),
    };
    let mut detail =
        vec!["Billed per token to your OpenRouter account, not a subscription.".to_owned()];
    if let (Some(i), Some(o)) = (&input, &output) {
        detail.push(format!("Price per million tokens: {i} input, {o} output"));
    }
    detail.push(format!("Context window: {} tokens", model.context_length));
    json!({
        "state": "api_key",
        "label": label,
        "detail": detail,
        "plan": null,
        "pool": null,
        "pool_shared": false,
        "windows": [],
        "remaining_percent": null,
        "credits": null,
        "limit_reached": false,
        "last_refresh": null,
        "provider_usage_url": ACTIVITY_URL,
    })
}

/// Picker rows for the "API keys" group.
pub fn picker_rows(catalog: Option<&Catalog>, key_set: bool, offline: bool) -> Vec<Value> {
    let Some(catalog) = catalog else {
        return Vec::new();
    };
    catalog
        .models
        .iter()
        .map(|model| {
            let (availability, label, reason) = if offline {
                (
                    "unavailable",
                    "Unavailable",
                    "Offline mode: cloud rows are off".to_owned(),
                )
            } else if !key_set {
                (
                    "sign_in",
                    "Add API key",
                    "Add an OpenRouter API key in Settings › Accounts".to_owned(),
                )
            } else {
                ("ready", "Ready", String::new())
            };
            json!({
                "id": format!("{ID_PREFIX}{}", model.id),
                "provider": PROVIDER,
                "account": "",
                "model": model.id,
                "route": "native",
                "group": "api",
                "name": model.name,
                "subtitle": format!("OpenRouter · {}", model.id),
                "inference": "cloud",
                "availability": availability,
                "availability_label": label,
                "reason": reason,
                "featured": false,
                "vision": model.vision,
                "tools": model.tools,
                "reasoning": model.reasoning,
                "is_default": false,
                "usage": usage(model),
            })
        })
        .collect()
}

/// The agent's model configuration for an `api:openrouter:<slug>` id.
/// `state_dir` holds the cached model list (the key is checked in
/// [`prepare`], which has the profile paths).
pub fn model_config(state_dir: &Path, id: &str) -> Result<ModelConfig> {
    let slug = id
        .strip_prefix(ID_PREFIX)
        .filter(|s| valid_slug(s))
        .with_context(|| format!("'{id}' is not an OpenRouter model id"))?;
    let context = model_in(state_dir, slug)
        .map(|m| m.context_length)
        .unwrap_or(CONTEXT_FALLBACK)
        .clamp(4096, CONTEXT_CAP);
    Ok(ModelConfig {
        default: id.to_owned(),
        name: slug.to_owned(),
        provider: PROVIDER.into(),
        endpoint: base_url(),
        api_key_env: KEY_ENV.into(),
        // Ignored by OpenRouter; a valid value keeps config validation happy.
        keep_alive: "5m".into(),
        context_limit: context as usize,
    })
}

pub fn is_openrouter(model: &ModelConfig) -> bool {
    model.provider == PROVIDER && model.default.starts_with(ID_PREFIX)
}

/// Refuse an OpenRouter task before any job exists when it cannot run.
pub fn precheck(paths: &AppPaths, target: &str, offline: bool) -> Result<()> {
    let Some(slug) = target.strip_prefix(ID_PREFIX) else {
        return Ok(());
    };
    ensure!(
        !offline,
        "Offline mode: choose a model that runs on this computer"
    );
    ensure!(
        key(paths).is_some(),
        "Add an OpenRouter API key in Settings › Accounts to use {slug}"
    );
    Ok(())
}

/// Checks the key and applies the cached capabilities: chat-only models get
/// no tool schemas, and only models that accept images get them.
pub fn prepare(
    paths: &AppPaths,
    model: &ModelConfig,
) -> Result<crate::local_engine::PreparedModel> {
    ensure!(
        key(paths).is_some(),
        "Add an OpenRouter API key in Settings › Accounts to use {}",
        model.name
    );
    let known = model_in(&paths.state, &model.name);
    let mut prepared = crate::local_engine::PreparedModel::passthrough(model.clone());
    prepared.tools = known.as_ref().is_none_or(|m| m.tools);
    prepared.vision = Some(known.as_ref().is_some_and(|m| m.vision));
    Ok(prepared)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        json!({"data":[
            {"id":"qwen/qwen3-coder","name":"Qwen: Qwen3 Coder","context_length":262144,
             "architecture":{"input_modalities":["text"],"output_modalities":["text"]},
             "pricing":{"prompt":"0.0000003","completion":"0.0000012"},
             "supported_parameters":["tools","tool_choice"]},
            {"id":"google/gemma-free:free","name":"Google: Gemma (free)","context_length":8192,
             "architecture":{"input_modalities":["text","image"],"output_modalities":["text"]},
             "pricing":{"prompt":"0","completion":"0"},"supported_parameters":["max_tokens"]},
            {"id":"openrouter/auto","name":"Auto Router","context_length":2000000,
             "architecture":{"input_modalities":["text"],"output_modalities":["text"]},
             "pricing":{"prompt":"-1","completion":"-1"},"supported_parameters":["tools"]},
            {"id":"black-forest-labs/flux","name":"FLUX","architecture":{"output_modalities":["image"]}},
            {"id":"bad id with spaces"},
            {"id":"qwen/qwen3-coder","name":"duplicate"}
        ]})
    }

    #[test]
    fn parses_text_models_with_tools_vision_and_prices() {
        let models = parse_models(&sample());
        let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "qwen/qwen3-coder",
                "google/gemma-free:free",
                "openrouter/auto"
            ]
        );
        assert!(models[0].tools && !models[0].vision);
        assert_eq!(models[0].context_length, 262144);
        assert!(!models[1].tools && models[1].vision);
        assert_eq!(models[1].prompt_price, Some(0.0));
        assert_eq!(
            models[2].prompt_price, None,
            "router pricing is not a fixed price"
        );
    }

    #[test]
    fn rows_are_api_key_rows_never_subscriptions() {
        let catalog = Catalog {
            fetched_at: 1.0,
            models: parse_models(&sample()),
        };
        let rows = picker_rows(Some(&catalog), false, false);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0]["id"], "api:openrouter:qwen/qwen3-coder");
        assert_eq!(rows[0]["group"], "api");
        assert_eq!(rows[0]["inference"], "cloud");
        assert_eq!(rows[0]["availability"], "sign_in");
        assert_eq!(
            rows[0]["usage"]["label"],
            "API key · $0.30/M in · $1.20/M out"
        );
        assert_eq!(rows[1]["usage"]["label"], "API key · free");
        assert_eq!(rows[2]["usage"]["label"], "API key · billed per token");
        assert!(rows[0]["usage"]["detail"][0]
            .as_str()
            .unwrap()
            .contains("not a subscription"));
        let ready = picker_rows(Some(&catalog), true, false);
        assert_eq!(ready[0]["availability"], "ready");
        let offline = picker_rows(Some(&catalog), true, true);
        assert_eq!(offline[0]["availability"], "unavailable");
        assert!(picker_rows(None, true, false).is_empty());
    }

    #[test]
    fn the_real_endpoint_is_a_cloud_route_that_needs_consent_and_is_off_offline() {
        let model = ModelConfig {
            default: format!("{ID_PREFIX}qwen/qwen3-coder"),
            name: "qwen/qwen3-coder".into(),
            provider: PROVIDER.into(),
            endpoint: DEFAULT_BASE.into(),
            api_key_env: KEY_ENV.into(),
            keep_alive: "5m".into(),
            context_limit: 32_768,
        };
        assert!(!crate::cli_agent::handoff::is_local(&model));
        assert!(!crate::config::runs_on_this_computer(&model));
        let plan = crate::cli_agent::handoff::plan(
            &[
                json!({"status":"completed","mode":"code","task":"t","summary":"s",
                "routing":{"provider":"llamacpp","model_id":"local:gguf:x","model_name":"x","inference":"local"}}),
            ],
            &[],
            &crate::cli_agent::handoff::TurnRoute::of_model(&model),
            1,
            false,
            false,
        );
        assert!(plan.is_err(), "local context and images need consent first");
    }

    #[test]
    fn model_config_needs_a_key_and_uses_the_cached_context() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(root.path()).unwrap();
        paths.ensure().unwrap();
        let catalog = Catalog {
            fetched_at: crate::now(),
            models: parse_models(&sample()),
        };
        atomic_write(
            &cache_path(&paths.state),
            &serde_json::to_vec(&catalog).unwrap(),
            false,
        )
        .unwrap();
        let id = "api:openrouter:qwen/qwen3-coder";
        let model = model_config(&paths.state, id).unwrap();
        let error = prepare(&paths, &model).err().unwrap().to_string();
        assert!(error.contains("Add an OpenRouter API key"), "{error}");
        config::set_secret(&paths, KEY_ENV, "sk-or-test").unwrap();
        let prepared = prepare(&paths, &model).unwrap();
        assert!(prepared.tools);
        assert_eq!(prepared.vision, Some(false));
        let gemma = model_config(&paths.state, "api:openrouter:google/gemma-free:free").unwrap();
        let prepared = prepare(&paths, &gemma).unwrap();
        assert!(!prepared.tools, "chat-only models get no tool schemas");
        assert_eq!(prepared.vision, Some(true));
        assert_eq!(gemma.context_limit, 8192);
        assert_eq!(model.provider, "openrouter");
        assert_eq!(model.name, "qwen/qwen3-coder");
        assert_eq!(model.api_key_env, KEY_ENV);
        assert_eq!(model.context_limit, 200_000, "capped");
        assert!(model.endpoint.ends_with("/api/v1") || model.endpoint.starts_with("http://127."));
        assert!(
            !crate::config::runs_on_this_computer(&model)
                || model.endpoint.starts_with("http://127.")
        );
        assert!(model_config(&paths.state, "api:openrouter:bad slug").is_err());
    }
}
