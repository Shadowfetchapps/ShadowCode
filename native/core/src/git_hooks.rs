//! A project's own Git hooks (pre-commit, commit-msg …) for commits
//! ShadowCode makes from the Git tab.
//!
//! ShadowCode's Git commands never run repository hooks by default
//! (`core.hooksPath=/dev/null`): a hook is a program the repository asks to
//! run. When a trusted project has real hooks, the Git tab asks once
//! whether to run them when committing there; the answer is kept per
//! project in ShadowCode's database and can be changed in Settings.
use crate::store::{keys, Store};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Hooks a commit runs.
pub const COMMIT_HOOKS: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Hook {
    pub name: String,
    pub path: String,
    /// The first command the hook runs, for the question (at most 160
    /// characters).
    pub preview: String,
}

#[derive(Default, Serialize, Deserialize)]
struct Preference {
    run: Option<bool>,
    #[serde(default)]
    decided_at: f64,
}

async fn git_line(workspace: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(["--no-pager", "-c", "core.fsmonitor=false"])
        .args(args)
        .current_dir(workspace)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (output.status.success() && !text.is_empty()).then_some(text)
}

/// The folder Git would run hooks from: `core.hooksPath` when set (relative
/// to the top of the working tree), else the repository's `hooks` folder.
async fn hooks_dir(workspace: &Path) -> Option<PathBuf> {
    let top = PathBuf::from(git_line(workspace, &["rev-parse", "--show-toplevel"]).await?);
    if let Some(configured) = git_line(workspace, &["config", "--get", "core.hooksPath"]).await {
        let path = PathBuf::from(&configured);
        return Some(if path.is_absolute() {
            path
        } else {
            top.join(path)
        });
    }
    let hooks = PathBuf::from(
        git_line(
            workspace,
            &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
        )
        .await?,
    );
    Some(hooks)
}

/// The project's hooks among `names` that Git would actually run.
pub async fn found(workspace: &Path, names: &[&str]) -> Vec<Hook> {
    let Some(dir) = hooks_dir(workspace).await else {
        return Vec::new();
    };
    let mut hooks = Vec::new();
    for name in names {
        let path = dir.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        #[cfg(unix)]
        let runnable = {
            use std::os::unix::fs::PermissionsExt;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        };
        #[cfg(not(unix))]
        let runnable = meta.is_file();
        if !runnable {
            continue;
        }
        let text = std::fs::read(&path)
            .map(|bytes| String::from_utf8_lossy(&bytes[..bytes.len().min(16_384)]).into_owned())
            .unwrap_or_default();
        let preview = text
            .lines()
            .map(str::trim)
            .find(|line| {
                !line.is_empty()
                    && !line.starts_with('#')
                    && !line.starts_with("set ")
                    && !line.starts_with(". ")
                    && !line.starts_with("source ")
            })
            .unwrap_or("a script")
            .chars()
            .take(160)
            .collect();
        hooks.push(Hook {
            name: (*name).to_owned(),
            path: path.to_string_lossy().into_owned(),
            preview,
        });
    }
    hooks
}

/// Whether commits from ShadowCode run this project's hooks; `None` until
/// the user answered.
pub fn preference(store: &Store, workspace: &Path) -> Result<Option<bool>> {
    Ok(store
        .native_meta(&keys::git_hooks(workspace))?
        .and_then(|text| serde_json::from_str::<Preference>(&text).ok())
        .and_then(|p| p.run))
}

/// Remember the answer; `None` forgets it (the next commit asks again).
pub fn set_preference(store: &Store, workspace: &Path, run: Option<bool>) -> Result<()> {
    store.update_native_json(&keys::git_hooks(workspace), |p: &mut Preference| {
        p.run = run;
        p.decided_at = crate::now();
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    #[tokio::test]
    async fn only_runnable_hooks_that_git_would_use_are_found() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        assert!(Command::new("git")
            .args(["init", "-q"])
            .current_dir(project)
            .status()
            .unwrap()
            .success());
        let hooks = project.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        std::fs::write(hooks.join("pre-commit.sample"), "#!/bin/sh\nexit 0\n").unwrap();
        assert!(found(project, COMMIT_HOOKS).await.is_empty());
        std::fs::write(
            hooks.join("pre-commit"),
            "#!/bin/sh\n# lint first\nnpx lint-staged\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                hooks.join("pre-commit"),
                std::fs::Permissions::from_mode(0o644),
            )
            .unwrap();
            assert!(
                found(project, COMMIT_HOOKS).await.is_empty(),
                "not executable"
            );
            std::fs::set_permissions(
                hooks.join("pre-commit"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        let hooks_found = found(project, COMMIT_HOOKS).await;
        assert_eq!(hooks_found.len(), 1);
        assert_eq!(hooks_found[0].name, "pre-commit");
        assert_eq!(hooks_found[0].preview, "npx lint-staged");
        // core.hooksPath (husky and others) is followed.
        std::fs::create_dir_all(project.join(".husky")).unwrap();
        std::fs::write(
            project.join(".husky/commit-msg"),
            "#!/bin/sh\nnpx commitlint --edit\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                project.join(".husky/commit-msg"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
        }
        assert!(Command::new("git")
            .args(["config", "core.hooksPath", ".husky"])
            .current_dir(project)
            .status()
            .unwrap()
            .success());
        let names: Vec<_> = found(project, COMMIT_HOOKS)
            .await
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, ["commit-msg"]);
    }
}
