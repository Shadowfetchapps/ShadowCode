//! Narrow secret redaction before file contents or tool output enter model context.
//! Fake fixture strings only — never commit real secrets.
use regex::Regex;
use serde_json::Value;
use std::sync::OnceLock;

const PLACEHOLDER: &str = "[redacted secret]";

/// Redact the entire payload of recognized private-key armor before the token
/// patterns run. Search only fixed markers, never a greedy body expression.
/// The scan moves forward once with constant parser state; malformed/nested
/// armor hides the uncertain remainder of this input rather than exposing it.
/// No key is decoded or validated. A complete recognized BEGIN marker is needed.
fn redact_private_key_blocks(input: &str) -> Option<(String, usize)> {
    static MARKER: OnceLock<Regex> = OnceLock::new();
    let marker = MARKER.get_or_init(|| {
        Regex::new(r"(?i)-----(BEGIN|END) ((?:RSA |EC |OPENSSH |DSA |ENCRYPTED )?PRIVATE KEY)-----")
            .expect("private key armor marker")
    });
    let mut output = String::new();
    let mut copied = 0;
    let mut open: Option<(usize, &str)> = None;
    let mut count = 0;
    for captures in marker.captures_iter(input) {
        let span = captures.get(0).expect("armor marker");
        let begins = captures
            .get(1)
            .expect("armor boundary")
            .as_str()
            .eq_ignore_ascii_case("BEGIN");
        let kind = captures.get(2).expect("armor type").as_str();
        if let Some((start, expected)) = open {
            if begins || !kind.eq_ignore_ascii_case(expected) {
                // A nested opening or wrong closing cannot establish a safe
                // end for this payload. Do not reinterpret its tail as prose.
                output.push_str(&input[copied..start]);
                output.push_str(PLACEHOLDER);
                return Some((output, count + 1));
            }
            output.push_str(&input[copied..start]);
            output.push_str(PLACEHOLDER);
            count += 1;
            copied = span.end();
            open = None;
        } else if begins {
            open = Some((span.start(), kind));
        }
    }
    if let Some((start, _)) = open {
        output.push_str(&input[copied..start]);
        output.push_str(PLACEHOLDER);
        count += 1;
    } else {
        if count == 0 {
            return None;
        }
        output.push_str(&input[copied..]);
    }
    Some((output, count))
}

fn patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let sources = [
            r"(?i)\bgh[pousr]_[A-Za-z0-9_]{20,}",
            r"(?i)\bxox[baprs]-[A-Za-z0-9-]{10,}",
            r"(?i)\bAKIA[0-9A-Z]{16}\b",
            r"(?i)\bASIA[0-9A-Z]{16}\b",
            r"(?i)\bsk-(?:live|test|proj)?[A-Za-z0-9_-]{16,}",
            r"(?i)\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}",
            r"(?i)\b(?:api[_-]?key|secret[_-]?key|access[_-]?token|auth[_-]?token)\s*[:=]\s*\S{16,}",
            r"(?i)\bBearer\s+[A-Za-z0-9._~+/=-]{20,}",
        ];
        sources
            .into_iter()
            .map(|p| Regex::new(p).expect("redaction regex"))
            .collect()
    })
}

/// High-entropy token heuristic: long base64/hex-like runs outside common words.
fn high_entropy_tokens(text: &str) -> Vec<(usize, usize)> {
    static ENTROPY: OnceLock<Regex> = OnceLock::new();
    let re = ENTROPY.get_or_init(|| Regex::new(r"[A-Za-z0-9+/_=-]{32,}").expect("entropy regex"));
    re.find_iter(text)
        .filter(|m| {
            let s = m.as_str();
            if s.chars().all(|c| c.is_ascii_digit()) {
                return false;
            }
            let classes = [
                s.bytes().any(|b| b.is_ascii_lowercase()),
                s.bytes().any(|b| b.is_ascii_uppercase()),
                s.bytes().any(|b| b.is_ascii_digit()),
                s.bytes()
                    .any(|b| matches!(b, b'+' | b'/' | b'_' | b'=' | b'-')),
            ]
            .into_iter()
            .filter(|v| *v)
            .count();
            classes >= 3 && shannon(s) >= 3.5
        })
        .map(|m| (m.start(), m.end()))
        .collect()
}

fn shannon(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[b as usize] += 1;
    }
    let len = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / len;
            -p * p.log2()
        })
        .sum()
}

pub fn is_secret_path(path: &str) -> bool {
    let name = path
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(path)
        .to_ascii_lowercase();
    // Stored attachments are named `<32-hex id>-<original name>`; judge the
    // original name so an attached `.env` is still recognized.
    let name = match name.split_once('-') {
        Some((id, rest))
            if id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()) && !rest.is_empty() =>
        {
            rest.to_owned()
        }
        _ => name,
    };
    // Committed templates document variable names without values; refusing
    // them would hide the one file a model may legitimately need to read.
    if matches!(
        name.as_str(),
        ".env.example" | ".env.sample" | ".env.template" | ".env.dist" | ".env.defaults"
    ) {
        return false;
    }
    matches!(
        name.as_str(),
        ".env"
            | ".env.local"
            | ".env.development"
            | ".env.production"
            | ".env.test"
            | "secrets.env"
            | ".secrets"
            | "credentials.json"
            | "service-account.json"
    ) || name.starts_with(".env.")
        || name.ends_with(".pem")
        || name.ends_with(".p12")
}

pub struct Redaction {
    pub text: String,
    pub redacted: bool,
    pub count: usize,
}

pub fn redact_text(input: &str) -> Redaction {
    let (mut text, mut count) =
        redact_private_key_blocks(input).unwrap_or_else(|| (input.to_owned(), 0));
    for pattern in patterns() {
        let found = pattern.find_iter(&text).count();
        if found == 0 {
            continue;
        }
        count += found;
        text = pattern.replace_all(&text, PLACEHOLDER).into_owned();
    }
    let mut spans = high_entropy_tokens(&text);
    spans.sort_by_key(|(start, _)| std::cmp::Reverse(*start));
    for (start, end) in spans {
        if text[start..end].contains("redacted") {
            continue;
        }
        text.replace_range(start..end, PLACEHOLDER);
        count += 1;
    }
    Redaction {
        redacted: count > 0,
        count,
        text,
    }
}

pub fn redact_value(value: &mut Value) -> usize {
    match value {
        Value::String(s) => {
            let result = redact_text(s);
            if result.redacted {
                *s = result.text;
                result.count
            } else {
                0
            }
        }
        Value::Array(items) => items.iter_mut().map(redact_value).sum(),
        Value::Object(map) => map.values_mut().map(redact_value).sum(),
        _ => 0,
    }
}

/// The text that replaces a redacted secret.
pub fn placeholder() -> &'static str {
    PLACEHOLDER
}

/// Object keys whose string values are credentials wherever they appear.
const SECRET_KEYS: &[&str] = &[
    "api_key",
    "apikey",
    "password",
    "secret",
    "client_secret",
    "access_token",
    "refresh_token",
    "id_token",
    "authorization",
    "bearer",
    "token",
];

/// Remove recognizable credentials from an API response without the
/// high-entropy heuristic, which would also hide IDs and hashes: values of
/// credential-named keys and text matching known key formats (private keys,
/// GitHub/Slack/AWS/OpenAI-style keys, JWTs, bearer headers).
pub fn redact_known_secrets(value: &mut Value) -> usize {
    match value {
        Value::String(text) => {
            let mut count = 0;
            if let Some((redacted, blocks)) = redact_private_key_blocks(text) {
                *text = redacted;
                count = blocks;
            }
            for pattern in patterns() {
                if pattern.is_match(text) {
                    count += pattern.find_iter(text).count();
                    *text = pattern.replace_all(text, PLACEHOLDER).into_owned();
                }
            }
            count
        }
        Value::Array(items) => items.iter_mut().map(redact_known_secrets).sum(),
        Value::Object(map) => map
            .iter_mut()
            .map(|(key, value)| {
                let named = SECRET_KEYS.contains(&key.to_ascii_lowercase().as_str());
                match value {
                    Value::String(text) if named && !text.is_empty() && text != PLACEHOLDER => {
                        *text = PLACEHOLDER.to_owned();
                        1
                    }
                    _ => redact_known_secrets(value),
                }
            })
            .sum(),
        _ => 0,
    }
}

pub fn secret_file_refusal(path: &str) -> Value {
    serde_json::json!({
        "ok": false,
        "path": path,
        "redacted": true,
        "error": format!(
            "Refusing to send {path} to the model. Secret files (.env, secrets.env, credential JSON, private keys) are blocked; summarize keys present without values if the user needs structure."
        ),
        "note": "Raw secret file contents are never included in model context."
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pem_fixture(kind: &str, body: &str) -> String {
        // Artificial body text: no real key is generated, parsed or stored.
        format!("-----BEGIN {kind}-----\n{body}\n-----END {kind}-----")
    }

    fn assert_pem_redaction(input: &str, expected: &str, count: usize) {
        let redacted = redact_text(input);
        assert_eq!(
            redacted.text.len(),
            expected.len(),
            "redacted output length"
        );
        assert_eq!(redacted.text, expected);
        assert_eq!(redacted.count, count);
        assert_eq!(redacted.redacted, count > 0);
        let again = redact_text(&redacted.text);
        assert_eq!(again.text, expected);
        assert_eq!(again.count, 0);
        assert!(!again.redacted);

        let mut known = serde_json::json!({"diagnostic": [input], "status": 7});
        assert_eq!(redact_known_secrets(&mut known), count);
        assert_eq!(known["diagnostic"][0], expected);
        assert_eq!(known["status"], 7);
        assert_eq!(redact_known_secrets(&mut known), 0);
        assert_eq!(known["diagnostic"][0], expected);

        let mut general = serde_json::json!({"output": input, "ok": true});
        assert_eq!(redact_value(&mut general), count);
        assert_eq!(general["output"], expected);
        assert_eq!(general["ok"], true);
    }

    #[test]
    fn pem_private_bodies_and_footers_are_removed_for_every_supported_container() {
        for kind in [
            "PRIVATE KEY",
            "RSA PRIVATE KEY",
            "EC PRIVATE KEY",
            "OPENSSH PRIVATE KEY",
            "DSA PRIVATE KEY",
            "ENCRYPTED PRIVATE KEY",
        ] {
            // Short/reflowed lines intentionally cannot rely on entropy masking.
            let body = "Zml4dHVyZQ==\nb25seQ==";
            let key = pem_fixture(kind, body);
            let input = format!("diagnostic before\n{key}\nordinary after");
            let expected = format!("diagnostic before\n{PLACEHOLDER}\nordinary after");
            assert_pem_redaction(&input, &expected, 1);
            for chunk in body.lines() {
                assert!(!redact_text(&input).text.contains(chunk));
                let mut known = Value::String(input.clone());
                redact_known_secrets(&mut known);
                assert!(!known.as_str().unwrap().contains(chunk));
            }
            assert!(!redact_text(&input).text.contains("END"));
        }
    }

    #[test]
    fn pem_multiple_blocks_preserve_surrounding_unicode_crlf_and_case_semantics() {
        let first = pem_fixture("RSA PRIVATE KEY", "first-fixture").to_ascii_lowercase();
        let second = pem_fixture("PRIVATE KEY", "second-fixture").replace('\n', "\r\n");
        let input = format!("λ before {first}\n中 between\n{second}\nafter 🦀");
        let expected = format!("λ before {PLACEHOLDER}\n中 between\n{PLACEHOLDER}\nafter 🦀");
        assert_pem_redaction(&input, &expected, 2);
    }

    #[test]
    fn pem_truncated_mismatched_or_nested_armor_hides_the_uncertain_remainder() {
        let start = "-----BEGIN PRIVATE KEY-----";
        for tail in [
            "fixture-body-without-footer",
            "fixture-body\n-----END PRIVATE KEY---",
            "fixture-body\n-----END RSA PRIVATE KEY-----\nuntrusted remainder",
            "fixture-body\n-----BEGIN EC PRIVATE KEY-----\nnested body\n-----END PRIVATE KEY-----\nambiguous remainder",
        ] {
            let input = format!("safe before\n{start}\n{tail}");
            assert_pem_redaction(&input, &format!("safe before\n{PLACEHOLDER}"), 1);
        }
    }

    #[test]
    fn pem_public_armor_and_plain_code_are_not_private_blocks() {
        for kind in ["PUBLIC KEY", "RSA PUBLIC KEY", "CERTIFICATE"] {
            let input = pem_fixture(kind, "Zml4dHVyZQ==\nb25seQ==");
            assert_pem_redaction(&input, &input, 0);
        }
        let code = "fn main() { let begin = \"BEGIN PUBLIC KEY\"; }";
        assert_pem_redaction(code, code, 0);
        // Known-only API preserves IDs/public high-entropy data by design.
        // The general text API's existing entropy heuristic is unchanged.
        let public = pem_fixture("PUBLIC KEY", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/=");
        let mut known = Value::String(public.clone());
        assert_eq!(redact_known_secrets(&mut known), 0);
        assert_eq!(known, public);
    }

    #[test]
    fn pem_large_malformed_input_has_bounded_output_without_recursive_parsing() {
        // A finite hostile body with many near-markers exercises a single
        // forward scan. This is not a wall-clock performance assertion.
        let body = "-----END PRIVATE KEY----\n".repeat(32_768);
        let input = format!("prefix\n-----BEGIN PRIVATE KEY-----\n{body}");
        assert!(input.len() > 700_000);
        assert_pem_redaction(&input, &format!("prefix\n{PLACEHOLDER}"), 1);
        assert!(input.ends_with(&body)); // Caller-owned input was not changed.
    }

    #[test]
    fn pem_known_only_redaction_removes_payload_without_an_entropy_fallback() {
        let body = "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789+/=";
        let key = pem_fixture("PRIVATE KEY", body);
        let mut diagnostic = serde_json::json!({"detail": key});
        assert_eq!(redact_known_secrets(&mut diagnostic), 1);
        assert_eq!(diagnostic["detail"], PLACEHOLDER);
        assert!(!diagnostic.to_string().contains(body));
    }

    #[test]
    fn pem_redaction_preserves_other_credential_patterns_outside_the_block() {
        let key = pem_fixture("EC PRIVATE KEY", "short-body");
        let token = format!("{}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz012345");
        let input = format!("{key}\nGitHub: {token}\nnormal prose");
        assert_pem_redaction(
            &input,
            &format!("{PLACEHOLDER}\nGitHub: {PLACEHOLDER}\nnormal prose"),
            2,
        );
    }

    #[test]
    fn redacts_common_fixture_patterns() {
        // Fixture strings are assembled so scanners do not treat this file as
        // containing live credentials. Patterns still match production regexes.
        let jwt = format!(
            "Authorization: Bearer {}.{}.{}",
            "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9",
            "eyJzdWIiOiIxMjM0In0",
            "signaturepad_fixture_only"
        );
        let aws_key = format!("{}{}", "AKIA", "IOSFODNN7EXAMPLE");
        let slack = format!("{}-{}-{}", "xoxb", "000000000000", "fixturetoken0000");
        let github = format!("{}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz012345");
        let pem = format!(
            "-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----",
            "MIIEvQIBADANBgkqhkiG9w0BAQEFAASC"
        );
        let openai = format!("{}{}", "sk-test", "abcdefghijklmnopqrstuvwxyz0123");
        let samples = [
            jwt,
            format!("AWS_ACCESS_KEY_ID={aws_key}"),
            format!("slack={slack}"),
            format!("github={github}"),
            pem,
            format!("openai={openai}"),
        ];
        for sample in samples {
            let result = redact_text(&sample);
            assert!(result.redacted, "expected redaction in {sample}");
            assert!(result.text.contains(PLACEHOLDER));
        }
        let aws = redact_text(&format!("key={aws_key}"));
        assert!(!aws.text.contains(&aws_key));
        assert!(aws.text.contains(PLACEHOLDER));
    }

    #[test]
    fn redacts_anthropic_and_stripe_fixtures() {
        let anthropic = format!(
            "{}{}",
            "sk-ant-api03-", "abcdefghijklmnopqrstuvwxyz0123456789ABCDEF"
        );
        let stripe = format!("{}{}", "sk_live_", "abcdefghijklmnopqrstuvwxyz012345");
        let npm = format!("{}{}", "npm_", "abcdefghijklmnopqrstuvwxyz0123456789");
        for sample in [anthropic, stripe, npm] {
            let result = redact_text(&sample);
            assert!(result.redacted, "expected redaction in {sample}");
            assert!(result.text.contains(PLACEHOLDER));
            assert!(!result.text.contains("abcdefghijklmnopqrstuvwxyz"));
        }
    }

    #[test]
    fn secret_paths_are_detected() {
        assert!(is_secret_path(".env"));
        assert!(is_secret_path("app/.env.local"));
        assert!(is_secret_path("secrets.env"));
        assert!(is_secret_path("deploy/key.pem"));
        assert!(is_secret_path(".env.production"));
        // Templates with placeholder names are readable; values in them still
        // pass through redact_text like any other content.
        assert!(!is_secret_path(".env.example"));
        assert!(!is_secret_path("app/.env.sample"));
        assert!(!is_secret_path(".env.template"));
        assert!(!is_secret_path("src/config.rs"));
        assert!(!is_secret_path("README.md"));
        // Attachment storage prefix does not hide the original name.
        assert!(is_secret_path(
            ".shadow/attachments/0123456789abcdef0123456789abcdef-.env"
        ));
        assert!(!is_secret_path(
            ".shadow/attachments/0123456789abcdef0123456789abcdef-notes.txt"
        ));
    }

    #[test]
    fn redact_value_reaches_nested_event_payloads() {
        let token = format!("{}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz012345");
        let mut event = serde_json::json!({
            "tool": "exec",
            "output": {"stdout": format!("TOKEN={token}\n"), "exit_code": 0},
            "arguments": {"command": format!("echo {token}")}
        });
        assert_eq!(redact_value(&mut event), 2);
        assert!(!event.to_string().contains(&token));
        assert_eq!(event["output"]["exit_code"], 0);
    }

    #[test]
    fn plain_code_is_mostly_untouched() {
        let code = "fn main() { let x = 42; }";
        let result = redact_text(code);
        assert!(!result.redacted);
        assert_eq!(result.text, code);
    }
}
