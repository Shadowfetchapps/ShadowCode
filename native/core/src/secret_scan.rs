//! Secrets in changes about to leave this computer: the staged changes
//! before ShadowCode commits them, and the commits a push would send.
//!
//! Only high-confidence findings count: provider keys with distinctive
//! shapes, private key blocks, environment and secrets files, and long
//! random values assigned to names like `API_KEY` or `PASSWORD`. Values are
//! never returned whole: a finding names the file, the line and the kind,
//! with a short masked preview. A line containing `shadowcode:allow-secret`
//! is skipped, and `.shadowcode/secret-scan-ignore` lists paths (globs, one
//! per line) that are never reported.
use anyhow::{Context, Result};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::{path::Path, sync::OnceLock, time::Duration};

/// Largest patch text scanned; the rest is reported as not scanned.
const MAX_PATCH_BYTES: usize = 32 * 1024 * 1024;
/// Longest line scanned (minified files are skipped).
const MAX_LINE_BYTES: usize = 20_000;
/// Most findings reported.
const MAX_FINDINGS: usize = 50;
/// The marker that allows a line.
pub const ALLOW_MARKER: &str = "shadowcode:allow-secret";
/// Paths that are never reported, one glob per line.
pub const IGNORE_FILE: &str = ".shadowcode/secret-scan-ignore";

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub path: String,
    /// Line in the new file; `None` for a whole file (an added `.env`).
    pub line: Option<usize>,
    /// What it looks like, e.g. "a GitHub token".
    pub kind: String,
    /// The first characters and the length, never the value.
    pub preview: String,
    /// The commit it is in, for a push.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Scan {
    pub findings: Vec<Finding>,
    /// Part of the changes was too large to scan.
    pub truncated: bool,
}
impl Scan {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
    /// The in-band refusal the Git routes answer with.
    pub fn refusal(&self, action: &str) -> Value {
        let count = self.findings.len();
        json!({
            "ok": false,
            "status": 409,
            "secrets": self.findings,
            "secrets_truncated": self.truncated,
            "error": format!(
                "{} {} like {} a secret. Nothing was {action}.",
                count,
                if count == 1 { "change looks" } else { "changes look" },
                if count == 1 { "it contains" } else { "they contain" },
            ),
        })
    }
}

fn patterns() -> &'static [(&'static str, Regex)] {
    static PATTERNS: OnceLock<Vec<(&'static str, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            ("an OpenRouter API key", r"sk-or-v1-[0-9a-f]{48,}"),
            (
                "an Anthropic API key",
                r"sk-ant-(?:api|admin)\d{2}-[A-Za-z0-9_-]{60,}",
            ),
            (
                "an OpenAI API key",
                r"sk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{20}T3BlbkFJ[A-Za-z0-9_-]{20,}",
            ),
            ("an OpenAI API key", r"\bsk-proj-[A-Za-z0-9_-]{80,}"),
            ("a Google API key", r"\bAIza[0-9A-Za-z_-]{35}\b"),
            ("an xAI API key", r"\bxai-[A-Za-z0-9]{60,}"),
            (
                "a GitHub token",
                r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b",
            ),
            ("a GitLab token", r"\bglpat-[A-Za-z0-9_-]{20,}"),
            ("a Slack token", r"\bxox[baprs]-[0-9]{8,}-[0-9A-Za-z-]{10,}"),
            ("a Stripe live key", r"\b(?:sk|rk)_live_[0-9A-Za-z]{24,}"),
            ("an AWS access key", r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b"),
            (
                "an AWS secret key",
                r"(?i)aws_secret_access_key\s*[:=]\s*[A-Za-z0-9/+=]{40}\b",
            ),
            ("a Hugging Face token", r"\bhf_[A-Za-z0-9]{34,}\b"),
            ("an npm token", r"\bnpm_[A-Za-z0-9]{36}\b"),
        ]
        .into_iter()
        .map(|(kind, source)| (kind, Regex::new(source).expect("secret pattern")))
        .collect()
    })
}

fn assignment() -> &'static Regex {
    static ASSIGNMENT: OnceLock<Regex> = OnceLock::new();
    ASSIGNMENT.get_or_init(|| {
        Regex::new(
            r#"(?i)\b([A-Z0-9_.-]*(?:SECRET|TOKEN|PASSWORD|PASSWD|API_?KEY|PRIVATE_?KEY|ACCESS_?KEY)[A-Z0-9_]*)["']?\s*[:=]\s*["']?([A-Za-z0-9+/_=.~-]{20,})"#,
        )
        .expect("assignment pattern")
    })
}

/// A random-looking value (mixed character classes, high entropy) that is
/// not an obvious placeholder.
fn looks_random(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    // Dotted names (`process.env.X`, `settings.auth_token`) are references.
    if value.contains('.') || lower.contains("environ") {
        return false;
    }
    if [
        "example",
        "changeme",
        "xxxx",
        "your",
        "placeholder",
        "dummy",
        "fake",
        "test",
        "sample",
        "redacted",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        return false;
    }
    let classes = [
        value.bytes().any(|b| b.is_ascii_lowercase()),
        value.bytes().any(|b| b.is_ascii_uppercase()),
        value.bytes().any(|b| b.is_ascii_digit()),
    ]
    .into_iter()
    .filter(|v| *v)
    .count();
    classes >= 2 && entropy(value) >= 3.5
}

fn entropy(s: &str) -> f64 {
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

fn mask(value: &str) -> String {
    let shown: String = value.chars().take(6).collect();
    format!("{shown}… ({} characters)", value.chars().count())
}

/// Files that are secrets by their name.
pub fn secret_file(path: &str) -> Option<&'static str> {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let template = [
        ".example",
        ".sample",
        ".template",
        ".dist",
        ".defaults",
        ".schema",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix));
    if (name == ".env" || name.starts_with(".env.")) && !template {
        return Some("an environment file");
    }
    if name == "secrets.env" {
        return Some("a secrets file");
    }
    if matches!(
        name.as_str(),
        "id_rsa" | "id_dsa" | "id_ecdsa" | "id_ed25519"
    ) || name.ends_with(".p12")
        || name.ends_with(".pfx")
        || name.ends_with(".keystore")
        || name.ends_with(".jks")
    {
        return Some("a private key file");
    }
    None
}

/// Findings in the added lines of one line of text.
fn scan_line(text: &str) -> Option<(&'static str, String)> {
    if text.len() > MAX_LINE_BYTES || text.contains(ALLOW_MARKER) {
        return None;
    }
    for (kind, pattern) in patterns() {
        if let Some(found) = pattern.find(text) {
            return Some((kind, mask(found.as_str())));
        }
    }
    for captures in assignment().captures_iter(text) {
        let value = captures.get(2).map_or("", |m| m.as_str());
        if looks_random(value) {
            let name = captures.get(1).map_or("", |m| m.as_str());
            return Some((
                if name.to_ascii_uppercase().contains("PASS") {
                    "a password"
                } else {
                    "a secret value"
                },
                mask(value),
            ));
        }
    }
    None
}

/// Glob patterns from the project's ignore file.
pub fn ignore_patterns(workspace: &Path) -> Vec<String> {
    std::fs::read_to_string(workspace.join(IGNORE_FILE))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .take(500)
        .map(str::to_owned)
        .collect()
}

/// `*` matches within one path segment, `**` across segments; a pattern
/// without `/` matches the file name anywhere.
pub fn glob_matches(pattern: &str, path: &str) -> bool {
    fn matches(p: &[u8], s: &[u8]) -> bool {
        match (p.first(), s.first()) {
            (None, None) => true,
            (Some(b'*'), _) if p.get(1) == Some(&b'*') => {
                let rest = p[2..].strip_prefix(b"/").unwrap_or(&p[2..]);
                (0..=s.len()).any(|i| matches(rest, &s[i..]))
            }
            (Some(b'*'), _) => (0..=s.len())
                .take_while(|&i| i == 0 || s[i - 1] != b'/')
                .any(|i| matches(&p[1..], &s[i..])),
            (Some(b'?'), Some(c)) if *c != b'/' => matches(&p[1..], &s[1..]),
            (Some(a), Some(b)) if a == b => matches(&p[1..], &s[1..]),
            _ => false,
        }
    }
    let pattern = pattern.trim_start_matches("./");
    if pattern.contains('/') {
        matches(pattern.trim_start_matches('/').as_bytes(), path.as_bytes())
    } else {
        let name = path.rsplit('/').next().unwrap_or(path);
        matches(pattern.as_bytes(), name.as_bytes())
    }
}

/// Scan a unified diff (`git diff`, `git log -p`) for secrets in added
/// lines and added secret files. `commit:<sha>` lines (from `--format`)
/// name the commit of the findings that follow.
pub fn scan_patch(patch: &str, ignored: &[String]) -> Scan {
    let mut scan = Scan::default();
    let patch = if patch.len() > MAX_PATCH_BYTES {
        scan.truncated = true;
        let mut end = MAX_PATCH_BYTES;
        while !patch.is_char_boundary(end) {
            end -= 1;
        }
        &patch[..end]
    } else {
        patch
    };
    let mut path = String::new();
    let mut skip = false;
    let mut line_no = 0usize;
    let mut commit: Option<String> = None;
    // A private key block: its start line and the base64 characters seen.
    let mut key: Option<(usize, usize)> = None;
    let push = |scan: &mut Scan, finding: Finding| {
        if scan.findings.len() >= MAX_FINDINGS {
            scan.truncated = true;
        } else if !scan
            .findings
            .iter()
            .any(|f| f.path == finding.path && f.line == finding.line)
        {
            scan.findings.push(finding);
        }
    };
    for raw in patch.lines() {
        if let Some(sha) = raw.strip_prefix("commit:") {
            commit = Some(sha.trim().chars().take(12).collect());
            continue;
        }
        if raw.starts_with("diff --git ") {
            key = None;
            continue;
        }
        if let Some(target) = raw.strip_prefix("+++ ") {
            let target = target.trim();
            path = target.strip_prefix("b/").unwrap_or(target).to_owned();
            skip = path == "/dev/null" || ignored.iter().any(|g| glob_matches(g, &path));
            if !skip {
                if let Some(kind) = secret_file(&path) {
                    push(
                        &mut scan,
                        Finding {
                            path: path.clone(),
                            line: None,
                            kind: kind.into(),
                            preview: "the whole file".into(),
                            commit: commit.clone(),
                        },
                    );
                }
            }
            continue;
        }
        if raw.starts_with("--- ") {
            continue;
        }
        if let Some(header) = raw.strip_prefix("@@ ") {
            // "@@ -a,b +c,d @@": the new file's first line.
            line_no = header
                .split_whitespace()
                .find_map(|part| part.strip_prefix('+'))
                .and_then(|range| range.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            continue;
        }
        if skip || path.is_empty() {
            continue;
        }
        if let Some(added) = raw.strip_prefix('+') {
            let here = line_no;
            line_no += 1;
            if let Some((start, chars)) = key.as_mut() {
                if added.contains("PRIVATE KEY-----") && added.contains("END") {
                    if *chars >= 64 {
                        let start = *start;
                        push(
                            &mut scan,
                            Finding {
                                path: path.clone(),
                                line: Some(start),
                                kind: "a private key".into(),
                                preview: format!("{} characters of key data", chars),
                                commit: commit.clone(),
                            },
                        );
                    }
                    key = None;
                } else {
                    *chars += added
                        .chars()
                        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
                        .count();
                }
                continue;
            }
            if added.contains("-----BEGIN")
                && added.contains("PRIVATE KEY-----")
                && !added.contains(ALLOW_MARKER)
            {
                key = Some((here, 0));
                continue;
            }
            if let Some((kind, preview)) = scan_line(added) {
                push(
                    &mut scan,
                    Finding {
                        path: path.clone(),
                        line: Some(here),
                        kind: kind.into(),
                        preview,
                        commit: commit.clone(),
                    },
                );
            }
        } else if !raw.starts_with('-') && !raw.starts_with('\\') {
            line_no += 1;
        }
    }
    scan
}

async fn git_text(workspace: &Path, args: &[&str]) -> Result<Option<String>> {
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "--no-pager",
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=false",
            "-c",
            "diff.external=",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(60), command.output())
        .await
        .context("Reading the changes for the secret check timed out")??;
    if !output.status.success() {
        return Ok(None);
    }
    Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()))
}

/// The staged changes of `workspace`.
pub async fn staged(workspace: &Path) -> Result<Scan> {
    let patch = git_text(
        workspace,
        &[
            "diff",
            "--cached",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "-U0",
            "--no-textconv",
        ],
    )
    .await?
    .context("Could not read the staged changes")?;
    Ok(scan_patch(&patch, &ignore_patterns(workspace)))
}

/// The commits `git push` would send from the current branch to `remote`:
/// those after the upstream, or, without one, those on no branch of the
/// remote.
pub async fn unpushed(workspace: &Path, remote: &str) -> Result<Scan> {
    let upstream = git_text(workspace, &["rev-parse", "--verify", "-q", "@{upstream}"])
        .await?
        .filter(|s| !s.trim().is_empty());
    let remotes = format!("--remotes={remote}");
    let mut args = vec![
        "log",
        "-p",
        "--no-color",
        "--no-ext-diff",
        "--no-renames",
        "--no-textconv",
        "-U0",
        "--format=commit:%H",
        "--max-count=500",
    ];
    let range;
    if upstream.is_some() {
        range = "@{upstream}..HEAD".to_owned();
        args.push(&range);
    } else {
        args.extend(["HEAD", "--not", &remotes]);
    }
    let patch = git_text(workspace, &args)
        .await?
        .context("Could not read the commits to push")?;
    Ok(scan_patch(&patch, &ignore_patterns(workspace)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn github() -> String {
        format!("ghp_{}", "aB3dE5fG7hI9jK1lM3nO5pQ7rS9tU1vW3xY5")
    }
    fn openrouter() -> String {
        format!("sk-or-v1-{}", "0123456789abcdef".repeat(4))
    }
    fn patch(path: &str, lines: &[&str]) -> String {
        let mut text = format!(
            "diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n@@ -0,0 +1,{} @@\n",
            lines.len()
        );
        for line in lines {
            text.push('+');
            text.push_str(line);
            text.push('\n');
        }
        text
    }

    #[test]
    fn provider_keys_are_found_with_their_line_and_masked() {
        let token = github();
        let key = openrouter();
        let text = patch(
            "src/config.py",
            &[
                "x = 1",
                &format!("TOKEN = \"{token}\""),
                &format!("key: {key}"),
            ],
        );
        let scan = scan_patch(&text, &[]);
        assert_eq!(scan.findings.len(), 2, "{scan:?}");
        assert_eq!(scan.findings[0].line, Some(2));
        assert_eq!(scan.findings[0].kind, "a GitHub token");
        assert!(!scan.findings[0].preview.contains(&token));
        assert_eq!(scan.findings[1].kind, "an OpenRouter API key");
        let json = serde_json::to_string(&scan).unwrap();
        assert!(!json.contains(&token) && !json.contains(&key));
    }

    #[test]
    fn env_files_private_keys_and_random_assignments() {
        let scan = scan_patch(&patch(".env", &["A=1"]), &[]);
        assert_eq!(scan.findings[0].kind, "an environment file");
        assert!(scan_patch(&patch(".env.example", &["A=1"]), &[]).is_clean());
        let body = "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7".repeat(2);
        let key = patch(
            "deploy/key.pem",
            &[
                "-----BEGIN PRIVATE KEY-----",
                &body,
                "-----END PRIVATE KEY-----",
            ],
        );
        assert_eq!(scan_patch(&key, &[]).findings[0].kind, "a private key");
        let short = patch(
            "tests/key.pem",
            &[
                "-----BEGIN PRIVATE KEY-----",
                "abc",
                "-----END PRIVATE KEY-----",
            ],
        );
        assert!(scan_patch(&short, &[]).is_clean());
        let assigned = patch(
            "app.js",
            &["const API_KEY = 'Zq8vN2kLp4Rt7Wx9Yb3Mc6Df1Gh5Jk0Q';"],
        );
        assert_eq!(
            scan_patch(&assigned, &[]).findings[0].kind,
            "a secret value"
        );
        let password = patch("app.env.ts", &["DB_PASSWORD=\"Zq8vN2kLp4Rt7Wx9Yb3Mc6\""]);
        assert_eq!(scan_patch(&password, &[]).findings[0].kind, "a password");
    }

    #[test]
    fn placeholders_allowed_lines_and_ignored_paths_are_not_reported() {
        for line in [
            "API_KEY = 'your-api-key-goes-here-please'",
            "TOKEN = process.env.GITHUB_TOKEN_FOR_THE_BUILD",
            "password: changeme-changeme-changeme",
            "SECRET_KEY = 'aaaaaaaaaaaaaaaaaaaaaaaaaaaa'",
            "sha = 'e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855'",
        ] {
            assert!(
                scan_patch(&patch("a.py", &[line]), &[]).is_clean(),
                "{line}"
            );
        }
        let token = github();
        let allowed = patch(
            "a.py",
            &[&format!("T = '{token}'  # shadowcode:allow-secret")],
        );
        assert!(scan_patch(&allowed, &[]).is_clean());
        let text = patch("tests/fixtures/tokens.txt", &[&token]);
        assert!(scan_patch(&text, &["tests/fixtures/**".into()]).is_clean());
        assert!(!scan_patch(&text, &["docs/**".into()]).is_clean());
    }

    #[test]
    fn line_numbers_follow_hunks_and_removed_lines_do_not_count() {
        let token = github();
        let text = format!(
            "diff --git a/a.txt b/a.txt\n--- a/a.txt\n+++ b/a.txt\n@@ -10,2 +10,3 @@\n context\n-{token}\n+plain\n+{token}\n"
        );
        let scan = scan_patch(&text, &[]);
        assert_eq!(scan.findings.len(), 1);
        assert_eq!(scan.findings[0].line, Some(12));
    }

    #[test]
    fn commits_are_named_in_push_scans() {
        let token = github();
        let text = format!("commit:0123456789abcdef0123\n{}", patch("a.txt", &[&token]));
        assert_eq!(
            scan_patch(&text, &[]).findings[0].commit.as_deref(),
            Some("0123456789ab")
        );
    }

    #[test]
    fn globs() {
        assert!(glob_matches("*.pem", "deploy/keys/server.pem"));
        assert!(glob_matches("tests/**", "tests/a/b/c.txt"));
        assert!(glob_matches("tests/*.txt", "tests/a.txt"));
        assert!(!glob_matches("tests/*.txt", "tests/a/b.txt"));
        assert!(glob_matches("fixtures/**/*.json", "fixtures/x/y/z.json"));
    }
}
