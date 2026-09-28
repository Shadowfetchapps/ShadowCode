//! Allowance: one view of how much each way of running a model has left.
//! Built only from what the sources report (vendor usage snapshots, the
//! OpenRouter key check, the local catalog); nothing is estimated.
use serde_json::{json, Value};

/// Remaining percentage at or below which a subscription counts as low.
const LOW_PERCENT: f64 = 10.0;

fn subscription_row(id: &str, status: &Value) -> Value {
    let product = status["product"].as_str().unwrap_or(id).to_owned();
    let usage = &status["usage"];
    let url = usage["provider_usage_url"].clone();
    let windows: Vec<Value> = usage["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|w| {
            json!({
                "label": w["label"],
                "remaining_percent": w["remaining_percent"],
                "resets_at": w["resets_at"],
            })
        })
        .collect();
    let first_remaining = windows
        .iter()
        .filter_map(|w| w["remaining_percent"].as_f64())
        .fold(None::<f64>, |min, v| Some(min.map_or(v, |m| m.min(v))));
    let (state, headline) = match status["availability"].as_str().unwrap_or("") {
        "setup_required" => ("not_installed", "Not installed".to_owned()),
        "sign_in" => ("sign_in", "Not signed in".to_owned()),
        _ if usage["limit_reached"] == true || usage["state"] == "limit_reached" => {
            ("limit_reached", "Plan limit reached".to_owned())
        }
        "ready" => match first_remaining {
            Some(left) if left <= LOW_PERCENT => ("low", format!("{left:.0}% left")),
            Some(left) => ("ok", format!("{left:.0}% left")),
            None if usage["state"] == "api_key"
                || usage["label"]
                    .as_str()
                    .is_some_and(|l| l.contains("billed per token")) =>
            {
                ("ok", "API key login · billed per token".to_owned())
            }
            None => ("unknown", "Usage not reported".to_owned()),
        },
        _ => (
            "unavailable",
            status["detail"]
                .as_str()
                .filter(|d| !d.is_empty())
                .unwrap_or("Unavailable")
                .to_owned(),
        ),
    };
    json!({
        "id": format!("cli:{id}"),
        "kind": "subscription",
        "product": product,
        "state": state,
        "headline": headline,
        "remaining_percent": first_remaining,
        "windows": windows,
        "plan": status["account"]["plan"],
        "note": status["usage_note"],
        "last_checked": usage["last_refresh"],
        "usage_url": url,
    })
}

fn openrouter_row(status: &Value) -> Value {
    let key = &status["key"];
    let (state, headline, remaining) = if status["offline"] == true {
        ("offline", "Offline mode".to_owned(), None)
    } else if status["key_set"] != true {
        ("no_key", "No API key".to_owned(), None)
    } else if let Some(error) = status["key_error"].as_str() {
        ("unavailable", error.to_owned(), None)
    } else if key["credits_remaining"]
        .as_f64()
        .is_some_and(|credits| credits <= 0.0)
    {
        // The account balance, not the key's limit, decides whether a paid
        // request is accepted.
        (
            "limit_reached",
            "Out of credits · add credits on OpenRouter".to_owned(),
            Some(0.0),
        )
    } else {
        let used = key["usage"].as_f64().unwrap_or(0.0);
        let credits = key["credits_remaining"].as_f64();
        match (key["limit"].as_f64(), key["limit_remaining"].as_f64()) {
            (Some(limit), Some(left)) if limit > 0.0 => {
                let left = credits.map_or(left, |credits| left.min(credits));
                let percent = (left / limit * 100.0).clamp(0.0, 100.0);
                let state = if left <= 0.0 {
                    "limit_reached"
                } else if percent <= LOW_PERCENT {
                    "low"
                } else {
                    "ok"
                };
                (
                    state,
                    format!("${left:.2} of ${limit:.2} left"),
                    Some(percent),
                )
            }
            _ => match credits {
                Some(credits) => ("ok", format!("${credits:.2} credit left"), None),
                None => ("ok", format!("${used:.2} used · no limit set"), None),
            },
        }
    };
    json!({
        "id": "openrouter",
        "kind": "api_key",
        "product": "OpenRouter",
        "state": state,
        "headline": headline,
        "remaining_percent": remaining,
        "used": key["usage"],
        "limit": key["limit"],
        "limit_remaining": key["limit_remaining"],
        "credits_remaining": key["credits_remaining"],
        "usage_url": status["activity_url"],
    })
}

fn local_row(catalog: &Value, fallback: Option<(&str, &str)>, on_limit: &str) -> Value {
    let ready = catalog["models"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["availability"] == "ready")
        .count();
    json!({
        "id": "local",
        "kind": "local",
        "product": "On this computer",
        "state": if ready > 0 { "ok" } else { "none" },
        "headline": if ready > 0 {
            format!("No quota · {ready} model{} ready", if ready == 1 { "" } else { "s" })
        } else {
            "No local model ready".to_owned()
        },
        "remaining_percent": null,
        "ready_models": ready,
        "on_limit": on_limit,
        "fallback": fallback.map(|(id, name)| json!({"id": id, "name": name})),
    })
}

/// The Allowance rows: subscriptions (in picker order), OpenRouter, local.
pub fn build(
    vendors: &Value,
    openrouter: &Value,
    local_catalog: &Value,
    fallback: Option<(&str, &str)>,
    on_limit: &str,
    now: f64,
) -> Value {
    let mut rows = Vec::new();
    for id in ["codex", "claude", "cursor", "antigravity", "grok"] {
        if let Some(status) = vendors.get(id) {
            rows.push(subscription_row(id, status));
        }
    }
    rows.push(openrouter_row(openrouter));
    rows.push(local_row(local_catalog, fallback, on_limit));
    json!({"generated_at": now, "rows": rows})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_use_only_reported_figures() {
        let vendors = json!({
            "codex": {"product":"Codex","availability":"ready","account":{"plan":"pro"},
                "usage":{"state":"ok","windows":[{"label":"Weekly","remaining_percent":2.0,"resets_at":100.0}],
                    "last_refresh":50.0,"provider_usage_url":"https://chatgpt.com/codex/settings/usage"}},
            "claude": {"product":"Claude Code","availability":"ready","usage":{"state":"unavailable","windows":[]}},
            "cursor": {"product":"Cursor","availability":"sign_in","usage":{}},
            "antigravity": {"product":"Antigravity","availability":"setup_required","usage":{}},
            "grok": {"product":"Grok","availability":"ready","usage":{"state":"limit_reached","limit_reached":true,"windows":[]}},
        });
        let openrouter = json!({"key_set":true,"offline":false,
            "key":{"usage":0.25,"limit":5.0,"limit_remaining":4.75},"activity_url":"https://openrouter.ai/activity"});
        let catalog = json!({"models":[{"id":"local:gguf:a","availability":"ready"},{"id":"local:gguf:b","availability":"unavailable"}]});
        let out = build(
            &vendors,
            &openrouter,
            &catalog,
            Some(("local:gguf:a", "qwen3:14b")),
            "local",
            1.0,
        );
        let rows = out["rows"].as_array().unwrap();
        let ids: Vec<&str> = rows.iter().map(|r| r["id"].as_str().unwrap()).collect();
        assert_eq!(
            ids,
            [
                "cli:codex",
                "cli:claude",
                "cli:cursor",
                "cli:antigravity",
                "cli:grok",
                "openrouter",
                "local"
            ]
        );
        assert_eq!(rows[0]["state"], "low");
        assert_eq!(rows[0]["headline"], "2% left");
        assert_eq!(rows[0]["windows"][0]["resets_at"], 100.0);
        assert_eq!(rows[1]["state"], "unknown");
        assert_eq!(rows[2]["state"], "sign_in");
        assert_eq!(rows[3]["state"], "not_installed");
        assert_eq!(rows[4]["state"], "limit_reached");
        assert_eq!(rows[5]["headline"], "$4.75 of $5.00 left");
        assert_eq!(rows[5]["state"], "ok");
        assert_eq!(rows[6]["headline"], "No quota · 1 model ready");
        assert_eq!(rows[6]["fallback"]["name"], "qwen3:14b");
    }

    #[test]
    fn openrouter_account_credits_beat_the_keys_limit() {
        // Seen live: the key had $5.00 of its limit left while the account
        // balance was below zero, and every paid request failed with 402.
        let broke = openrouter_row(&json!({"key_set":true,"key":{
            "usage":0.01,"limit":5.0,"limit_remaining":4.99,"credits_remaining":-0.17}}));
        assert_eq!(broke["state"], "limit_reached");
        assert_eq!(
            broke["headline"],
            "Out of credits · add credits on OpenRouter"
        );
        assert_eq!(broke["remaining_percent"], 0.0);
        let low = openrouter_row(&json!({"key_set":true,"key":{
            "usage":0.5,"limit":5.0,"limit_remaining":4.5,"credits_remaining":0.25}}));
        assert_eq!(low["headline"], "$0.25 of $5.00 left");
        assert_eq!(low["state"], "low");
        let open = openrouter_row(&json!({"key_set":true,"key":{
            "usage":1.5,"limit":null,"credits_remaining":8.5}}));
        assert_eq!(open["headline"], "$8.50 credit left");
    }

    #[test]
    fn openrouter_without_key_limit_or_offline() {
        assert_eq!(openrouter_row(&json!({"key_set":false}))["state"], "no_key");
        assert_eq!(
            openrouter_row(&json!({"offline":true,"key_set":true}))["state"],
            "offline"
        );
        let no_limit = openrouter_row(&json!({"key_set":true,"key":{"usage":1.5,"limit":null}}));
        assert_eq!(no_limit["headline"], "$1.50 used · no limit set");
        let spent = openrouter_row(
            &json!({"key_set":true,"key":{"usage":5.0,"limit":5.0,"limit_remaining":0.0}}),
        );
        assert_eq!(spent["state"], "limit_reached");
    }
}
