//! Reasoning effort: one composer choice (low, medium, high, or the model's
//! default) mapped onto each runtime's own control, and only where the
//! runtime has one:
//!
//! | runtime            | control                                              |
//! |--------------------|------------------------------------------------------|
//! | OpenRouter         | request field `reasoning.effort` (models that list `reasoning`) |
//! | llama.cpp (GGUF)   | `chat_template_kwargs.enable_thinking`: off for low, on for medium/high, on templates with the switch |
//! | Codex              | `turn/start.effort` (and `-c model_reasoning_effort="…"` for new threads and `codex exec`) |
//! | Claude Code        | `--effort <level>`; a CLI without the flag gets the `MAX_THINKING_TOKENS` budget |
//! | Grok (and any ACP agent with a `thought_level` option) | `session/set_config_option` on that option (Grok `reasoning_effort`) |
//! | Cursor             | none: the effort is part of Cursor's model ids; the composer hides the control |
//!
//! Picker rows say whether the control applies with `reasoning: true`; a
//! vendor that reports it per model (Claude Code, ACP agents) decides it
//! per row.
use crate::cli_agent::Vendor;
use anyhow::{bail, Result};
use serde_json::{json, Value};

pub const LEVELS: &[&str] = &["low", "medium", "high"];

/// `""` and `"default"` keep the model's default.
pub fn parse(value: &str) -> Result<Option<String>> {
    match value.trim() {
        "" | "default" => Ok(None),
        level if LEVELS.contains(&level) => Ok(Some(level.to_owned())),
        other => bail!("Unknown reasoning effort `{other}`: choose low, medium or high"),
    }
}

/// The effort a vendor CLI receives (`LaunchOptions::effort`).
pub fn vendor(vendor: Vendor, effort: Option<&str>) -> Option<String> {
    match vendor {
        // The ACP adapter applies it only when the session offers a
        // `thought_level` option with that value.
        Vendor::Codex | Vendor::Claude | Vendor::Grok | Vendor::Antigravity => {
            effort.map(str::to_owned)
        }
        Vendor::Cursor => None,
    }
}

/// Request body fields for the native loop, merged over the prepared
/// model's own (`PreparedModel::extra_body`).
pub fn native_body(openrouter: bool, extra: Option<Value>, effort: Option<&str>) -> Option<Value> {
    let Some(effort) = effort else {
        return extra;
    };
    if openrouter {
        let mut body = extra.unwrap_or_else(|| json!({}));
        body["reasoning"] = json!({"effort": effort});
        return Some(body);
    }
    match extra {
        // Only templates with the switch get it (local_engine::prepare).
        Some(mut body) if body["chat_template_kwargs"]["enable_thinking"].is_boolean() => {
            body["chat_template_kwargs"]["enable_thinking"] = json!(effort != "low");
            Some(body)
        }
        other => other,
    }
}

/// Whether a picker row offers the control.
pub fn row_supports(row: &Value) -> bool {
    let id = row["id"].as_str().unwrap_or("");
    if let Some(model) = crate::cli_agent::resolve_vendor(id) {
        let Some(runtime) = Vendor::from_provider(&model.provider) else {
            return false;
        };
        if vendor(runtime, Some("medium")).is_none() {
            return false;
        }
        // The runtime's own per-model answer (Claude Code `supportsEffort`,
        // an ACP `thought_level` option) wins; otherwise Codex and Claude
        // Code always take it and ACP agents only when they said so.
        return match row["reasoning"].as_bool() {
            Some(reported) => reported,
            None => matches!(runtime, Vendor::Codex | Vendor::Claude),
        };
    }
    if id.starts_with("local:gguf:") {
        return row["local"]["thinking_switch"] == true;
    }
    row["reasoning"] == true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_levels_and_default() {
        assert_eq!(parse("").unwrap(), None);
        assert_eq!(parse("default").unwrap(), None);
        assert_eq!(parse("high").unwrap().as_deref(), Some("high"));
        assert!(parse("max").is_err());
    }

    #[test]
    fn maps_to_each_runtime_or_nothing() {
        assert_eq!(vendor(Vendor::Codex, Some("low")).as_deref(), Some("low"));
        assert_eq!(
            vendor(Vendor::Claude, Some("high")).as_deref(),
            Some("high")
        );
        assert_eq!(vendor(Vendor::Cursor, Some("high")), None);
        assert_eq!(vendor(Vendor::Grok, Some("high")).as_deref(), Some("high"));
        assert_eq!(
            native_body(true, None, Some("high")).unwrap(),
            json!({"reasoning":{"effort":"high"}})
        );
        let thinking = json!({"chat_template_kwargs":{"enable_thinking":false}});
        assert_eq!(
            native_body(false, Some(thinking.clone()), Some("medium")).unwrap()
                ["chat_template_kwargs"]["enable_thinking"],
            true
        );
        assert_eq!(
            native_body(false, Some(thinking.clone()), Some("low")).unwrap()
                ["chat_template_kwargs"]["enable_thinking"],
            false
        );
        // No switch, no default change: the body stays as it was.
        assert_eq!(native_body(false, None, Some("high")), None);
        assert_eq!(
            native_body(false, Some(thinking.clone()), None),
            Some(thinking)
        );
    }

    #[test]
    fn picker_rows_say_where_the_control_applies() {
        assert!(row_supports(&json!({"id":"cli:codex:gpt-5"})));
        assert!(row_supports(&json!({"id":"cli:claude"})));
        assert!(row_supports(
            &json!({"id":"cli:claude:opus","reasoning":true})
        ));
        assert!(!row_supports(
            &json!({"id":"cli:claude:haiku","reasoning":false})
        ));
        assert!(!row_supports(&json!({"id":"cli:cursor:auto"})));
        assert!(!row_supports(
            &json!({"id":"cli:cursor:auto","reasoning":true})
        ));
        assert!(!row_supports(&json!({"id":"cli:grok:grok-4.7"})));
        assert!(row_supports(
            &json!({"id":"cli:grok:grok-4.7","reasoning":true})
        ));
        assert!(row_supports(
            &json!({"id":"local:gguf:ab","local":{"thinking_switch":true}})
        ));
        assert!(!row_supports(
            &json!({"id":"local:gguf:ab","local":{"thinking_switch":false}})
        ));
        assert!(row_supports(
            &json!({"id":"api:openrouter:x/y","reasoning":true})
        ));
        assert!(!row_supports(&json!({"id":"api:openrouter:x/y"})));
    }
}
