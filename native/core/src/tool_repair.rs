//! Repairing tool calls that smaller and local models get slightly wrong,
//! instead of failing the turn.
//!
//! - Arguments that are almost JSON: wrapped in a code fence, with trailing
//!   commas, single quotes, raw newlines inside strings, or JSON encoded twice.
//! - Tool calls written as text instead of the protocol's `tool_calls`:
//!   Qwen/Hermes `<tool_call>{…}</tool_call>`, Llama `<|python_tag|>{…}` and
//!   `<function=name>{…}</function>`, or a whole answer that is one JSON call
//!   (optionally fenced). Only names of tools offered in the request count.
//!
//! Anything that still does not read as one JSON object is left as it was
//! and fails as before. The transcript notes each repair.
use serde_json::{Map, Value};

/// Parse tool arguments, repairing common slips. `None` when they are not
/// one JSON object even after repair.
pub fn arguments(raw: &str) -> Option<Value> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Some(Value::Object(Map::new()));
    }
    for candidate in candidates(trimmed) {
        if let Ok(value) = serde_json::from_str::<Value>(&candidate) {
            if value.is_object() {
                return Some(value);
            }
            // Arguments encoded twice: "{\"path\": \"a\"}".
            if let Some(inner) = value
                .as_str()
                .and_then(|s| serde_json::from_str::<Value>(s).ok())
            {
                if inner.is_object() {
                    return Some(inner);
                }
            }
        }
    }
    None
}

/// Increasingly forgiving readings of `text`.
fn candidates(text: &str) -> Vec<String> {
    let unfenced = strip_fence(text);
    let mut out = vec![unfenced.to_owned()];
    let no_commas = remove_trailing_commas(unfenced);
    out.push(no_commas.clone());
    let escaped = escape_raw_newlines(&no_commas);
    out.push(escaped.clone());
    if !escaped.contains('"') || looks_single_quoted(&escaped) {
        out.push(single_to_double_quotes(&escaped));
    }
    out
}

fn strip_fence(text: &str) -> &str {
    let text = text.trim();
    let Some(rest) = text.strip_prefix("```") else {
        return text;
    };
    let rest = rest.split_once('\n').map_or("", |(_, body)| body);
    rest.trim_end().strip_suffix("```").unwrap_or(rest).trim()
}

/// Drop commas directly before `}` or `]` (outside strings).
fn remove_trailing_commas(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for (i, &c) in chars.iter().enumerate() {
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
        }
        if c == ',' {
            let next = chars[i + 1..].iter().find(|c| !c.is_whitespace());
            if matches!(next, Some('}') | Some(']')) {
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Raw newlines and tabs inside strings become `\n` and `\t`.
fn escape_raw_newlines(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut escaped = false;
    for c in text.chars() {
        if in_string {
            if escaped {
                escaped = false;
                out.push(c);
                continue;
            }
            match c {
                '\\' => {
                    escaped = true;
                    out.push(c);
                }
                '"' => {
                    in_string = false;
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                _ => out.push(c),
            }
        } else {
            if c == '"' {
                in_string = true;
            }
            out.push(c);
        }
    }
    out
}

fn looks_single_quoted(text: &str) -> bool {
    text.trim_start().starts_with("{'") || text.contains("': '") || text.contains("':'")
}

/// `{'a': 'b'}` → `{"a": "b"}`, keeping apostrophes inside words.
fn single_to_double_quotes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    for (i, &c) in chars.iter().enumerate() {
        match c {
            '\'' => {
                let prev = i.checked_sub(1).map(|p| chars[p]);
                let next = chars.get(i + 1).copied();
                let inside_word = prev.is_some_and(char::is_alphanumeric)
                    && next.is_some_and(char::is_alphanumeric);
                if inside_word {
                    out.push('\'');
                } else {
                    in_string = !in_string;
                    out.push('"');
                }
            }
            '"' if in_string => out.push_str("\\\""),
            _ => out.push(c),
        }
    }
    out
}

/// One call read from text: the tool's name and its arguments.
#[derive(Clone, Debug, PartialEq)]
pub struct TextCall {
    pub name: String,
    pub arguments: Value,
}

fn call_from_value(value: &Value, tools: &[&str]) -> Option<TextCall> {
    let object = value.as_object()?;
    let name = object
        .get("name")
        .or_else(|| object.get("tool"))
        .or_else(|| object.get("function").and_then(|f| f.get("name")))
        .and_then(Value::as_str)?;
    if !tools.contains(&name) {
        return None;
    }
    let raw = object
        .get("arguments")
        .or_else(|| object.get("parameters"))
        .or_else(|| object.get("args"))
        .or_else(|| object.get("input"))
        .or_else(|| object.get("function").and_then(|f| f.get("arguments")))
        .cloned()
        .unwrap_or(Value::Object(Map::new()));
    let arguments = match raw {
        Value::String(text) => arguments(&text)?,
        value if value.is_object() => value,
        _ => return None,
    };
    Some(TextCall {
        name: name.to_owned(),
        arguments,
    })
}

fn between<'a>(text: &'a str, open: &str, close: &str) -> Vec<&'a str> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        let after = &rest[start + open.len()..];
        match after.find(close) {
            Some(end) => {
                found.push(&after[..end]);
                rest = &after[end + close.len()..];
            }
            None => {
                found.push(after);
                break;
            }
        }
    }
    found
}

/// Tool calls a model wrote as text, and the text left around them. Only
/// names in `tools` count; `None` when the text holds no such call.
pub fn calls_from_text(text: &str, tools: &[&str]) -> Option<(Vec<TextCall>, String)> {
    let trimmed = text.trim();
    let mut calls = Vec::new();
    // Qwen / Hermes: <tool_call>{…}</tool_call>
    for body in between(trimmed, "<tool_call>", "</tool_call>") {
        if let Some(call) = arguments(body).and_then(|v| call_from_value(&v, tools)) {
            calls.push(call);
        }
    }
    if !calls.is_empty() {
        return Some((calls, strip_all(trimmed, "<tool_call>", "</tool_call>")));
    }
    // Llama 3.1: <function=name>{…}</function>
    for block in between(trimmed, "<function=", "</function>") {
        if let Some((name, body)) = block.split_once('>') {
            if tools.contains(&name.trim()) {
                if let Some(arguments) = arguments(body) {
                    calls.push(TextCall {
                        name: name.trim().to_owned(),
                        arguments,
                    });
                }
            }
        }
    }
    if !calls.is_empty() {
        return Some((calls, strip_all(trimmed, "<function=", "</function>")));
    }
    // Llama 3.1: <|python_tag|>{…}
    if let Some((before, after)) = trimmed.split_once("<|python_tag|>") {
        let body = after.split("<|eom_id|>").next().unwrap_or(after);
        for part in body.split(';') {
            if let Some(call) = arguments(part).and_then(|v| call_from_value(&v, tools)) {
                calls.push(call);
            }
        }
        if !calls.is_empty() {
            return Some((calls, before.trim().to_owned()));
        }
    }
    // The whole answer is one call (optionally fenced).
    if let Some(call) = arguments(trimmed).and_then(|v| call_from_value(&v, tools)) {
        return Some((vec![call], String::new()));
    }
    None
}

fn strip_all(text: &str, open: &str, close: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(start) = rest.find(open) {
        out.push_str(&rest[..start]);
        match rest[start..].find(close) {
            Some(end) => rest = &rest[start + end + close.len()..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn almost_json_arguments_are_repaired() {
        let cases = [
            (r#"{"path": "a.txt"}"#, json!({"path":"a.txt"})),
            (
                "```json\n{\"path\": \"a.txt\"}\n```",
                json!({"path":"a.txt"}),
            ),
            (
                r#"{"path": "a.txt", "limit": 3,}"#,
                json!({"path":"a.txt","limit":3}),
            ),
            (r#"{"paths": ["a", "b",],}"#, json!({"paths":["a","b"]})),
            (
                "{\"content\": \"line one\nline two\"}",
                json!({"content":"line one\nline two"}),
            ),
            (
                "{'path': 'a.txt', 'note': \"it's fine\"}",
                json!({"path":"a.txt","note":"it's fine"}),
            ),
            (r#""{\"path\": \"a.txt\"}""#, json!({"path":"a.txt"})),
            ("", json!({})),
        ];
        for (raw, expected) in cases {
            assert_eq!(arguments(raw), Some(expected), "{raw}");
        }
        for broken in ["{\"path\": ", "[1, 2]", "just words", "\"a string\""] {
            assert_eq!(arguments(broken), None, "{broken}");
        }
    }

    #[test]
    fn calls_written_as_text_are_read_for_offered_tools_only() {
        let tools = ["read_file", "exec"];
        let qwen = "I'll look first.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"src/main.rs\"}}\n</tool_call>";
        let (calls, text) = calls_from_text(qwen, &tools).unwrap();
        assert_eq!(
            calls,
            vec![TextCall {
                name: "read_file".into(),
                arguments: json!({"path":"src/main.rs"})
            }]
        );
        assert_eq!(text, "I'll look first.");
        let two = "<tool_call>{\"name\":\"exec\",\"arguments\":{\"command\":\"ls\"}}</tool_call><tool_call>{\"name\":\"read_file\",\"arguments\":\"{\\\"path\\\":\\\"a\\\"}\"}</tool_call>";
        assert_eq!(calls_from_text(two, &tools).unwrap().0.len(), 2);
        let llama = "<|python_tag|>{\"name\": \"exec\", \"parameters\": {\"command\": \"cargo test\"}}<|eom_id|>";
        assert_eq!(
            calls_from_text(llama, &tools).unwrap().0[0].arguments,
            json!({"command":"cargo test"})
        );
        let function = "<function=exec>{\"command\": \"make\"}</function>";
        assert_eq!(calls_from_text(function, &tools).unwrap().0[0].name, "exec");
        let fenced =
            "```json\n{\"name\": \"exec\", \"arguments\": {\"command\": \"npm test\",}}\n```";
        assert_eq!(
            calls_from_text(fenced, &tools).unwrap().0[0].arguments,
            json!({"command":"npm test"})
        );
        // Unknown tools, prose with an example, and plain answers stay text.
        assert!(calls_from_text(
            "<tool_call>{\"name\":\"rm_everything\",\"arguments\":{}}</tool_call>",
            &tools
        )
        .is_none());
        assert!(calls_from_text(
            "To read a file, call {\"name\": \"read_file\"} with a path.",
            &tools
        )
        .is_none());
        assert!(calls_from_text("All tests pass.", &tools).is_none());
    }
}
