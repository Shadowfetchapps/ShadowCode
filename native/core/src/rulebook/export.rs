//! Optional: use the profile in the Claude Code and Codex CLIs outside
//! ShadowCode. Runs only when the user asks, and only creates symlinks
//! whose names start with `shadowcode-` (or the vendor's global rules file
//! when the user has none). An existing file or folder is never replaced.
//! Every link is recorded, and turning the export off removes exactly the
//! links it made, only while they still point into the profile.
//!
//! - Claude Code: `~/.claude/rules/shadowcode-profile.md` → profile
//!   `AGENTS.md`; `~/.claude/skills/shadowcode-<skill>` → each profile skill
//!   folder (`CLAUDE_CONFIG_DIR` is respected).
//! - Codex: `~/.codex/AGENTS.md` → profile `AGENTS.md`, only when that file
//!   does not exist; `~/.codex/skills/shadowcode-<skill>` → each profile
//!   skill folder (`CODEX_HOME` is respected).
use super::{Book, ExportLink, State};
use crate::paths::AppPaths;
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

pub const TARGETS: [&str; 2] = ["claude", "codex"];

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is not set")?,
    ))
}

/// The CLI's own folder: `CLAUDE_CONFIG_DIR` / `CODEX_HOME`, or the default.
pub fn vendor_home(target: &str) -> Result<PathBuf> {
    let (variable, default) = match target {
        "claude" => ("CLAUDE_CONFIG_DIR", ".claude"),
        "codex" => ("CODEX_HOME", ".codex"),
        _ => bail!("Unknown export target {target}"),
    };
    match std::env::var_os(variable)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
    {
        Some(dir) => Ok(dir),
        None => Ok(home()?.join(default)),
    }
}

/// A link the export would make: `(link, target)`.
fn planned(book: &Book, target: &str, vendor: &Path) -> Vec<(PathBuf, PathBuf)> {
    let mut links = Vec::new();
    let rules = book.dir.join("AGENTS.md");
    if rules.is_file() && book.enabled(&super::profile_id("AGENTS.md")) {
        let link = match target {
            "claude" => vendor.join("rules").join("shadowcode-profile.md"),
            _ => vendor.join("AGENTS.md"),
        };
        links.push((link, rules));
    }
    // The user's own skill folders (`skills/<name>/SKILL.md`).
    let skills = book.dir.join("skills");
    if let Ok(entries) = std::fs::read_dir(&skills) {
        let mut names: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| crate::workflows::valid_name(n))
            .collect();
        names.sort();
        for name in names.into_iter().take(128) {
            let dir = skills.join(&name);
            let id = super::profile_id(&format!("skills/{name}/SKILL.md"));
            if dir.join("SKILL.md").is_file() && book.enabled(&id) {
                links.push((
                    vendor.join("skills").join(format!("shadowcode-{name}")),
                    dir,
                ));
            }
        }
    }
    links
}

fn points_to(link: &Path, target: &Path) -> bool {
    std::fs::symlink_metadata(link).is_ok_and(|m| m.file_type().is_symlink())
        && std::fs::read_link(link).is_ok_and(|t| t == target)
}

/// What an export would do and what it has done.
pub fn status(paths: &AppPaths) -> Result<Value> {
    status_in(paths, &vendor_home)
}

pub(crate) fn status_in(
    paths: &AppPaths,
    homes: &dyn Fn(&str) -> Result<PathBuf>,
) -> Result<Value> {
    let book = Book::load(paths, None);
    let mut targets = Vec::new();
    for target in TARGETS {
        let vendor = homes(target)?;
        let recorded = book.state.exports.get(target).cloned().unwrap_or_default();
        let links: Vec<Value> = planned(&book, target, &vendor)
            .into_iter()
            .map(|(link, to)| {
                let state = if points_to(&link, &to) {
                    "linked"
                } else if std::fs::symlink_metadata(&link).is_ok() {
                    "blocked"
                } else {
                    "available"
                };
                json!({"link": link, "target": to, "state": state})
            })
            .collect();
        targets.push(json!({
            "id": target,
            "label": if target == "claude" { "Claude Code" } else { "Codex" },
            "home": vendor,
            "enabled": !recorded.is_empty(),
            "links": links,
            "created": recorded,
        }));
    }
    Ok(json!({"targets": targets}))
}

/// Create the links for `target`. Existing files are skipped and listed.
pub fn enable(paths: &AppPaths, target: &str) -> Result<Value> {
    ensure!(TARGETS.contains(&target), "Unknown export target {target}");
    enable_in(paths, target, &vendor_home(target)?)
}

pub(crate) fn enable_in(paths: &AppPaths, target: &str, vendor: &Path) -> Result<Value> {
    let book = Book::load(paths, None);
    let vendor = vendor.to_path_buf();
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    for (link, to) in planned(&book, target, &vendor) {
        if points_to(&link, &to) {
            created.push(ExportLink {
                link: link.display().to_string(),
                target: to.display().to_string(),
            });
            continue;
        }
        if std::fs::symlink_metadata(&link).is_ok() {
            skipped.push(json!({"link": link, "reason": "Something already exists here; it was left as it is"}));
            continue;
        }
        if let Some(parent) = link.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("Cannot create {}", parent.display()))?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&to, &link)
            .with_context(|| format!("Cannot create {}", link.display()))?;
        created.push(ExportLink {
            link: link.display().to_string(),
            target: to.display().to_string(),
        });
    }
    let record = created.clone();
    State::update(paths, |state| {
        let entry = state.exports.entry(target.to_owned()).or_default();
        for link in record {
            if !entry.contains(&link) {
                entry.push(link);
            }
        }
        if entry.is_empty() {
            state.exports.remove(target);
        }
        Ok(())
    })?;
    Ok(json!({"target": target, "created": created, "skipped": skipped}))
}

/// Remove the links the export made for `target`, only while they are
/// still symlinks into the profile.
pub fn disable(paths: &AppPaths, target: &str) -> Result<Value> {
    ensure!(TARGETS.contains(&target), "Unknown export target {target}");
    let state = State::load(paths)?;
    let profile = super::profile_dir(paths);
    let mut removed = Vec::new();
    let mut kept = Vec::new();
    for link in state.exports.get(target).cloned().unwrap_or_default() {
        let path = PathBuf::from(&link.link);
        let to = PathBuf::from(&link.target);
        if to.starts_with(&profile) && points_to(&path, &to) {
            std::fs::remove_file(&path)
                .with_context(|| format!("Cannot remove {}", path.display()))?;
            removed.push(link.link);
        } else if std::fs::symlink_metadata(&path).is_ok() {
            kept.push(json!({"link": link.link, "reason": "It no longer points into your profile, so it was left alone"}));
        }
    }
    State::update(paths, |state| {
        state.exports.remove(target);
        Ok(())
    })?;
    Ok(json!({"target": target, "removed": removed, "kept": kept}))
}
