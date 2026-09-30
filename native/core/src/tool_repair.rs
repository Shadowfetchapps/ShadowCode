//! Repairing tool calls that smaller and local models get slightly wrong,
//! instead of failing the turn.
//!
//! - Arguments that are almost JSON: wrapped in a code fence, with trailing
//!   commas, single quotes, raw newlines inside strings, or JSON encoded twice.
//! - Tool calls written as text instead of the protocol's `tool_calls`:
//!   Qwen/Hermes `<tool_call>{…}</tool_call>`, Llama `<|python_tag|>{…}` and
//!   `<function=name>{…}</function>`, or a whole answer that is one JSON call
//!   (optionally fenced). Only names of tools offered in the request count,
//!   and only calls that end the answer: one in a code fence or followed by
//!   more text was quoted, not made.
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
    out.push(escaped);
    if !unfenced.contains('"') || looks_single_quoted(unfenced) {
        // Quotes first, so the comma and newline repairs see the strings.
        let quoted = single_to_double_quotes(unfenced);
        out.push(escape_raw_newlines(&remove_trailing_commas(&quoted)));
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

/// `{'a': 'b'}` → `{"a": "b"}`, keeping apostrophes inside words and
/// escaped ones (`'it\'s'`) as they were meant.
fn single_to_double_quotes(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut in_string = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '\\' => match chars.get(i) {
                // `\'` is a plain apostrophe in JSON.
                Some('\'') => {
                    out.push('\'');
                    i += 1;
                }
                Some(&next) => {
                    out.push('\\');
                    out.push(next);
                    i += 1;
                }
                None => out.push('\\'),
            },
            '\'' => {
                // `i` is already past this quote.
                let prev = i.checked_sub(2).map(|p| chars[p]);
                let next = chars.get(i).copied();
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

/// The call blocks that end the answer: from an `open` outside any code
/// fence, nothing but `open…close` blocks follow (the last may be
/// unclosed). Returns the prose before them and each block's body. A block
/// the model only quoted, in a code fence or with more prose after it, is
/// not a call.
fn trailing_blocks<'a>(text: &'a str, open: &str, close: &str) -> Option<(&'a str, Vec<&'a str>)> {
    text.match_indices(open).find_map(|(start, _)| {
        let prose = &text[..start];
        if prose.matches("```").count() % 2 == 1 {
            return None;
        }
        let mut bodies = Vec::new();
        let mut rest = &text[start..];
        while let Some(after) = rest.strip_prefix(open) {
            match after.find(close) {
                Some(end) => {
                    bodies.push(&after[..end]);
                    rest = after[end + close.len()..].trim_start();
                }
                None => {
                    bodies.push(after);
                    rest = "";
                }
            }
        }
        rest.is_empty().then(|| (prose.trim_end(), bodies))
    })
}

/// `text` cut at each `separator` outside a double-quoted string.
fn split_outside_strings(text: &str, separator: char) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut in_string, mut escaped) = (0, false, false);
    for (at, c) in text.char_indices() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == separator {
            parts.push(&text[start..at]);
            start = at + c.len_utf8();
        }
    }
    parts.push(&text[start..]);
    parts
}

/// Tool calls a model wrote as text at the end of its answer, and the text
/// before them. Only names in `tools` count; `None` when the text holds no
/// such call.
pub fn calls_from_text(text: &str, tools: &[&str]) -> Option<(Vec<TextCall>, String)> {
    let trimmed = text.trim();
    let read = |body: &str| arguments(body).and_then(|v| call_from_value(&v, tools));
    // Qwen / Hermes: <tool_call>{…}</tool_call>
    if let Some((prose, bodies)) = trailing_blocks(trimmed, "<tool_call>", "</tool_call>") {
        let calls: Vec<TextCall> = bodies.into_iter().filter_map(read).collect();
        if !calls.is_empty() {
            return Some((calls, prose.to_owned()));
        }
    }
    // Llama 3.1: <function=name>{…}</function>
    if let Some((prose, blocks)) = trailing_blocks(trimmed, "<function=", "</function>") {
        let calls: Vec<TextCall> = blocks
            .into_iter()
            .filter_map(|block| {
                let (name, body) = block.split_once('>')?;
                let name = name.trim();
                if !tools.contains(&name) {
                    return None;
                }
                Some(TextCall {
                    name: name.to_owned(),
                    arguments: arguments(body)?,
                })
            })
            .collect();
        if !calls.is_empty() {
            return Some((calls, prose.to_owned()));
        }
    }
    // Llama 3.1: <|python_tag|>{…}, several calls separated by `;` (a `;`
    // inside an argument string does not separate).
    if let Some((prose, bodies)) = trailing_blocks(trimmed, "<|python_tag|>", "<|eom_id|>") {
        let mut calls = Vec::new();
        for body in bodies {
            match read(body) {
                Some(call) => calls.push(call),
                None => calls.extend(
                    split_outside_strings(body, ';')
                        .into_iter()
                        .filter_map(read),
                ),
            }
        }
        if !calls.is_empty() {
            return Some((calls, prose.to_owned()));
        }
    }
    // The whole answer is one call (optionally fenced).
    if let Some(call) = read(trimmed) {
        return Some((vec![call], String::new()));
    }
    None
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

    #[test]
    fn quoted_calls_are_not_run() {
        let tools = ["exec", "write_file"];
        let example =
            "<tool_call>{\"name\":\"write_file\",\"arguments\":{\"path\":\"config.json\",\"content\":\"{}\"}}</tool_call>";
        // In a code fence, or with more of the answer after it: quoted.
        let fenced = format!("The README shows this format:\n```\n{example}\n```\nThat is all.");
        assert!(calls_from_text(&fenced, &tools).is_none());
        let fenced_last = format!("Calls look like this:\n```xml\n{example}");
        assert!(calls_from_text(&fenced_last, &tools).is_none());
        let inline = format!("A call such as {example} writes the file.");
        assert!(calls_from_text(&inline, &tools).is_none());
        // A real call after a quoted one: only the real one runs, and the
        // quoted one stays in the answer.
        let real = "<tool_call>{\"name\":\"exec\",\"arguments\":{\"command\":\"ls\"}}</tool_call>";
        let both = format!("The format is {example}. Now:\n{real}");
        let (calls, text) = calls_from_text(&both, &tools).unwrap();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "exec");
        assert!(text.contains("config.json"), "{text}");
        let quoted_tag = "Llama writes `<|python_tag|>` before a call, like:\n```\n<|python_tag|>{\"name\": \"exec\", \"parameters\": {\"command\": \"ls\"}}\n```";
        assert!(calls_from_text(quoted_tag, &tools).is_none());
    }

    #[test]
    fn python_tag_calls_keep_semicolons_in_their_arguments() {
        let tools = ["exec", "write_file"];
        let one = "<|python_tag|>{\"name\": \"exec\", \"parameters\": {\"command\": \"cd web; npm test\"}}<|eom_id|>";
        let (calls, text) = calls_from_text(one, &tools).unwrap();
        assert_eq!(calls[0].arguments, json!({"command":"cd web; npm test"}));
        assert_eq!(text, "");
        let two = "<|python_tag|>{\"name\": \"exec\", \"parameters\": {\"command\": \"a; b\"}}; {\"name\": \"write_file\", \"parameters\": {\"path\": \"a.rs\", \"content\": \"let x = 1;\"}}";
        let (calls, _) = calls_from_text(two, &tools).unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].arguments, json!({"command":"a; b"}));
        assert_eq!(
            calls[1].arguments,
            json!({"path":"a.rs","content":"let x = 1;"})
        );
    }

    #[test]
    fn single_quoted_arguments_keep_escaped_quotes_and_their_text() {
        assert_eq!(
            arguments(r"{'path': 'src/a.rs', 'content': 'let c = \'a\';'}"),
            Some(json!({"path":"src/a.rs","content":"let c = 'a';"}))
        );
        assert_eq!(
            arguments("{'path': 'a.txt', 'content': 'x = [1,]\nend,}',}"),
            Some(json!({"path":"a.txt","content":"x = [1,]\nend,}"}))
        );
    }
}
