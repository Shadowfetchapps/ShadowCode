//! Optional: use the profile in the Claude Code and Codex CLIs outside
//! ShadowCode. Runs only when the user asks, and only creates symlinks
//! whose names start with `shadowcode-` (or the vendor's global rules file
//! when the user has none). An existing file or folder is never replaced.
//! Every link is recorded (also when a later one fails), and turning the
//! export off removes exactly the links it made, only while they still
//! point into the profile. While an export is on, its links follow the
//! switches: a switched-off rules file or skill loses its link, and runs
//! inside ShadowCode do not send what the vendor already reads through a
//! link (`delivery::plan`).
//!
//! - Claude Code: `~/.claude/rules/shadowcode-profile.md` → profile
//!   `AGENTS.md`; `~/.claude/skills/shadowcode-<skill>` → each profile skill
//!   folder (`CLAUDE_CONFIG_DIR` is respected).
//! - Codex: `~/.codex/AGENTS.md` → profile `AGENTS.md`, only when that file
//!   does not exist; `~/.codex/skills/shadowcode-<skill>` → each profile
//!   skill folder (`CODEX_HOME` is respected).
use super::{Book, ExportLink, State};
use crate::{cli_agent::Vendor, paths::AppPaths};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub const TARGETS: [&str; 2] = ["claude", "codex"];

/// The export target a vendor CLI reads, if it has one.
fn target_of(vendor: Vendor) -> Option<&'static str> {
    match vendor {
        Vendor::Claude => Some("claude"),
        Vendor::Codex => Some("codex"),
        _ => None,
    }
}

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

/// Profile files and skill folders `vendor` reads by itself through an
/// export that is on: its recorded links in the vendor's current folder
/// that still point at them. `homes` is `vendor_home` (a fixture in tests).
pub(crate) fn linked(
    book: &Book,
    vendor: Vendor,
    homes: &dyn Fn(&str) -> Result<PathBuf>,
) -> HashSet<PathBuf> {
    let Some(target) = target_of(vendor) else {
        return HashSet::new();
    };
    let Some(recorded) = book.state.exports.get(target) else {
        return HashSet::new();
    };
    let Ok(home) = homes(target) else {
        return HashSet::new();
    };
    recorded
        .iter()
        .map(|l| (PathBuf::from(&l.link), PathBuf::from(&l.target)))
        .filter(|(link, to)| link.starts_with(&home) && points_to(link, to))
        .map(|(_, to)| to)
        .collect()
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
            // On until the user stops it, even while nothing is linked
            // (every item switched off).
            "enabled": book.state.exports.contains_key(target),
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
    link_in(paths, target, vendor, false)
}

/// Bring every export that is on in step with the profile after a switch
/// (`set_enabled`): a switched-off rules file or skill loses its link, a
/// switched-on one gets it back. `homes` is `vendor_home` (a fixture in
/// tests).
pub(crate) fn sync_in(paths: &AppPaths, homes: &dyn Fn(&str) -> Result<PathBuf>) -> Result<()> {
    let state = State::load(paths)?;
    for target in TARGETS {
        if state.exports.contains_key(target) {
            link_in(paths, target, &homes(target)?, true)?;
        }
    }
    Ok(())
}

/// Make the links `planned` wants for `target` and remove recorded ones it
/// no longer wants (only while they still point into the profile). Every
/// link that exists afterwards is recorded, even when another one failed;
/// the failures are then reported. `stay_on` keeps the export on when
/// nothing is linked.
fn link_in(paths: &AppPaths, target: &str, vendor: &Path, stay_on: bool) -> Result<Value> {
    let book = Book::load(paths, None);
    let profile = super::profile_dir(paths);
    let wanted = planned(&book, target, vendor);
    let recorded = book.state.exports.get(target).cloned().unwrap_or_default();
    let mut created = Vec::new();
    let mut skipped = Vec::new();
    let mut removed = Vec::new();
    let mut failed = Vec::new();
    for link in recorded {
        let (path, to) = (PathBuf::from(&link.link), PathBuf::from(&link.target));
        if wanted.iter().any(|(l, t)| *l == path && *t == to)
            || !(to.starts_with(&profile) && points_to(&path, &to))
        {
            continue;
        }
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(link.link),
            Err(error) => {
                failed.push(format!("Cannot remove {}: {error}", path.display()));
                created.push(link);
            }
        }
    }
    for (link, to) in wanted {
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
        if let Err(error) = make_link(&link, &to) {
            failed.push(format!("{error:#}"));
            continue;
        }
        created.push(ExportLink {
            link: link.display().to_string(),
            target: to.display().to_string(),
        });
    }
    let record = created.clone();
    State::update(paths, |state| {
        if record.is_empty() && !stay_on {
            state.exports.remove(target);
        } else {
            state.exports.insert(target.to_owned(), record);
        }
        Ok(())
    })?;
    ensure!(
        failed.is_empty(),
        "Some links could not be changed: {}. The links that were made are kept, and Stop using removes them.",
        failed.join("; ")
    );
    Ok(json!({"target": target, "created": created, "skipped": skipped, "removed": removed}))
}

fn make_link(link: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("Cannot create {}", parent.display()))?;
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(to, link)
        .with_context(|| format!("Cannot create {}", link.display()))?;
    Ok(())
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
