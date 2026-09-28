//! Repository configuration that ShadowCode's own Git commands never obey.
//!
//! Git runs a program for every path whose `filter` attribute names a driver
//! with a `clean`, `smudge` or `process` command, during `status`, `diff`,
//! `add` and checkouts. The repository's own configuration (`.git/config`,
//! `config.worktree` and files they include) can define such drivers, and a
//! shell command in the project can write that configuration: the sandbox
//! makes the project writable, including `.git`. ShadowCode runs Git itself
//! outside the sandbox (checkpoints after each command, the Changes view,
//! reviews, Compare), so it switches off every filter driver that the
//! repository's own configuration defines. Drivers from your global or system
//! configuration, which a sandboxed command cannot change, and Git LFS's
//! standard commands keep working. Hooks and fsmonitor are switched off at
//! each call site.
//!
//! Git commands that write your repository's real index or history for you
//! (staging, committing, discarding from the Changes view) keep the filters,
//! so a filter such as git-crypt still protects what you commit.
use anyhow::{bail, ensure, Context, Result};
use std::{collections::BTreeSet, path::Path, process::Output};

/// Git LFS's standard filter commands (`git lfs install`), in any scope.
const LFS: [(&str, &str); 3] = [
    ("clean", "git-lfs clean -- %f"),
    ("smudge", "git-lfs smudge -- %f"),
    ("process", "git-lfs filter-process"),
];

/// Configuration scopes a command in the project cannot write.
const TRUSTED_SCOPES: [&str; 3] = ["system", "global", "command"];

/// Variables that would point Git at another repository than `dir`.
const REDIRECTS: [&str; 7] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_NAMESPACE",
    "GIT_CEILING_DIRECTORIES",
];

fn listing(scoped: bool) -> Vec<&'static str> {
    let mut args = vec!["--no-pager", "config"];
    if scoped {
        args.push("--show-scope");
    }
    args.extend(["-z", "--get-regexp", r"^filter\."]);
    args
}

/// `git config` exits 1 when nothing matches and 129 for an unknown option
/// (`--show-scope` needs Git 2.26).
fn outcome(output: &Output) -> Option<Result<&[u8]>> {
    match output.status.code() {
        Some(0) => Some(Ok(&output.stdout)),
        Some(1) => Some(Ok(&[])),
        Some(129) => None,
        _ => Some(Err(anyhow::anyhow!(
            "Git could not read this repository's configuration: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// `-c` arguments that switch off the filter drivers defined by `dir`'s
/// repository configuration. An error means Git must not run in `dir`.
pub fn args(dir: &Path) -> Result<Vec<String>> {
    let run = |scoped: bool| -> Result<Output> {
        let mut command = std::process::Command::new("git");
        command
            .args(listing(scoped))
            .current_dir(dir)
            .stdin(std::process::Stdio::null());
        for name in REDIRECTS {
            command.env_remove(name);
        }
        command
            .output()
            .context("Git failed to start while checking its configuration")
    };
    let output = run(true)?;
    match outcome(&output) {
        Some(listed) => from_listing(listed?, true),
        None => from_listing(outcome(&run(false)?).context("Git is too old")??, false),
    }
}

/// [`args`] without blocking the async runtime.
pub async fn args_async(dir: &Path) -> Result<Vec<String>> {
    let run = |scoped: bool| {
        let mut command = tokio::process::Command::new("git");
        command
            .args(listing(scoped))
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        for name in REDIRECTS {
            command.env_remove(name);
        }
        async move {
            command
                .output()
                .await
                .context("Git failed to start while checking its configuration")
        }
    };
    let output = run(true).await?;
    match outcome(&output) {
        Some(listed) => from_listing(listed?, true),
        None => from_listing(
            outcome(&run(false).await?).context("Git is too old")??,
            false,
        ),
    }
}

/// Parse `git config [--show-scope] -z --get-regexp ^filter\.`: records are
/// `[scope NUL] key LF value NUL` (no LF for a key without a value). Without
/// scopes every driver counts as the repository's own.
pub fn from_listing(listed: &[u8], scoped: bool) -> Result<Vec<String>> {
    let mut fields = listed.split(|b| *b == 0).filter(|f| !f.is_empty());
    let mut drivers = BTreeSet::new();
    while let Some(first) = fields.next() {
        let (scope, record) = if scoped {
            let Some(record) = fields.next() else {
                bail!("Unexpected output from git config");
            };
            (String::from_utf8_lossy(first).into_owned(), record)
        } else {
            (String::new(), first)
        };
        let record = String::from_utf8_lossy(record);
        let (key, value) = record.split_once('\n').unwrap_or((&record, ""));
        let Some((name, variable)) = key
            .strip_prefix("filter.")
            .and_then(|rest| rest.rsplit_once('.'))
        else {
            continue;
        };
        if !matches!(variable, "clean" | "smudge" | "process")
            || TRUSTED_SCOPES.contains(&scope.as_str())
            || LFS.contains(&(variable, value))
        {
            continue;
        }
        // `-c name=value` splits at the first `=`, so such a name cannot be
        // switched off from the command line: refuse instead.
        ensure!(
            !name.contains('=') && !name.is_empty(),
            "This repository's Git configuration defines a filter ShadowCode cannot switch off"
        );
        drivers.insert(name.to_owned());
    }
    let mut args = vec!["-c".to_owned(), "log.showSignature=false".to_owned()];
    for name in drivers {
        for (variable, value) in [
            ("clean", ""),
            ("smudge", ""),
            ("process", ""),
            ("required", "false"),
        ] {
            args.push("-c".into());
            args.push(format!("filter.{name}.{variable}={value}"));
        }
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args([
                "-c",
                "user.name=Guard",
                "-c",
                "user.email=guard@example.invalid",
            ])
            .args(args)
            .current_dir(dir)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn only_the_repository_s_own_drivers_are_switched_off() {
        let listed = b"system\0filter.lfs.process\ngit-lfs filter-process\0\
global\0filter.nb.clean\nnbstripout\0\
local\0filter.lfs.clean\ngit-lfs clean -- %f\0\
local\0filter.x.y.clean\nrun-something\0\
worktree\0filter.w.smudge\nother\0\
local\0filter.r.required\0";
        let args = from_listing(listed, true).unwrap();
        let text = args.join(" ");
        assert!(text.contains("filter.x.y.clean="), "{text}");
        assert!(text.contains("filter.x.y.process="), "{text}");
        assert!(text.contains("filter.x.y.required=false"), "{text}");
        assert!(text.contains("filter.w.smudge="), "{text}");
        assert!(!text.contains("filter.lfs."), "LFS stays on: {text}");
        assert!(
            !text.contains("filter.nb."),
            "global drivers stay on: {text}"
        );
        assert!(!text.contains("filter.r."), "required alone runs nothing");
        assert!(text.contains("log.showSignature=false"));
        // A changed LFS command in the repository is not LFS any more.
        let changed = from_listing(b"local\0filter.lfs.clean\ngit-lfs clean -- %f; x\0", true)
            .unwrap()
            .join(" ");
        assert!(changed.contains("filter.lfs.clean="), "{changed}");
        // Without scopes (old Git) every non-LFS driver is switched off.
        let old = from_listing(b"filter.nb.clean\nnbstripout\0", false)
            .unwrap()
            .join(" ");
        assert!(old.contains("filter.nb.clean="), "{old}");
        // A name the command line cannot express refuses.
        assert!(from_listing(b"local\0filter.a=b.clean\nx\0", true).is_err());
    }

    #[test]
    fn repository_filters_never_run_in_shadowcode_git() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-qm", "base"]);
        // Configuration a command in the project could write.
        let marker = root.path().join("filter-ran");
        let command = format!("touch '{}'; cat", marker.display());
        git(&repo, &["config", "filter.probe.clean", &command]);
        git(&repo, &["config", "filter.probe.smudge", &command]);
        std::fs::write(repo.join(".gitattributes"), b"* filter=probe\n").unwrap();
        // Same size, newer time: Git has to read the file again.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(repo.join("a.txt"), b"two\n").unwrap();

        let guard = args(&repo).unwrap();
        for command in [
            vec!["status", "--porcelain"],
            vec!["diff", "--no-ext-diff", "--no-textconv"],
            vec!["checkout", "--", "a.txt"],
        ] {
            let out = Command::new("git")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                ])
                .args(&guard)
                .args(&command)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(out.status.success(), "{command:?}");
            assert!(!marker.exists(), "git {command:?} ran a repository filter");
        }
    }
}
