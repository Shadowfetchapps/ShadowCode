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
//!
//! Files Git shows as binary (real binary content, or a `-diff` or `binary`
//! attribute) are checked by name, and their content is read from Git when
//! it is text. A check that could not read everything (too large, too many
//! commits) says so in `unchecked`, and the callers refuse as for a finding.
//! These Git processes obey none of the repository's hooks, fsmonitor,
//! signature programs or filter drivers (`crate::git_guard`).
use anyhow::{ensure, Context, Result};
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};
use std::{collections::HashMap, path::Path, sync::OnceLock, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};

/// Most text read from Git for one check (the patch, and the text of files
/// Git showed as binary); the rest is reported as not checked.
const MAX_PATCH_BYTES: usize = 32 * 1024 * 1024;
/// Most commits a push check reads.
const MAX_COMMITS: usize = 500;
/// Longest line scanned (minified files are skipped).
const MAX_LINE_BYTES: usize = 20_000;
/// Most findings reported.
const MAX_FINDINGS: usize = 50;
/// Base64 characters that make a private key block real (a 64-character
/// Ed25519 key is the shortest); shorter blocks are test stand-ins.
const KEY_CHARS: usize = 64;
/// Largest file read to see whether changed lines are inside a private key.
const MAX_KEY_FILE: usize = 1024 * 1024;
/// Why a check is incomplete.
const TOO_LARGE: &str = "Part of the changes is too large to check for secrets (over 32 MB).";
const TOO_MANY_COMMITS: &str =
    "More than 500 commits would be pushed; only the newest 500 were checked for secrets.";
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
    /// There were more findings than are listed.
    pub truncated: bool,
    /// Part of the changes was not checked, in a sentence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unchecked: Option<&'static str>,
    /// Files whose content the patch left out, to read from Git.
    #[serde(skip)]
    followups: Vec<Followup>,
}
impl Scan {
    /// Nothing found, and everything was checked.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty() && self.unchecked.is_none()
    }
    /// The in-band refusal the Git routes answer with.
    pub fn refusal(&self, action: &str) -> Value {
        let count = self.findings.len();
        let mut error = if count == 0 {
            String::new()
        } else {
            format!(
                "{} {} like {} a secret. ",
                count,
                if count == 1 {
                    "change looks"
                } else {
                    "changes look"
                },
                if count == 1 {
                    "it contains"
                } else {
                    "they contain"
                },
            )
        };
        if let Some(reason) = self.unchecked {
            error.push_str(reason);
            error.push(' ');
        }
        error.push_str(&format!("Nothing was {action}."));
        json!({
            "ok": false,
            "status": 409,
            "secrets": self.findings,
            "secrets_truncated": self.truncated,
            "secrets_unchecked": self.unchecked,
            "error": error,
        })
    }
    fn report(&mut self, finding: Finding) {
        if self.findings.len() >= MAX_FINDINGS {
            self.truncated = true;
        } else if !self
            .findings
            .iter()
            .any(|f| f.path == finding.path && f.line == finding.line)
        {
            self.findings.push(finding);
        }
    }
}

/// Content a patch leaves out that the check still needs.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Pending {
    /// Git showed the file as binary.
    Binary,
    /// Added lines (these line numbers) that look like the inside of a
    /// private key whose unchanged BEGIN line the patch leaves out.
    KeyBody(Vec<usize>),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Followup {
    path: String,
    blob: String,
    commit: Option<String>,
    what: Pending,
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

const BEGIN: &str = "-----BEGIN ";
const END: &str = "-----END ";

/// Whether `text` holds a private key block's `BEGIN` or `END` marker
/// (PEM, OpenSSH and PGP armor).
fn key_marker(text: &str, marker: &str) -> bool {
    text.contains(marker)
        && (text.contains("PRIVATE KEY-----") || text.contains("PRIVATE KEY BLOCK-----"))
}

fn base64_chars(text: &str) -> usize {
    text.bytes()
        .filter(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        .count()
}

/// The key data characters of a line inside a key block, or `None` for a
/// line that is not key data.
fn key_data(text: &str) -> Option<usize> {
    let text = text.trim();
    text.bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='))
        .then_some(text.len())
}

/// An armor header inside a key block (`Proc-Type: 4,ENCRYPTED`,
/// `Version: …`).
fn key_header(text: &str) -> bool {
    text.trim().split_once(": ").is_some_and(|(name, _)| {
        !name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

/// A whole key on one line (a JSON string with `\n` escapes): the key data
/// between its markers.
fn inline_key(text: &str) -> Option<usize> {
    let after = text.find(BEGIN)? + BEGIN.len();
    let data = after + text[after..].find("-----")? + 5;
    let end = text[data..].find(END)?;
    Some(base64_chars(&text[data..data + end]))
}

/// Private key blocks in a whole file: first line, last line, key data.
fn key_blocks(text: &str) -> Vec<(usize, usize, usize)> {
    let mut blocks = Vec::new();
    let mut open: Option<(usize, usize)> = None;
    for (index, line) in text.lines().enumerate() {
        let number = index + 1;
        open = match open {
            Some((start, chars)) if key_marker(line, END) => {
                blocks.push((start, number, chars));
                None
            }
            Some((start, chars)) => match key_data(line) {
                Some(count) => Some((start, chars + count)),
                None if key_header(line) => Some((start, chars)),
                None => None,
            },
            None if key_marker(line, BEGIN) && !line.contains(ALLOW_MARKER) => Some((number, 0)),
            None => None,
        };
    }
    blocks
}

/// The path in `diff --git a/P b/P` (renames are off, so both are the same).
/// Quoted names are left to the `+++` line.
fn header_path(rest: &str) -> Option<String> {
    let rest = rest.strip_prefix("a/")?;
    let half = rest.len().checked_sub(3)? / 2;
    let (left, right) = (rest.get(..half)?, rest.get(half..)?);
    (right.strip_prefix(" b/")? == left).then(|| left.to_owned())
}

/// One file of a patch, as the parser reads it.
#[derive(Default)]
struct Section {
    path: String,
    /// Deleted, ignored, or of unknown name: nothing to report.
    skip: bool,
    /// The file's new blob (the `index` line, with `--full-index`).
    blob: Option<String>,
    /// Before the first hunk; `---` and `+++` lines are headers only here.
    header: bool,
    /// Marks in front of a hunk line: one, or one per parent of a merge.
    columns: usize,
    line: usize,
    binary: bool,
    /// The file's name was checked.
    named: bool,
    /// An open private key block: its first line and the key data seen.
    key: Option<(usize, usize)>,
    /// Added lines that look like key data outside any block.
    loose: Vec<usize>,
}

impl Section {
    fn start(path: Option<String>, ignored: &[String]) -> Self {
        let mut section = Section {
            header: true,
            columns: 1,
            ..Section::default()
        };
        section.name(path.unwrap_or_default(), ignored);
        section
    }
    fn name(&mut self, path: String, ignored: &[String]) {
        self.skip = path.is_empty()
            || path == "/dev/null"
            || ignored.iter().any(|glob| glob_matches(glob, &path));
        self.path = path;
    }
    fn finding(
        &self,
        line: Option<usize>,
        kind: &str,
        preview: String,
        commit: &Option<String>,
    ) -> Finding {
        Finding {
            path: self.path.clone(),
            line,
            kind: kind.into(),
            preview,
            commit: commit.clone(),
        }
    }
    fn key_found(&self, scan: &mut Scan, start: usize, chars: usize, commit: &Option<String>) {
        if chars >= KEY_CHARS {
            scan.report(self.finding(
                Some(start),
                "a private key",
                format!("{chars} characters of key data"),
                commit,
            ));
        }
    }
    /// The whole-file check, once the name is final.
    fn check_name(&mut self, scan: &mut Scan, commit: &Option<String>) {
        if self.named || self.skip {
            return;
        }
        self.named = true;
        if let Some(kind) = secret_file(&self.path) {
            scan.report(self.finding(None, kind, "the whole file".into(), commit));
        }
    }
    fn added(&mut self, text: &str, scan: &mut Scan, commit: &Option<String>) {
        let here = self.line;
        self.line += 1;
        if let Some((start, chars)) = self.key {
            if key_marker(text, END) {
                self.key = None;
                self.key_found(scan, start, chars, commit);
                return;
            }
            if let Some(count) = key_data(text) {
                self.key = Some((start, chars + count));
                return;
            }
            if !key_header(text) {
                // Not key data: the block ended without its END line.
                self.key = None;
                self.key_found(scan, start, chars, commit);
            }
        }
        if text.contains(ALLOW_MARKER) {
            return;
        }
        if self.key.is_none() && key_marker(text, BEGIN) {
            match inline_key(text) {
                Some(chars) => self.key_found(scan, here, chars, commit),
                None => self.key = Some((here, 0)),
            }
            return;
        }
        if self.key.is_none()
            && text.trim().len() >= 40
            && key_data(text).is_some()
            && self.loose.len() < 10_000
        {
            self.loose.push(here);
        }
        if let Some((kind, preview)) = scan_line(text) {
            scan.report(self.finding(Some(here), kind, preview, commit));
        }
    }
    /// End of the file: a key block without its END line counts, and the
    /// content the patch left out is noted for [`follow_up`].
    fn finish(&mut self, scan: &mut Scan, commit: &Option<String>) {
        if self.skip {
            return;
        }
        if let Some((start, chars)) = self.key.take() {
            self.key_found(scan, start, chars, commit);
        }
        let Some(blob) = self.blob.clone() else {
            return;
        };
        let followup = |what| Followup {
            path: self.path.clone(),
            blob: blob.clone(),
            commit: commit.clone(),
            what,
        };
        if self.binary {
            scan.followups.push(followup(Pending::Binary));
        }
        if !self.loose.is_empty() {
            scan.followups
                .push(followup(Pending::KeyBody(self.loose.clone())));
        }
    }
}

/// Scan a unified diff (`git diff`, `git log -p`, with `--cc` for merges)
/// for secrets in added lines and added secret files. `commit:<sha>` lines
/// (from `--format`) name the commit of the findings that follow.
pub fn scan_patch(patch: &str, ignored: &[String]) -> Scan {
    let mut scan = Scan::default();
    let mut commit: Option<String> = None;
    let mut file = Section::start(None, ignored);
    for raw in patch.lines() {
        if let Some(sha) = raw.strip_prefix("commit:") {
            file.finish(&mut scan, &commit);
            file = Section::start(None, ignored);
            commit = Some(sha.trim().chars().take(12).collect());
            continue;
        }
        if let Some(rest) = raw.strip_prefix("diff --git ") {
            file.finish(&mut scan, &commit);
            file = Section::start(header_path(rest), ignored);
            continue;
        }
        if let Some(path) = raw
            .strip_prefix("diff --cc ")
            .or_else(|| raw.strip_prefix("diff --combined "))
        {
            file.finish(&mut scan, &commit);
            file = Section::start(Some(path.to_owned()), ignored);
            continue;
        }
        if raw.starts_with("@@") {
            // "@@ -a,b +c,d @@", or "@@@ -a,b -c,d +e,f @@@" for a merge
            // with two parents: the new file's first line.
            file.header = false;
            file.check_name(&mut scan, &commit);
            file.columns = raw.bytes().take_while(|b| *b == b'@').count().max(2) - 1;
            file.line = raw
                .split_whitespace()
                .find_map(|part| part.strip_prefix('+'))
                .and_then(|range| range.split(',').next())
                .and_then(|n| n.parse().ok())
                .unwrap_or(1);
            continue;
        }
        if file.header {
            if let Some(ids) = raw.strip_prefix("index ") {
                // "index <old>..<new> <mode>" (merges: "<old>,<old>..<new>").
                let new = ids
                    .split_whitespace()
                    .next()
                    .and_then(|ids| ids.rsplit_once(".."))
                    .map(|(_, new)| new);
                match new {
                    Some(new) if new.bytes().all(|b| b == b'0') => file.skip = true,
                    Some(new) => file.blob = Some(new.to_owned()),
                    None => {}
                }
            } else if raw.starts_with("deleted file mode") {
                file.skip = true;
            } else if let Some(target) = raw.strip_prefix("+++ ") {
                let target = target.trim();
                let path = target.strip_prefix("b/").unwrap_or(target).to_owned();
                file.name(path, ignored);
            } else if raw.starts_with("Binary files ") {
                file.binary = true;
                file.check_name(&mut scan, &commit);
            }
            continue;
        }
        if file.skip {
            continue;
        }
        let Some(marks) = raw.as_bytes().get(..file.columns) else {
            continue;
        };
        // A line the change adds has `+` for every parent; one taken from
        // a parent of a merge was checked in that parent's commit.
        if marks.iter().all(|m| *m == b'+') {
            file.added(&raw[file.columns..], &mut scan, &commit);
        } else if marks.iter().all(|m| matches!(m, b'+' | b' ')) {
            file.line += 1;
        }
    }
    file.finish(&mut scan, &commit);
    scan
}

/// A blob read for [`follow_up`].
enum Blob {
    Text(String),
    Binary,
    TooLarge,
}

/// A Git command for the secret check, run outside the sandbox: no pager,
/// hooks, fsmonitor, signature programs, external diff or repository
/// filter drivers, and no look inside submodules' working trees.
async fn git_command(workspace: &Path, args: &[&str]) -> Result<tokio::process::Command> {
    let mut command = tokio::process::Command::new("git");
    command
        .args([
            "--no-pager",
            "--no-optional-locks",
            "-c",
            "core.quotepath=false",
            "-c",
            "color.ui=false",
            "-c",
            "diff.external=",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ])
        .args(crate::git_guard::args_async(workspace, args).await?)
        .args(crate::git_guard::harden(args))
        .current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    Ok(command)
}

/// A Git command's output, at most [`MAX_PATCH_BYTES`], and whether it was
/// cut there; `None` when Git failed.
async fn git_text(workspace: &Path, args: &[&str]) -> Result<Option<(String, bool)>> {
    let mut child = git_command(workspace, args)
        .await?
        .spawn()
        .context("Git failed to start for the secret check")?;
    let mut stdout = child.stdout.take().context("Git has no output")?;
    let mut bytes = Vec::new();
    let read = async {
        (&mut stdout)
            .take(MAX_PATCH_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .await?;
        if bytes.len() > MAX_PATCH_BYTES {
            // Enough: Git is stopped when `child` is dropped.
            return anyhow::Ok(true);
        }
        Ok(child.wait().await?.success())
    };
    let done = tokio::time::timeout(Duration::from_secs(60), read)
        .await
        .context("Reading the changes for the secret check timed out")??;
    if !done {
        return Ok(None);
    }
    let cut = bytes.len() > MAX_PATCH_BYTES;
    bytes.truncate(MAX_PATCH_BYTES);
    Ok(Some((String::from_utf8_lossy(&bytes).into_owned(), cut)))
}

/// Read blobs through one `git cat-file --batch`. Text (no NUL byte in the
/// first 8000 bytes, as Git decides) is kept up to each blob's limit and
/// `budget` bytes in all; binary content is skipped.
async fn read_blobs(
    workspace: &Path,
    wanted: &HashMap<String, usize>,
    budget: usize,
) -> Result<HashMap<String, Blob>> {
    let ids: Vec<&String> = wanted.keys().collect();
    let mut child = git_command(workspace, &["cat-file", "--batch"])
        .await?
        .stdin(std::process::Stdio::piped())
        .spawn()
        .context("Git failed to start for the secret check")?;
    let mut input = child.stdin.take().context("Git has no input")?;
    let request: String = ids.iter().map(|id| format!("{id}\n")).collect();
    let writer = tokio::spawn(async move {
        let _ = input.write_all(request.as_bytes()).await;
    });
    let mut reader = tokio::io::BufReader::new(child.stdout.take().context("Git has no output")?);
    let mut blobs = HashMap::new();
    let mut kept = 0usize;
    let read = async {
        for id in &ids {
            // "<id> <type> <size>", or "<id> missing".
            let mut header = String::new();
            if reader.read_line(&mut header).await? == 0 {
                break;
            }
            let mut fields = header.split_whitespace().skip(1);
            let (Some(kind), Some(size)) = (
                fields.next(),
                fields.next().and_then(|s| s.parse::<usize>().ok()),
            ) else {
                continue;
            };
            let head = size.min(8000);
            let mut data = vec![0; head];
            reader.read_exact(&mut data).await?;
            let binary = kind != "blob" || data.contains(&0);
            if !binary && size <= wanted[*id] && kept + size <= budget {
                data.resize(size, 0);
                reader.read_exact(&mut data[head..]).await?;
                kept += size;
                blobs.insert(
                    (*id).clone(),
                    Blob::Text(String::from_utf8_lossy(&data).into_owned()),
                );
            } else {
                tokio::io::copy(
                    &mut (&mut reader).take((size - head) as u64),
                    &mut tokio::io::sink(),
                )
                .await?;
                blobs.insert(
                    (*id).clone(),
                    if binary { Blob::Binary } else { Blob::TooLarge },
                );
            }
            let mut newline = [0u8; 1];
            reader.read_exact(&mut newline).await?;
        }
        anyhow::Ok(())
    };
    tokio::time::timeout(Duration::from_secs(60), read)
        .await
        .context("Reading changed files for the secret check timed out")??;
    let _ = writer.await;
    Ok(blobs)
}

/// Check what the patch left out: files Git showed as binary are read and
/// scanned when they are text, and lines that look like key data are looked
/// up in their whole file to see whether they are inside a private key.
async fn follow_up(
    workspace: &Path,
    scan: &mut Scan,
    budget: usize,
    ignored: &[String],
) -> Result<()> {
    let followups = std::mem::take(&mut scan.followups);
    if followups.is_empty() {
        return Ok(());
    }
    let mut wanted: HashMap<String, usize> = HashMap::new();
    for followup in &followups {
        let limit = match followup.what {
            Pending::Binary => budget,
            Pending::KeyBody(_) => MAX_KEY_FILE.min(budget),
        };
        let entry = wanted.entry(followup.blob.clone()).or_default();
        *entry = (*entry).max(limit);
    }
    let blobs = read_blobs(workspace, &wanted, budget).await?;
    for followup in followups {
        let commit = &followup.commit;
        match (&followup.what, blobs.get(&followup.blob)) {
            (Pending::Binary, Some(Blob::Text(text))) => {
                let mut file = Section::start(Some(followup.path.clone()), ignored);
                file.header = false;
                file.named = true;
                file.line = 1;
                for line in text.lines() {
                    file.added(line, scan, commit);
                }
                file.finish(scan, commit);
            }
            (Pending::Binary, Some(Blob::TooLarge)) => {
                scan.unchecked.get_or_insert(TOO_LARGE);
            }
            (Pending::KeyBody(lines), Some(Blob::Text(text))) => {
                let file = Section::start(Some(followup.path.clone()), ignored);
                for (start, end, chars) in key_blocks(text) {
                    if lines.iter().any(|line| (start..=end).contains(line)) {
                        file.key_found(scan, start, chars, commit);
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// The staged changes of `workspace`.
pub async fn staged(workspace: &Path) -> Result<Scan> {
    let (patch, cut) = git_text(
        workspace,
        &[
            "diff",
            "--cached",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "-U0",
            "--no-textconv",
            "--full-index",
        ],
    )
    .await?
    .context("Could not read the staged changes")?;
    let ignored = ignore_patterns(workspace);
    let mut scan = scan_patch(&patch, &ignored);
    if cut {
        scan.unchecked = Some(TOO_LARGE);
    }
    follow_up(
        workspace,
        &mut scan,
        MAX_PATCH_BYTES.saturating_sub(patch.len()),
        &ignored,
    )
    .await?;
    Ok(scan)
}

/// The commits a push of `commit` to `remote` would send: those on no
/// branch of that remote (as last fetched), whatever the upstream is. A
/// merge commit counts with the changes made in the merge itself (`--cc`).
pub async fn unpushed(workspace: &Path, commit: &str, remote: &str) -> Result<Scan> {
    ensure!(
        commit.len() >= 40 && commit.bytes().all(|b| b.is_ascii_hexdigit()),
        "Not a commit id: {commit}"
    );
    let remotes = format!("--remotes={remote}");
    let most = format!("--max-count={}", MAX_COMMITS + 1);
    let (patch, cut) = git_text(
        workspace,
        &[
            "log",
            "-p",
            "--cc",
            "--no-color",
            "--no-ext-diff",
            "--no-renames",
            "--no-textconv",
            "-U0",
            "--full-index",
            "--format=commit:%H",
            &most,
            commit,
            "--not",
            &remotes,
        ],
    )
    .await?
    .context("Could not read the commits to push")?;
    let ignored = ignore_patterns(workspace);
    let mut scan = scan_patch(&patch, &ignored);
    if cut {
        scan.unchecked = Some(TOO_LARGE);
    } else if patch.lines().filter(|l| l.starts_with("commit:")).count() > MAX_COMMITS {
        scan.unchecked = Some(TOO_MANY_COMMITS);
    }
    follow_up(
        workspace,
        &mut scan,
        MAX_PATCH_BYTES.saturating_sub(patch.len()),
        &ignored,
    )
    .await?;
    Ok(scan)
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

    /// Armor lines, built at run time so no key shape is in the source.
    fn armor(which: &str, kind: &str) -> String {
        format!("-----{which} {kind}-----")
    }
    fn body() -> String {
        "MIIEvQIBADANBgkqhkiG9w0BAQEFAASCBKcwggSjAgEAAoIBAQC7".repeat(2)
    }

    #[test]
    fn binary_files_are_checked_by_name_and_read_afterwards() {
        let text = format!(
            "diff --git a/cert.p12 b/cert.p12\nnew file mode 100644\nindex {zero}..{a}\nBinary files /dev/null and b/cert.p12 differ\n\
diff --git a/conf.json b/conf.json\nnew file mode 100644\nindex {zero}..{b}\nBinary files /dev/null and b/conf.json differ\n\
diff --git a/gone.p12 b/gone.p12\ndeleted file mode 100644\nindex {a}..{zero}\nBinary files a/gone.p12 and /dev/null differ\n",
            zero = "0".repeat(40),
            a = "a".repeat(40),
            b = "b".repeat(40),
        );
        let scan = scan_patch(&text, &[]);
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].path, "cert.p12");
        assert_eq!(scan.findings[0].kind, "a private key file");
        let read: Vec<_> = scan
            .followups
            .iter()
            .map(|f| (f.path.as_str(), f.blob.as_str(), &f.what))
            .collect();
        assert_eq!(
            read,
            [
                ("cert.p12", "a".repeat(40).as_str(), &Pending::Binary),
                ("conf.json", "b".repeat(40).as_str(), &Pending::Binary)
            ]
        );
        let ignored = scan_patch(&text, &["*.p12".into(), "*.json".into()]);
        assert!(ignored.is_clean() && ignored.followups.is_empty());
    }

    #[test]
    fn header_shaped_lines_inside_a_hunk_are_content() {
        let token = github();
        // An added line "++ /dev/null" reads "+++ /dev/null" in the patch.
        let text = patch("a.py", &["++ /dev/null", &format!("T = '{token}'")]);
        let scan = scan_patch(&text, &[]);
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].path, "a.py");
        assert_eq!(scan.findings[0].line, Some(2));
    }

    #[test]
    fn key_blocks_end_where_their_key_data_ends() {
        let token = github();
        let begin = armor("BEGIN", "PRIVATE KEY");
        let end = armor("END", "PRIVATE KEY");
        // A BEGIN line in code hides nothing after it.
        let code = patch(
            "src/pem.ts",
            &[
                &format!("const HEADER = \"{begin}\";"),
                "const x = 1;",
                &format!("const t = '{token}';"),
            ],
        );
        let scan = scan_patch(&code, &[]);
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].kind, "a GitHub token");
        assert_eq!(scan.findings[0].line, Some(3));
        // Key data without its END line still counts.
        let cut = patch("deploy/key.pem", &[&begin, &body(), &body()]);
        assert_eq!(scan_patch(&cut, &[]).findings[0].kind, "a private key");
        // PGP armor, with a header and a blank line.
        let pgp = patch(
            "signing.asc",
            &[
                &armor("BEGIN", "PGP PRIVATE KEY BLOCK"),
                "Version: GnuPG v2",
                "",
                &body(),
                "=abCD",
                &armor("END", "PGP PRIVATE KEY BLOCK"),
            ],
        );
        let found = scan_patch(&pgp, &[]);
        assert_eq!(found.findings.len(), 1, "{found:?}");
        assert_eq!(found.findings[0].kind, "a private key");
        assert_eq!(found.findings[0].line, Some(1));
        // A whole key on one line (a service account's JSON file).
        let json = patch(
            "sa.json",
            &[&format!(
                "  \"private_key\": \"{begin}\\n{}\\n{end}\\n\",",
                body()
            )],
        );
        assert_eq!(scan_patch(&json, &[]).findings[0].kind, "a private key");
        let short = patch("sa.json", &[&format!("\"k\": \"{begin}\\nabc\\n{end}\"")]);
        assert!(scan_patch(&short, &[]).is_clean());
    }

    #[test]
    fn merge_commits_count_only_the_lines_the_merge_adds() {
        let token = github();
        let text = format!(
            "commit:{merge}\n\ndiff --cc f.txt\nindex {a},{b}..{c}\n--- a/f.txt\n+++ b/f.txt\n\
@@@ -2,1 -2,1 +2,2 @@@ one\n- two main\n -two side\n +from side {token}\n++resolved {token}\n",
            merge = "1".repeat(40),
            a = "a".repeat(40),
            b = "b".repeat(40),
            c = "c".repeat(40),
        );
        let scan = scan_patch(&text, &[]);
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].line, Some(3));
        assert_eq!(scan.findings[0].commit.as_deref(), Some("111111111111"));
    }

    #[test]
    fn changed_key_data_alone_is_looked_up_in_the_whole_file() {
        let data = &body()[..64];
        let text = format!(
            "diff --git a/certs/dev.key b/certs/dev.key\nindex {a}..{b} 100644\n--- a/certs/dev.key\n+++ b/certs/dev.key\n@@ -2,2 +2,2 @@\n-{old}\n-{old}\n+{data}\n+{data}\n",
            a = "a".repeat(40),
            b = "b".repeat(40),
            old = "A".repeat(64),
        );
        let scan = scan_patch(&text, &[]);
        assert!(scan.findings.is_empty(), "{scan:?}");
        assert_eq!(scan.followups.len(), 1);
        assert_eq!(scan.followups[0].what, Pending::KeyBody(vec![2, 3]));
        let file = format!(
            "{}\n{data}\n{data}\n{}\n",
            armor("BEGIN", "PRIVATE KEY"),
            armor("END", "PRIVATE KEY")
        );
        assert_eq!(key_blocks(&file), [(1, 4, 128)]);
    }

    #[test]
    fn incomplete_checks_are_refused_and_say_why() {
        let scan = Scan {
            unchecked: Some(TOO_MANY_COMMITS),
            ..Scan::default()
        };
        assert!(!scan.is_clean());
        let refusal = scan.refusal("pushed");
        assert_eq!(refusal["secrets_unchecked"], TOO_MANY_COMMITS);
        assert_eq!(
            refusal["error"],
            format!("{TOO_MANY_COMMITS} Nothing was pushed.")
        );
        assert_eq!(
            Scan::default().refusal("committed")["secrets_unchecked"],
            Value::Null
        );
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Scan",
                "-c",
                "user.email=scan@example.invalid",
                "-c",
                "commit.gpgSign=false",
                "-c",
                "core.hooksPath=/dev/null",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_owned()
    }

    /// A repository with one commit, pushed to a bare `origin`.
    fn repository(root: &Path) -> std::path::PathBuf {
        let remote = root.join("origin.git");
        let repo = root.join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(
            root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                remote.to_str().unwrap(),
            ],
        );
        git(&repo, &["init", "-q", "-b", "main"]);
        std::fs::write(repo.join("f.txt"), "one\ntwo\nthree\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "base"]);
        git(
            &repo,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&repo, &["push", "-q", "origin", "main"]);
        repo
    }

    fn executable(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A sandboxed command can write `.git/config`, and the check runs
    /// outside the sandbox: it must never start a program named there.
    #[tokio::test]
    async fn the_check_never_runs_the_repository_s_fsmonitor_or_signature_program() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        let ran = root.path().join("ran");
        let program = root.path().join("program.sh");
        executable(
            &program,
            &format!("#!/bin/sh\ntouch '{}'\nexit 1\n", ran.display()),
        );
        git(
            &repo,
            &["config", "core.fsmonitor", program.to_str().unwrap()],
        );
        std::fs::write(repo.join("a.txt"), "a\n").unwrap();
        git(&repo, &["-c", "core.fsmonitor=false", "add", "a.txt"]);
        // Control: Git runs it when it reads the index.
        std::process::Command::new("git")
            .args(["diff", "--cached", "-U0"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(ran.exists(), "the setup must reach core.fsmonitor");
        std::fs::remove_file(&ran).unwrap();
        assert!(staged(&repo).await.unwrap().is_clean());
        assert!(!ran.exists(), "the staged check ran core.fsmonitor");

        // A signed commit, with signatures shown by default.
        git(&repo, &["config", "--unset", "core.fsmonitor"]);
        git(&repo, &["config", "log.showSignature", "true"]);
        git(&repo, &["config", "gpg.program", program.to_str().unwrap()]);
        let tree = git(&repo, &["rev-parse", "HEAD^{tree}"]);
        let parent = git(&repo, &["rev-parse", "HEAD"]);
        let object = root.path().join("commit.txt");
        std::fs::write(
            &object,
            format!(
                "tree {tree}\nparent {parent}\nauthor S <s@example.invalid> 1700000000 +0000\ncommitter S <s@example.invalid> 1700000000 +0000\ngpgsig {}\n \n iQEzBAABCAAdFiEE\n {}\n\nsigned\n",
                armor("BEGIN", "PGP SIGNATURE"),
                armor("END", "PGP SIGNATURE")
            ),
        )
        .unwrap();
        let signed = git(
            &repo,
            &[
                "hash-object",
                "-t",
                "commit",
                "-w",
                object.to_str().unwrap(),
            ],
        );
        git(&repo, &["update-ref", "refs/heads/main", &signed]);
        std::process::Command::new("git")
            .args(["log", "-1"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(ran.exists(), "the setup must reach gpg.program");
        std::fs::remove_file(&ran).unwrap();
        assert!(unpushed(&repo, &signed, "origin").await.unwrap().is_clean());
        assert!(!ran.exists(), "the push check ran gpg.program");
    }

    #[tokio::test]
    async fn binary_secret_files_and_files_hidden_by_attributes_are_checked() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        let token = github();
        std::fs::write(repo.join("cert.p12"), b"\x30\x82\x00\x00key container").unwrap();
        std::fs::write(repo.join("logo.png"), vec![0u8; 200_000]).unwrap();
        std::fs::write(repo.join(".gitattributes"), "*.json -diff\n").unwrap();
        std::fs::write(
            repo.join("settings.json"),
            format!("{{\n  \"token\": \"{token}\"\n}}\n"),
        )
        .unwrap();
        git(&repo, &["add", "."]);
        let scan = staged(&repo).await.unwrap();
        let found: Vec<_> = scan
            .findings
            .iter()
            .map(|f| (f.path.as_str(), f.line, f.kind.as_str()))
            .collect();
        assert_eq!(
            found,
            [
                ("cert.p12", None, "a private key file"),
                ("settings.json", Some(2), "a GitHub token")
            ],
            "{scan:?}"
        );
        assert_eq!(scan.unchecked, None);
    }

    #[tokio::test]
    async fn a_new_key_between_unchanged_markers_is_found() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        let block = |kind: &str, data: &str| {
            format!(
                "{}\n{data}\n{data}\n{}\n",
                armor("BEGIN", kind),
                armor("END", kind)
            )
        };
        std::fs::create_dir(repo.join("certs")).unwrap();
        std::fs::write(
            repo.join("certs/dev.key"),
            block("PRIVATE KEY", "placeholder"),
        )
        .unwrap();
        std::fs::write(
            repo.join("certs/dev.crt"),
            block("CERTIFICATE", "placeholder"),
        )
        .unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "placeholders"]);
        let data = &body()[..64];
        std::fs::write(repo.join("certs/dev.key"), block("PRIVATE KEY", data)).unwrap();
        std::fs::write(repo.join("certs/dev.crt"), block("CERTIFICATE", data)).unwrap();
        git(&repo, &["add", "."]);
        let scan = staged(&repo).await.unwrap();
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].path, "certs/dev.key");
        assert_eq!(scan.findings[0].line, Some(1));
        assert_eq!(scan.findings[0].kind, "a private key");
    }

    #[tokio::test]
    async fn pushes_check_every_commit_the_remote_lacks() {
        let token = github();
        // A branch that tracks a local branch: the local branch's commits
        // go out too.
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        std::fs::write(repo.join("local.py"), format!("T = '{token}'\n")).unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "local"]);
        git(&repo, &["switch", "-q", "-c", "feature", "--track", "main"]);
        std::fs::write(repo.join("g.txt"), "feature\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-qm", "feature"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let scan = unpushed(&repo, &head, "origin").await.unwrap();
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].path, "local.py");

        // A secret added while resolving a merge.
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        git(&repo, &["switch", "-q", "-c", "side"]);
        std::fs::write(repo.join("f.txt"), "one\ntwo side\nthree\n").unwrap();
        git(&repo, &["commit", "-qam", "side"]);
        git(&repo, &["switch", "-q", "main"]);
        std::fs::write(repo.join("f.txt"), "one\ntwo main\nthree\n").unwrap();
        git(&repo, &["commit", "-qam", "main"]);
        let merged = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Scan",
                "-c",
                "user.email=scan@example.invalid",
            ])
            .args(["merge", "-q", "side"])
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(!merged.status.success(), "the merge must conflict");
        std::fs::write(
            repo.join("f.txt"),
            format!("one\ntwo resolved {token}\nthree\n"),
        )
        .unwrap();
        git(&repo, &["commit", "-qam", "merge"]);
        let head = git(&repo, &["rev-parse", "HEAD"]);
        let scan = unpushed(&repo, &head, "origin").await.unwrap();
        assert_eq!(scan.findings.len(), 1, "{scan:?}");
        assert_eq!(scan.findings[0].path, "f.txt");
        assert_eq!(scan.findings[0].line, Some(2));
        assert_eq!(scan.findings[0].commit.as_deref(), Some(&head[..12]));
    }

    #[tokio::test]
    async fn checks_that_could_not_read_everything_are_not_clean() {
        let root = tempfile::tempdir().unwrap();
        let repo = repository(root.path());
        // A large binary file is checked by name only.
        std::fs::write(repo.join("model.bin"), vec![0u8; 40 * 1024 * 1024]).unwrap();
        git(&repo, &["add", "model.bin"]);
        assert!(staged(&repo).await.unwrap().is_clean());
        git(&repo, &["commit", "-qm", "model"]);
        // Text over the limit is not passed as clean.
        let line = format!("{}\n", "x".repeat(99));
        std::fs::write(repo.join("big.txt"), line.repeat(340_000)).unwrap();
        git(&repo, &["add", "big.txt"]);
        let scan = staged(&repo).await.unwrap();
        assert_eq!(scan.unchecked, Some(TOO_LARGE), "{scan:?}");
        assert!(!scan.is_clean());
        git(&repo, &["reset", "-q"]);

        // More commits than a push check reads, made in one fast-import.
        let mut stream = String::new();
        for i in 1..=MAX_COMMITS + 1 {
            let from = if i == 1 {
                "refs/remotes/origin/main".to_owned()
            } else {
                format!(":{}", i - 1)
            };
            let content = format!("{i}\n");
            stream.push_str(&format!(
                "commit refs/heads/long\nmark :{i}\ncommitter S <s@example.invalid> 1700000000 +0000\ndata 1\nc\nfrom {from}\nM 100644 inline n.txt\ndata {}\n{content}\n",
                content.len()
            ));
        }
        let mut import = std::process::Command::new("git")
            .args(["fast-import", "--quiet"])
            .current_dir(&repo)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        {
            use std::io::Write;
            import
                .stdin
                .take()
                .unwrap()
                .write_all(stream.as_bytes())
                .unwrap();
        }
        assert!(import.wait().unwrap().success());
        let tip = git(&repo, &["rev-parse", "refs/heads/long"]);
        let scan = unpushed(&repo, &tip, "origin").await.unwrap();
        assert!(scan.findings.is_empty(), "{scan:?}");
        assert_eq!(scan.unchecked, Some(TOO_MANY_COMMITS));
        let fewer = git(&repo, &["rev-parse", "refs/heads/long~1"]);
        assert!(unpushed(&repo, &fewer, "origin").await.unwrap().is_clean());
    }
}
