//! "Always allow in this project": exact, low-risk commands (tests, builds,
//! linters, type checks) a project may run without asking again.
//!
//! A rule covers one exact command in its normalized form
//! ([`super::assess::normalized`]); changed arguments ask again. Only
//! commands [`super::assess`] offers it for can be added, and each is
//! checked again before it is used, so a rule can never cover a command
//! that deletes, uses the network, rewrites history or leaves the project.
//! Rules are kept per project in ShadowCode's database, never in the
//! repository, and are listed and removed in Settings.
use super::assess;
use crate::store::{keys, Store};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Most rules one project keeps.
pub const MAX_RULES: usize = 100;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    pub command: String,
    pub added_at: f64,
}

#[derive(Default, Serialize, Deserialize)]
struct Rules {
    #[serde(default)]
    commands: Vec<Rule>,
}

pub fn list(store: &Store, workspace: &Path) -> Result<Vec<Rule>> {
    Ok(store
        .native_meta(&keys::always_allow(workspace))?
        .and_then(|text| serde_json::from_str::<Rules>(&text).ok())
        .map(|rules| rules.commands)
        .unwrap_or_default())
}

/// Remember `command` for `workspace`. Refused unless the command may be
/// always allowed exactly as written.
pub fn add(store: &Store, workspace: &Path, command: &str) -> Result<Vec<Rule>> {
    let form = assess::always_allowed_form(command, workspace)
        .context("Only exact test, build and check commands can be always allowed")?;
    store.update_native_json(&keys::always_allow(workspace), |rules: &mut Rules| {
        if !rules.commands.iter().any(|r| r.command == form) {
            rules.commands.push(Rule {
                command: form.clone(),
                added_at: crate::now(),
            });
        }
        if rules.commands.len() > MAX_RULES {
            let extra = rules.commands.len() - MAX_RULES;
            rules.commands.drain(..extra);
        }
        rules.commands.clone()
    })
}

pub fn remove(store: &Store, workspace: &Path, command: &str) -> Result<Vec<Rule>> {
    store.update_native_json(&keys::always_allow(workspace), |rules: &mut Rules| {
        rules.commands.retain(|r| r.command != command);
        rules.commands.clone()
    })
}

/// The rule that covers `command` in `workspace`, if any. The command is
/// assessed again: a rule only ever covers what may still be allowed.
pub fn covering(store: &Store, workspace: &Path, command: &str) -> Result<Option<String>> {
    let Some(form) = assess::always_allowed_form(command, workspace) else {
        return Ok(None);
    };
    Ok(list(store, workspace)?
        .into_iter()
        .any(|rule| rule.command == form)
        .then_some(form))
}

/// The button's words for a command that may be always allowed.
pub fn label(form: &str) -> String {
    format!("Always allow `{form}` in this project")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_offered_commands_are_kept_and_matched_exactly() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("db.sqlite")).unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        assert!(add(&store, &project, "rm -rf build").is_err());
        assert!(add(&store, &project, "cargo test && git push").is_err());
        assert!(add(&store, &project, "cargo test --lib").is_ok());
        assert_eq!(list(&store, &project).unwrap().len(), 1);
        assert_eq!(
            covering(&store, &project, "cargo   test --lib")
                .unwrap()
                .as_deref(),
            Some("cargo test --lib")
        );
        assert_eq!(covering(&store, &project, "cargo test").unwrap(), None);
        assert_eq!(
            covering(&store, &project, "cargo test --lib; rm -rf x").unwrap(),
            None
        );
        // Another project has its own rules.
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        assert_eq!(covering(&store, &other, "cargo test --lib").unwrap(), None);
        assert!(remove(&store, &project, "cargo test --lib")
            .unwrap()
            .is_empty());
        assert_eq!(
            covering(&store, &project, "cargo test --lib").unwrap(),
            None
        );
    }
}
