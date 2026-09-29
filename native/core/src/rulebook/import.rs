//! Import a profile (rules, skills, commands, agents) from a Git repository
//! into `imports/<name>/` inside the profile folder.
//!
//! Only `https://` and SSH (`ssh://`, `user@host:path`) addresses are
//! accepted. Git runs with hooks, symlinks, submodules, fsmonitor and every
//! other transport switched off, prompts disabled and a time limit, so
//! cloning or updating never runs code from the repository. Its files are
//! only ever read as text. A shallow clone lands in a temporary folder and
//! is moved into place once it passes the size limits.
use super::{ensure_profile, valid_import_name, ImportRecord, State, MAX_IMPORTS};
use crate::{
    paths::AppPaths,
    process::{self, ProcessSpec},
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub const CLONE_TIMEOUT: Duration = Duration::from_secs(180);
const INFO_TIMEOUT: Duration = Duration::from_secs(20);
/// A checked-out import may not exceed this size or file count.
pub const MAX_IMPORT_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_IMPORT_FILES: usize = 5000;

/// Refuse anything but `https://…`, `ssh://…` and `user@host:path`.
pub fn validate_url(url: &str) -> Result<()> {
    let refuse = || {
        anyhow::anyhow!("Use an https:// or SSH (git@host:owner/repo) address. Other kinds of address are refused.")
    };
    ensure!(
        !url.is_empty() && url.len() <= 2048,
        "Give the repository address"
    );
    let scheme = url.split_once("://").map(|(scheme, _)| scheme);
    if scheme.is_some_and(|s| s != "https" && s != "ssh")
        || url.starts_with('-')
        || url.contains("::")
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(refuse());
    }
    if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("ssh://"))
    {
        let parsed = reqwest::Url::parse(url).map_err(|_| refuse())?;
        let host = parsed.host_str().unwrap_or("");
        ensure!(
            !host.is_empty() && !host.starts_with('-') && !rest.starts_with('/'),
            "The address has no host"
        );
        if url.starts_with("https://") {
            ensure!(
                parsed.username().is_empty() && parsed.password().is_none(),
                "Leave sign-in details out of the address; Git uses your own credential helper"
            );
        }
        ensure!(
            !parsed.path().trim_matches('/').is_empty(),
            "The address has no repository path"
        );
        return Ok(());
    }
    // scp-like SSH: [user@]host:path, with no scheme and no slash before
    // the first colon.
    let (before, path) = url.split_once(':').ok_or_else(refuse)?;
    let host = before.rsplit('@').next().unwrap_or("");
    let user_ok = before.split_once('@').is_none_or(|(user, _)| {
        !user.is_empty()
            && user
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    });
    if before.contains('/')
        || host.is_empty()
        || host.starts_with('-')
        || !host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.'))
        || !user_ok
        || path.is_empty()
        || path.starts_with('-')
        || path.starts_with(':')
    {
        return Err(refuse());
    }
    Ok(())
}

/// A folder name for the repository: its last path part without `.git`.
pub fn name_for(url: &str) -> String {
    let tail = url
        .trim_end_matches('/')
        .rsplit(['/', ':'])
        .next()
        .unwrap_or("")
        .trim_end_matches(".git");
    let mut name: String = tail
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_') {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    name = name.trim_matches('-').chars().take(40).collect();
    if name.is_empty() || !valid_import_name(&name) {
        "profile".into()
    } else {
        name
    }
}

/// Git's own settings for every import command: nothing from the
/// repository runs, and only HTTPS and SSH transports are allowed.
fn hardening(allow_file: bool) -> Vec<String> {
    let mut args: Vec<String> = [
        "--no-pager",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.symlinks=false",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "submodule.recurse=false",
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.ssh.allow=always",
        "-c",
        "transfer.fsckObjects=true",
        "-c",
        "advice.detachedHead=false",
        "-c",
        "color.ui=false",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if allow_file {
        // Tests clone a local bare repository.
        args.extend(["-c".into(), "protocol.file.allow=always".into()]);
    }
    args
}

fn transport_env() -> BTreeMap<String, String> {
    const PASS: &[&str] = &[
        "SSH_AUTH_SOCK",
        "SSH_AGENT_PID",
        "GIT_SSH",
        "GIT_SSH_COMMAND",
        "GIT_CONFIG_GLOBAL",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "NO_PROXY",
        "http_proxy",
        "https_proxy",
        "no_proxy",
        "SSL_CERT_FILE",
        "SSL_CERT_DIR",
    ];
    let mut env: BTreeMap<String, String> = PASS
        .iter()
        .filter_map(|name| Some((name.to_string(), std::env::var(name).ok()?)))
        .collect();
    if !env.contains_key("GIT_SSH_COMMAND") && !env.contains_key("GIT_SSH") {
        // Never wait for a password or host-key question.
        env.insert("GIT_SSH_COMMAND".into(), "ssh -o BatchMode=yes".into());
    }
    // Git LFS downloads run the user's own git-lfs; skip them: an import
    // only needs text files.
    env.insert("GIT_LFS_SKIP_SMUDGE".into(), "1".into());
    env
}

async fn git(
    cwd: &Path,
    args: Vec<String>,
    allow_file: bool,
    timeout: Duration,
) -> Result<process::ProcessResult> {
    let mut all = hardening(allow_file);
    all.extend(args);
    let mut spec = ProcessSpec::command("git", &[], cwd.to_owned());
    spec.args = all;
    spec.timeout = timeout;
    spec.env = transport_env();
    process::run(spec, CancellationToken::new(), None).await
}

fn failed(what: &str, result: &process::ProcessResult) -> anyhow::Error {
    if result.timed_out {
        return anyhow::anyhow!(
            "{what} took longer than {} seconds and was stopped",
            CLONE_TIMEOUT.as_secs()
        );
    }
    let detail = crate::redaction::redact_text(result.stderr.trim()).text;
    let lines: Vec<&str> = detail.lines().collect();
    let detail = lines[lines.len().saturating_sub(3)..].join(" ");
    anyhow::anyhow!(
        "{what} failed: {}",
        if detail.is_empty() {
            "Git reported an error".into()
        } else {
            detail
        }
    )
}

/// Walk a checkout (not following links) and refuse one over the limits.
fn check_size(dir: &Path) -> Result<()> {
    let mut bytes = 0u64;
    let mut files = 0usize;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        for entry in std::fs::read_dir(&current)? {
            let entry = entry?;
            let kind = entry.file_type()?;
            if entry.file_name() == ".git" {
                continue;
            }
            files += 1;
            if kind.is_dir() {
                stack.push(entry.path());
            } else {
                bytes += entry.metadata()?.len();
            }
            ensure!(
                files <= MAX_IMPORT_FILES && bytes <= MAX_IMPORT_BYTES,
                "The repository is too large for a profile (limit {} MB and {MAX_IMPORT_FILES} files)",
                MAX_IMPORT_BYTES / 1024 / 1024
            );
        }
    }
    Ok(())
}

/// The checked-out commit of an import: `{commit, short, subject, date}`.
pub async fn commit(dir: &Path) -> Value {
    commit_with(dir, false).await
}

async fn commit_with(dir: &Path, allow_file: bool) -> Value {
    match git(
        dir,
        vec!["log".into(), "-1".into(), "--format=%H%x00%s%x00%cI".into()],
        allow_file,
        INFO_TIMEOUT,
    )
    .await
    {
        Ok(result) if result.ok => {
            let line = result.stdout.trim();
            let mut parts = line.splitn(3, '\0');
            let hash = parts.next().unwrap_or("").to_owned();
            json!({
                "commit": hash,
                "short": hash.chars().take(10).collect::<String>(),
                "subject": crate::tools::truncate(parts.next().unwrap_or(""), 200),
                "date": parts.next().unwrap_or(""),
            })
        }
        _ => json!({"commit": null, "short": null, "subject": null, "date": null}),
    }
}

/// Clone `url` into `imports/<name>/`. `POST /api/rules/imports`.
pub async fn add(paths: &AppPaths, url: &str) -> Result<Value> {
    add_with(paths, url, false).await
}

pub(crate) async fn add_with(paths: &AppPaths, url: &str, allow_file: bool) -> Result<Value> {
    let url = url.trim();
    if !allow_file {
        validate_url(url)?;
    }
    let profile = ensure_profile(paths)?;
    let imports = profile.join("imports");
    crate::paths::private_directory(&imports)?;
    let existing = std::fs::read_dir(&imports)?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter(|e| valid_import_name(&e.file_name().to_string_lossy()))
        .count();
    ensure!(
        existing < MAX_IMPORTS,
        "At most {MAX_IMPORTS} profiles can be imported; remove one first"
    );
    let state = State::load(paths)?;
    ensure!(
        !state.imports.values().any(|r| r.url == url),
        "This repository is already imported; use Update to get its latest version"
    );
    let base = name_for(url);
    let mut name = base.clone();
    let mut n = 2;
    while imports.join(&name).exists() {
        name = format!("{base}-{n}");
        n += 1;
    }
    let incoming = tempfile::Builder::new()
        .prefix(".incoming-")
        .tempdir_in(&imports)?;
    let target = incoming.path().join("checkout");
    let result = git(
        &imports,
        vec![
            "clone".into(),
            "--depth".into(),
            "1".into(),
            "--single-branch".into(),
            "--no-recurse-submodules".into(),
            "--no-tags".into(),
            "--".into(),
            url.into(),
            target.display().to_string(),
        ],
        allow_file,
        CLONE_TIMEOUT,
    )
    .await?;
    if !result.ok {
        return Err(failed("Cloning the repository", &result));
    }
    check_size(&target)?;
    let destination = imports.join(&name);
    std::fs::rename(&target, &destination)
        .with_context(|| format!("Cannot move the import into {}", destination.display()))?;
    drop(incoming);
    let record = ImportRecord {
        url: url.into(),
        added_at: crate::now(),
    };
    let saved = State::update(paths, |state| {
        state.imports.insert(name.clone(), record.clone());
        Ok(())
    });
    if let Err(error) = saved {
        let _ = std::fs::remove_dir_all(&destination);
        return Err(error);
    }
    let commit = commit_with(&destination, allow_file).await;
    Ok(json!({"name": name, "url": url, "path": destination, "commit": commit}))
}

fn import_dir(paths: &AppPaths, name: &str) -> Result<PathBuf> {
    ensure!(valid_import_name(name), "Unknown imported profile");
    let dir = super::profile_dir(paths).join("imports").join(name);
    let meta = std::fs::symlink_metadata(&dir).context("Unknown imported profile")?;
    ensure!(meta.is_dir(), "Unknown imported profile");
    Ok(dir)
}

/// Fetch the latest commit of an import's branch and check it out.
/// Refused when files in the import were changed by hand.
pub async fn update(paths: &AppPaths, name: &str) -> Result<Value> {
    update_with(paths, name, false).await
}

pub(crate) async fn update_with(paths: &AppPaths, name: &str, allow_file: bool) -> Result<Value> {
    let dir = import_dir(paths, name)?;
    ensure!(
        dir.join(".git").is_dir(),
        "This import is not a Git checkout; remove it and import again"
    );
    let before = commit_with(&dir, allow_file).await;
    let status = git(
        &dir,
        vec![
            "status".into(),
            "--porcelain".into(),
            "--untracked-files=no".into(),
        ],
        allow_file,
        INFO_TIMEOUT,
    )
    .await?;
    if !status.ok {
        return Err(failed("Reading the import", &status));
    }
    ensure!(
        status.stdout.trim().is_empty(),
        "Files in this import were changed by hand; copy your changes into your own profile files, then remove and import it again"
    );
    let fetch = git(
        &dir,
        vec![
            "fetch".into(),
            "--depth".into(),
            "1".into(),
            "--no-tags".into(),
            "--no-recurse-submodules".into(),
            "origin".into(),
        ],
        allow_file,
        CLONE_TIMEOUT,
    )
    .await?;
    if !fetch.ok {
        return Err(failed("Fetching the latest version", &fetch));
    }
    let reset = git(
        &dir,
        vec![
            "reset".into(),
            "--hard".into(),
            "--no-recurse-submodules".into(),
            "FETCH_HEAD".into(),
        ],
        allow_file,
        INFO_TIMEOUT,
    )
    .await?;
    if !reset.ok {
        return Err(failed("Checking out the latest version", &reset));
    }
    check_size(&dir)?;
    let after = commit_with(&dir, allow_file).await;
    Ok(json!({
        "name": name,
        "changed": before["commit"] != after["commit"],
        "before": before,
        "commit": after,
    }))
}

/// Delete an import's folder and forget it. Its switches stay harmless.
pub fn remove(paths: &AppPaths, name: &str) -> Result<()> {
    let dir = import_dir(paths, name)?;
    std::fs::remove_dir_all(&dir).with_context(|| format!("Cannot remove {}", dir.display()))?;
    State::update(paths, |state| {
        state.imports.remove(name);
        let prefix = format!("profile:imports/{name}/");
        state.disabled.retain(|id| !id.starts_with(&prefix));
        Ok(())
    })
}

/// Imports with their address and checked-out commit.
pub async fn list(paths: &AppPaths) -> Vec<Value> {
    let book = super::Book::load(paths, None);
    let mut out = Vec::new();
    for name in &book.imports {
        let dir = book.dir.join("imports").join(name);
        let record = book.state.imports.get(name).cloned().unwrap_or_default();
        out.push(json!({
            "name": name,
            "url": crate::redaction::redact_text(&record.url).text,
            "path": dir,
            "added_at": record.added_at,
            "commit": commit(&dir).await,
        }));
    }
    out
}
