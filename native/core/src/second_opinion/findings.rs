//! Findings from a reviewer's reply.
//!
//! The reviewer is asked for one JSON object, but replies arrive wrapped in
//! prose, code fences or reasoning blocks, with other key names, trailing
//! commas, cut off mid-object, or as a plain Markdown list. Each form is
//! tried in turn. Whatever cannot be read as findings stays visible as the
//! summary text, with a plain note saying so, instead of being dropped.
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::sync::LazyLock;

/// At most this many findings are kept from one reply.
pub const MAX_FINDINGS: usize = 50;
const MAX_TITLE: usize = 200;
const MAX_EXPLANATION: usize = 2_000;
const MAX_FIX: usize = 4_000;
const MAX_PATH: usize = 500;
const MAX_SUMMARY: usize = 12_000;

/// One problem the reviewer reported.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Finding {
    /// `f1`, `f2`… in reply order.
    pub id: String,
    /// Project-relative path as the diff names it; empty when the reviewer
    /// named no file.
    pub file: String,
    /// First line in the new version of the file, when given.
    pub line: Option<u64>,
    pub end_line: Option<u64>,
    /// Header (`@@ -a,b +c,d @@`) of the reviewed hunk the line falls in.
    pub hunk: Option<String>,
    /// `high`, `medium`, `low` or `info`.
    pub severity: String,
    pub title: String,
    pub explanation: String,
    pub suggested_fix: String,
    /// `open`, `dismissed` or `fixing` (a follow-up task was queued).
    pub status: String,
    /// The follow-up task queued by "Ask the agent to fix this".
    pub fix_job_id: Option<String>,
    pub fix_session_id: Option<String>,
}

/// What a reply said.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Parsed {
    pub summary: String,
    pub findings: Vec<Finding>,
    /// Plain words about a reply that was not in the requested form; empty
    /// when it was.
    pub note: String,
}

/// Remove `<think>…</think>` style reasoning. An unclosed tag loses only the
/// tag itself, never the answer after it.
pub fn strip_reasoning(text: &str) -> String {
    let mut text = text.to_owned();
    for (open, close) in [
        ("<think>", "</think>"),
        ("<thinking>", "</thinking>"),
        ("<reasoning>", "</reasoning>"),
    ] {
        while let Some(start) = text.find(open) {
            match text[start..].find(close) {
                Some(end) => text.replace_range(start..start + end + close.len(), ""),
                None => text.replace_range(start..start + open.len(), ""),
            }
        }
        // A reply that starts inside a reasoning block (the opening tag was
        // part of the prompt template) ends it with a lone closing tag.
        if let Some(end) = text.find(close) {
            text.replace_range(..end + close.len(), "");
        }
    }
    text.trim().to_owned()
}

pub fn clip(text: &str, limit: usize) -> String {
    let text = text.trim();
    if text.chars().count() <= limit {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(limit.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

/// Read a reviewer's reply.
pub fn parse(reply: &str) -> Parsed {
    let clean = strip_reasoning(reply);
    if clean.is_empty() {
        return Parsed {
            note: "The reviewer's reply was empty.".into(),
            ..Default::default()
        };
    }
    for (candidate, span) in candidates(&clean) {
        let Some(value) = parse_json(&candidate) else {
            continue;
        };
        if let Some((summary, findings)) = interpret(&value) {
            let summary = if summary.is_empty() {
                prose_outside(&clean, span)
            } else {
                summary
            };
            return Parsed {
                summary: clip(&summary, MAX_SUMMARY),
                findings: number(findings),
                note: String::new(),
            };
        }
    }
    // A reply cut off mid-object still carries its complete findings.
    let salvaged: Vec<Finding> = objects(&clean)
        .into_iter()
        .filter_map(|(text, _)| parse_json(&text))
        .filter_map(|value| finding(&value))
        .collect();
    if !salvaged.is_empty() {
        let count = salvaged.len().min(MAX_FINDINGS);
        return Parsed {
            summary: clip(&summary_before_json(&clean), MAX_SUMMARY),
            findings: number(salvaged),
            note: format!(
                "The reviewer's reply was cut short or malformed; {count} finding{} could be read from it.",
                if count == 1 { "" } else { "s" }
            ),
        };
    }
    let listed = markdown_findings(&clean);
    if !listed.is_empty() {
        return Parsed {
            summary: clip(&clean, MAX_SUMMARY),
            findings: number(listed),
            note: "The reviewer answered in prose; its list was read as findings.".into(),
        };
    }
    Parsed {
        note: if looks_clean(&clean) {
            String::new()
        } else {
            "The reviewer did not list findings in the requested form; its reply is shown as written.".into()
        },
        summary: clip(&clean, MAX_SUMMARY),
        findings: Vec::new(),
    }
}

fn number(mut findings: Vec<Finding>) -> Vec<Finding> {
    let mut seen = std::collections::HashSet::new();
    findings.retain(|f| {
        seen.insert((
            f.file.clone(),
            f.line,
            f.title.clone(),
            f.explanation.clone(),
        ))
    });
    findings.truncate(MAX_FINDINGS);
    for (index, finding) in findings.iter_mut().enumerate() {
        finding.id = format!("f{}", index + 1);
        finding.status = "open".into();
    }
    findings
}

/// A short "no problems" reply needs no format note.
fn looks_clean(text: &str) -> bool {
    static CLEAN: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)\b(no (issues|problems|findings|bugs)|lgtm|looks (good|correct|fine)|nothing to (report|flag))\b").unwrap()
    });
    text.chars().count() < 600 && CLEAN.is_match(text)
}

/// JSON candidates in the order they are tried, with the byte span each
/// came from (to find the prose around it).
fn candidates(text: &str) -> Vec<(String, (usize, usize))> {
    static FENCE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?s)```[A-Za-z0-9_-]*[ \t]*\r?\n(.*?)```").unwrap());
    let mut out = Vec::new();
    for capture in FENCE.captures_iter(text) {
        let whole = capture.get(0).expect("match");
        out.push((capture[1].trim().to_owned(), (whole.start(), whole.end())));
    }
    out.push((text.trim().to_owned(), (0, 0)));
    // Top-level objects and arrays anywhere in the text.
    out.extend(balanced(text, false));
    out
}

/// Parse JSON, then once more after light repairs (trailing commas, typographic
/// quotes around keys and values, `//` comments).
fn parse_json(text: &str) -> Option<Value> {
    let text = text.trim();
    if !(text.starts_with('{') || text.starts_with('[')) {
        return None;
    }
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        return Some(value);
    }
    serde_json::from_str::<Value>(&repair(text)).ok()
}

fn repair(text: &str) -> String {
    static TRAILING: LazyLock<Regex> = LazyLock::new(|| Regex::new(r",(\s*[}\]])").unwrap());
    static COMMENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*//.*$").unwrap());
    let text = text.replace(['\u{201c}', '\u{201d}'], "\"");
    let text = COMMENT.replace_all(&text, "");
    TRAILING.replace_all(&text, "$1").into_owned()
}

/// Balanced `{…}` / `[…]` spans, string-aware. With `nested`, every object
/// at any depth; otherwise only the outermost ones.
fn balanced(text: &str, nested: bool) -> Vec<(String, (usize, usize))> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut stack: Vec<(u8, usize)> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for (index, &byte) in bytes.iter().enumerate() {
        if in_string {
            match byte {
                _ if escaped => escaped = false,
                b'\\' => escaped = true,
                b'"' => in_string = false,
                _ => {}
            }
            continue;
        }
        match byte {
            b'"' if !stack.is_empty() => in_string = true,
            b'{' | b'[' => stack.push((byte, index)),
            b'}' | b']' => {
                let want = if byte == b'}' { b'{' } else { b'[' };
                match stack.last() {
                    Some(&(open, start)) if open == want => {
                        stack.pop();
                        if nested && open == b'{' || !nested && stack.is_empty() {
                            out.push((text[start..=index].to_owned(), (start, index + 1)));
                        }
                    }
                    // Unbalanced: start over from here.
                    _ => stack.clear(),
                }
            }
            _ => {}
        }
    }
    out
}

fn objects(text: &str) -> Vec<(String, (usize, usize))> {
    balanced(text, true)
}

/// Prose before and after the JSON a reply's findings came from.
fn prose_outside(text: &str, span: (usize, usize)) -> String {
    if span == (0, 0) {
        return String::new();
    }
    let before = text[..span.0].trim();
    let after = text[span.1..].trim();
    [before, after]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

fn summary_before_json(text: &str) -> String {
    let cut = text
        .find("```")
        .or_else(|| text.find('{'))
        .or_else(|| text.find('['))
        .unwrap_or(text.len());
    text[..cut].trim().to_owned()
}

const LIST_KEYS: &[&str] = &[
    "findings", "issues", "problems", "comments", "results", "items", "bugs", "concerns",
];
const SUMMARY_KEYS: &[&str] = &[
    "summary",
    "overview",
    "verdict",
    "overall",
    "assessment",
    "conclusion",
];

/// `(summary, findings)` when `value` has the shape of a review.
fn interpret(value: &Value) -> Option<(String, Vec<Finding>)> {
    match value {
        Value::Object(map) => {
            let summary = SUMMARY_KEYS
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_str))
                .unwrap_or("")
                .trim()
                .to_owned();
            if let Some(list) = LIST_KEYS
                .iter()
                .find_map(|key| map.get(*key).and_then(Value::as_array))
            {
                return Some((summary, list.iter().filter_map(finding).collect()));
            }
            // One finding on its own, or a summary with nothing to report.
            match finding(value).filter(|one| !one.file.is_empty()) {
                Some(one) => Some((summary, vec![one])),
                None => (!summary.is_empty()).then_some((summary, Vec::new())),
            }
        }
        Value::Array(items) => {
            let found: Vec<Finding> = items.iter().filter_map(finding).collect();
            (items.is_empty() || !found.is_empty()).then_some((String::new(), found))
        }
        _ => None,
    }
}

fn text_of(map: &serde_json::Map<String, Value>, keys: &[&str]) -> String {
    keys.iter()
        .find_map(|key| match map.get(*key) {
            Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_owned()),
            Some(Value::Array(items)) if !items.is_empty() => Some(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"),
            )
            .filter(|s| !s.trim().is_empty()),
            _ => None,
        })
        .unwrap_or_default()
}

/// A number, `"12"`, `"L12"`, `"12-18"` or `"12:4"` as `(line, end)`.
fn lines_of(value: Option<&Value>) -> (Option<u64>, Option<u64>) {
    static RANGE: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?i)^\s*L?(\d+)(?:\s*(?:-|–|to|\.\.)\s*L?(\d+))?").unwrap());
    match value {
        Some(Value::Number(n)) => (n.as_u64().filter(|n| *n > 0), None),
        Some(Value::String(text)) => match RANGE.captures(text) {
            Some(c) => (
                c[1].parse().ok().filter(|n| *n > 0),
                c.get(2).and_then(|m| m.as_str().parse().ok()),
            ),
            None => (None, None),
        },
        Some(Value::Array(items)) => {
            let numbers: Vec<u64> = items.iter().filter_map(Value::as_u64).collect();
            (numbers.first().copied(), numbers.get(1).copied())
        }
        Some(Value::Object(map)) => (
            map.get("start").and_then(Value::as_u64),
            map.get("end").and_then(Value::as_u64),
        ),
        _ => (None, None),
    }
}

/// `high`, `medium`, `low` or `info`; `None` for a word that is not one.
pub fn severity(word: &str) -> Option<&'static str> {
    let word = word
        .trim()
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_ascii_lowercase();
    Some(match word.as_str() {
        "critical" | "blocker" | "blocking" | "high" | "severe" | "error" | "bug" | "security"
        | "p0" | "p1" | "must-fix" | "must fix" => "high",
        "medium" | "moderate" | "major" | "warning" | "warn" | "p2" | "should-fix"
        | "should fix" => "medium",
        "low" | "minor" | "nit" | "nitpick" | "style" | "trivial" | "suggestion" | "p3"
        | "optional" => "low",
        "info" | "note" | "question" | "praise" | "fyi" | "informational" => "info",
        _ => return None,
    })
}

/// A finding from one JSON object, or `None` when it says nothing usable.
fn finding(value: &Value) -> Option<Finding> {
    let map = value.as_object()?;
    let mut file = text_of(
        map,
        &[
            "file",
            "path",
            "filename",
            "file_path",
            "filepath",
            "location",
        ],
    );
    let (mut line, mut end_line) = lines_of(
        [
            "line",
            "line_number",
            "lineNumber",
            "start_line",
            "startLine",
            "line_start",
            "lines",
            "line_range",
        ]
        .iter()
        .find_map(|key| map.get(*key).filter(|v| !v.is_null())),
    );
    if end_line.is_none() {
        end_line = ["end_line", "endLine", "line_end"]
            .iter()
            .find_map(|key| map.get(*key).and_then(Value::as_u64));
    }
    // "src/a.rs:12" or "src/a.rs:12-14" in the file field.
    static SUFFIX: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^(.+?):L?(\d+)(?:[-–]L?(\d+))?(?::\d+)?$").unwrap());
    if let Some(c) = SUFFIX.captures(&file.clone()) {
        file = c[1].to_owned();
        if line.is_none() {
            line = c[2].parse().ok();
            end_line = end_line.or_else(|| c.get(3).and_then(|m| m.as_str().parse().ok()));
        }
    }
    let file = clip(
        file.trim_matches(|c| c == '`' || c == '"' || c == '\''),
        MAX_PATH,
    );
    let severity = [
        "severity",
        "level",
        "priority",
        "importance",
        "impact",
        "type",
        "category",
        "kind",
    ]
    .iter()
    .filter_map(|key| map.get(*key).and_then(Value::as_str))
    .find_map(severity)
    .unwrap_or("medium");
    let mut title = text_of(map, &["title", "headline", "name", "short", "issue_title"]);
    let explanation = text_of(
        map,
        &[
            "explanation",
            "description",
            "message",
            "details",
            "detail",
            "problem",
            "issue",
            "reason",
            "rationale",
            "why",
            "body",
            "comment",
            "text",
        ],
    );
    let suggested_fix = text_of(
        map,
        &[
            "suggested_fix",
            "suggestedFix",
            "suggested-fix",
            "fix",
            "suggestion",
            "recommendation",
            "suggested_change",
            "proposed_fix",
            "remedy",
            "solution",
            "patch",
            "how_to_fix",
        ],
    );
    if explanation.is_empty() && title.is_empty() {
        return None;
    }
    // A finding needs something to point at or something to say beyond a
    // label: a bare `{"severity": "high"}` is not one.
    if file.is_empty() && explanation.is_empty() && suggested_fix.is_empty() {
        return None;
    }
    let explanation = if explanation.is_empty() {
        title.clone()
    } else {
        explanation
    };
    if title.is_empty() || title == explanation {
        title = first_sentence(&explanation);
    }
    Some(Finding {
        file,
        line,
        end_line: end_line.filter(|end| Some(*end) > line),
        severity: severity.into(),
        title: clip(&title, MAX_TITLE),
        explanation: clip(&explanation, MAX_EXPLANATION),
        suggested_fix: clip(&suggested_fix, MAX_FIX),
        ..Default::default()
    })
}

fn first_sentence(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let end = line
        .char_indices()
        .find(|(i, c)| matches!(c, '.' | '!' | '?') && line[i + c.len_utf8()..].starts_with(' '))
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(line.len());
    clip(&line[..end], 120)
}

/// Findings from a Markdown list: `- **High** src/a.rs:12 — text. Fix: …`.
fn markdown_findings(text: &str) -> Vec<Finding> {
    static ITEM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"^\s*(?:[-*•]|\d+[.)])\s+(.*)$").unwrap());
    static SEVERITY: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)[\[(*_]*\b(critical|blocker|high|medium|moderate|major|low|minor|nit|nitpick|info|warning|error|bug)\b[\])*_:]*").unwrap()
    });
    static LOCATION: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"`?((?:[A-Za-z0-9_.@-]+/)*[A-Za-z0-9_.@-]+\.[A-Za-z0-9]{1,8})`?(?:(?::|,? line |,? lines? |#L)(\d+)(?:[-–](\d+))?)?`?").unwrap()
    });
    static FIX: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?i)(?:\*\*)?\b(?:suggested fix|fix|suggestion|recommendation)\b(?:\*\*)?\s*[:—–-]\s*").unwrap()
    });
    let mut out = Vec::new();
    let mut current: Option<Finding> = None;
    for raw in text.lines() {
        if let Some(item) = ITEM.captures(raw) {
            if let Some(done) = current.take() {
                out.push(done);
            }
            let body = item[1].trim().to_owned();
            let head: String = body.chars().take(48).collect();
            let severity_word = SEVERITY
                .captures(&head)
                .and_then(|c| severity(&c[1]).map(str::to_owned));
            let location = LOCATION.captures_iter(&body).find(|c| {
                // A path needs a folder or a line number to count; "e.g." or
                // "v1.2" in prose do not.
                c[1].contains('/') || c.get(2).is_some()
            });
            if severity_word.is_none() && location.is_none() {
                continue;
            }
            let mut rest = body.clone();
            if let Some(c) = SEVERITY.captures(&head) {
                let m = c.get(0).expect("match");
                rest.replace_range(m.start()..m.end(), "");
            }
            let (file, line, end_line) = match &location {
                Some(c) => {
                    let matched = c.get(0).expect("match").as_str().to_owned();
                    rest = rest.replacen(&matched, "", 1);
                    (
                        c[1].to_owned(),
                        c.get(2).and_then(|m| m.as_str().parse().ok()),
                        c.get(3).and_then(|m| m.as_str().parse().ok()),
                    )
                }
                None => (String::new(), None, None),
            };
            let rest = rest
                .trim()
                .trim_start_matches(|c: char| {
                    matches!(c, ':' | '-' | '—' | '–' | '*' | ',' | ' ' | '(' | ')' | '`')
                })
                .trim()
                .to_owned();
            let (explanation, fix) = match FIX.find(&rest) {
                Some(m) if m.start() > 0 => (
                    rest[..m.start()].trim().to_owned(),
                    rest[m.end()..].trim().to_owned(),
                ),
                _ => (rest.clone(), String::new()),
            };
            if explanation.is_empty() {
                continue;
            }
            current = Some(Finding {
                file,
                line,
                end_line,
                severity: severity_word.unwrap_or_else(|| "medium".into()),
                title: first_sentence(&explanation),
                explanation: clip(&explanation, MAX_EXPLANATION),
                suggested_fix: clip(&fix, MAX_FIX),
                ..Default::default()
            });
        } else if let Some(finding) = current.as_mut() {
            // Indented continuation lines belong to the item above.
            let line = raw.trim();
            if line.is_empty() || !raw.starts_with([' ', '\t']) {
                if let Some(done) = current.take() {
                    out.push(done);
                }
                continue;
            }
            match FIX.find(line) {
                Some(m) if m.start() == 0 || finding.suggested_fix.is_empty() => {
                    let fix = line[m.end()..].trim();
                    finding.suggested_fix = clip(fix, MAX_FIX);
                }
                _ if !finding.suggested_fix.is_empty() => {
                    finding.suggested_fix =
                        clip(&format!("{}\n{line}", finding.suggested_fix), MAX_FIX)
                }
                _ => {
                    finding.explanation =
                        clip(&format!("{} {line}", finding.explanation), MAX_EXPLANATION)
                }
            }
        }
    }
    if let Some(done) = current {
        out.push(done);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_clean_json_reply() {
        let parsed = parse(
            r#"{"summary":"One real bug.","findings":[{"file":"src/lib.rs","line":12,"severity":"high","title":"Off by one","explanation":"The loop skips the last item.","suggested_fix":"Use ..= instead of .."}]}"#,
        );
        assert_eq!(parsed.note, "");
        assert_eq!(parsed.summary, "One real bug.");
        assert_eq!(parsed.findings.len(), 1);
        let f = &parsed.findings[0];
        assert_eq!(
            (f.id.as_str(), f.file.as_str(), f.line, f.severity.as_str()),
            ("f1", "src/lib.rs", Some(12), "high")
        );
        assert_eq!(f.title, "Off by one");
        assert_eq!(f.suggested_fix, "Use ..= instead of ..");
        assert_eq!(f.status, "open");
    }

    #[test]
    fn fenced_json_with_prose_reasoning_and_other_key_names() {
        let reply = "<think>Let me look at the diff… {not json}</think>\nHere is my review.\n\n```json\n{\n  \"overview\": \"\",\n  \"issues\": [\n    {\"path\": \"b/app/main.py:40-44\", \"priority\": \"Critical\", \"description\": \"SQL is built from user input. It allows injection.\", \"recommendation\": \"Use a parameterised query.\"},\n    {\"filename\": \"README.md\", \"level\": \"nit\", \"message\": \"Typo in heading\"},\n  ],\n}\n```\nLet me know if you want more detail.";
        let parsed = parse(reply);
        assert_eq!(parsed.note, "");
        assert!(parsed.summary.starts_with("Here is my review."));
        assert!(parsed.summary.ends_with("more detail."));
        assert!(!parsed.summary.contains("think"));
        let f = &parsed.findings;
        assert_eq!(f.len(), 2);
        assert_eq!(f[0].file, "b/app/main.py");
        assert_eq!((f[0].line, f[0].end_line), (Some(40), Some(44)));
        assert_eq!(f[0].severity, "high");
        assert_eq!(f[0].title, "SQL is built from user input.");
        assert_eq!(f[0].suggested_fix, "Use a parameterised query.");
        assert_eq!(f[1].severity, "low");
        assert_eq!(f[1].explanation, "Typo in heading");
    }

    #[test]
    fn a_bare_array_and_string_line_numbers() {
        let parsed = parse(
            r#"[{"file":"a.ts","line":"L7","severity":"medium","explanation":"x may be undefined"},{"file":"b.ts","lines":"3-5","issue":"unused import"}]"#,
        );
        assert_eq!(parsed.findings.len(), 2);
        assert_eq!(parsed.findings[0].line, Some(7));
        assert_eq!(
            (parsed.findings[1].line, parsed.findings[1].end_line),
            (Some(3), Some(5))
        );
        assert_eq!(parsed.findings[1].severity, "medium");
    }

    #[test]
    fn no_findings_is_a_clean_result() {
        let parsed = parse("```json\n{\"summary\": \"Looks correct.\", \"findings\": []}\n```");
        assert!(parsed.findings.is_empty());
        assert_eq!(parsed.summary, "Looks correct.");
        assert_eq!(parsed.note, "");
        let prose = parse("LGTM — no issues found in this change.");
        assert!(prose.findings.is_empty());
        assert_eq!(prose.note, "");
        assert!(prose.summary.contains("LGTM"));
    }

    #[test]
    fn a_reply_cut_off_mid_json_keeps_its_complete_findings() {
        let reply = "Review:\n{\"summary\":\"Two issues\",\"findings\":[{\"file\":\"src/a.rs\",\"line\":3,\"severity\":\"low\",\"explanation\":\"Unused variable\"},{\"file\":\"src/b.rs\",\"line\":9,\"severity\":\"high\",\"explanation\":\"Panics on empty input\",\"suggested_fix\":\"Return early\"},{\"file\":\"src/c.rs\",\"expla";
        let parsed = parse(reply);
        assert_eq!(parsed.findings.len(), 2);
        assert!(parsed.note.contains("cut short"));
        assert_eq!(parsed.summary, "Review:");
        assert_eq!(parsed.findings[1].suggested_fix, "Return early");
    }

    #[test]
    fn a_markdown_list_is_read_as_findings() {
        let reply = "I found a few things:\n\n1. **High** `src/server.rs:88` — The lock is held across an await. Fix: drop the guard before awaiting.\n2. [minor] docs/guide.md line 4: broken link\n   It points to a removed page.\n- Generally nice work.\n";
        let parsed = parse(reply);
        assert!(parsed.note.contains("prose"));
        assert_eq!(parsed.findings.len(), 2, "{:#?}", parsed.findings);
        let f = &parsed.findings;
        assert_eq!((f[0].file.as_str(), f[0].line), ("src/server.rs", Some(88)));
        assert_eq!(f[0].severity, "high");
        assert_eq!(f[0].explanation, "The lock is held across an await.");
        assert_eq!(f[0].suggested_fix, "drop the guard before awaiting.");
        assert_eq!((f[1].file.as_str(), f[1].line), ("docs/guide.md", Some(4)));
        assert_eq!(f[1].severity, "low");
        assert!(f[1].explanation.contains("removed page"));
    }

    #[test]
    fn unreadable_replies_stay_visible_as_text() {
        let parsed = parse("The change seems risky overall but I could not pin it down.");
        assert!(parsed.findings.is_empty());
        assert!(parsed.note.contains("not list findings"));
        assert!(parsed.summary.contains("risky"));
        let empty = parse("  <think>hmm</think>  ");
        assert!(empty.note.contains("empty"));
    }

    #[test]
    fn findings_are_bounded_deduplicated_and_need_substance() {
        let mut items: Vec<String> = (0..80)
            .map(|i| format!(r#"{{"file":"f{i}.rs","line":{i},"explanation":"problem {i}"}}"#))
            .collect();
        items.push(r#"{"file":"f1.rs","line":1,"explanation":"problem 1"}"#.into());
        items.push(r#"{"severity":"high"}"#.into());
        let long = "x".repeat(10_000);
        items.push(format!(r#"{{"file":"g.rs","explanation":"{long}"}}"#));
        let parsed = parse(&format!("{{\"findings\":[{}]}}", items.join(",")));
        assert_eq!(parsed.findings.len(), MAX_FINDINGS);
        assert!(parsed
            .findings
            .iter()
            .all(|f| f.explanation.chars().count() <= MAX_EXPLANATION));
        let ids: Vec<&str> = parsed.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids[0], "f1");
        assert_eq!(ids[49], "f50");
    }

    #[test]
    fn severity_words_map_to_four_levels() {
        assert_eq!(severity("Blocker"), Some("high"));
        assert_eq!(severity("**Major**"), Some("medium"));
        assert_eq!(severity("nit"), Some("low"));
        assert_eq!(severity("Note"), Some("info"));
        assert_eq!(severity("purple"), None);
        // An unknown word falls back to medium.
        let parsed = parse(r#"[{"file":"a.rs","severity":"purple","explanation":"odd"}]"#);
        assert_eq!(parsed.findings[0].severity, "medium");
    }

    #[test]
    fn reasoning_blocks_are_removed_but_unclosed_tags_keep_the_answer() {
        assert_eq!(strip_reasoning("<think>a</think>answer"), "answer");
        assert_eq!(strip_reasoning("<think>answer only"), "answer only");
        assert_eq!(strip_reasoning("hidden</think>visible"), "visible");
    }
}
