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

/// Settings every ShadowCode Git call gets without asking Git first: no
/// signature checks (they run the configured `gpg.program`) and no recursion
/// into submodules (their own configuration is not inspected here).
const STATIC: [&str; 4] = [
    "-c",
    "log.showSignature=false",
    "-c",
    "submodule.recurse=false",
];

/// Subcommands that only read Git's objects, refs or configuration. They
/// never load the index: a command that does (even `write-tree`) re-checks
/// "racily clean" entries right after an index write by hashing the working
/// tree file, which runs the clean filter. Everything else is treated as
/// possibly running a filter driver.
const OBJECT_ONLY: &[&str] = &[
    "rev-parse",
    "commit-tree",
    "update-ref",
    "symbolic-ref",
    "for-each-ref",
    "show-ref",
    "config",
    "merge-base",
    "ls-tree",
    "check-ref-format",
    "var",
    "rev-list",
    "log",
    "show",
    "name-rev",
    "count-objects",
    "branch",
    "remote",
];

/// Whether a Git command line (global options, then the subcommand and its
/// arguments) may move content between the working tree and Git, which runs
/// filter drivers. Unknown shapes count as "may".
pub fn may_run_filters(args: &[&str]) -> bool {
    let mut words = args.iter().copied();
    let mut subcommand = None;
    while let Some(word) = words.next() {
        match word {
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" => {
                words.next();
            }
            w if w.starts_with('-') => {}
            w => {
                subcommand = Some(w);
                break;
            }
        }
    }
    let Some(subcommand) = subcommand else {
        return true;
    };
    let rest: Vec<&str> = words.collect();
    match subcommand {
        // Object content, unless asked to apply filters or textconv.
        "cat-file" => rest
            .iter()
            .any(|a| a.starts_with("--filters") || a.starts_with("--textconv")),
        "worktree" => !matches!(
            rest.first().copied(),
            Some("list" | "prune" | "lock" | "unlock")
        ),
        other => !OBJECT_ONLY.contains(&other),
    }
}

/// `-c` arguments for a ShadowCode Git call in `dir`: the static settings, and,
/// when `git_args` may run filter drivers, one that switches off each driver
/// the repository's own configuration defines. An error means Git must not
/// run in `dir`.
pub fn args(dir: &Path, git_args: &[&str]) -> Result<Vec<String>> {
    let mut out: Vec<String> = STATIC.iter().map(|s| (*s).to_owned()).collect();
    if may_run_filters(git_args) {
        out.extend(filters(dir)?);
    }
    Ok(out)
}

/// [`args`] without blocking the async runtime.
pub async fn args_async(dir: &Path, git_args: &[&str]) -> Result<Vec<String>> {
    let mut out: Vec<String> = STATIC.iter().map(|s| (*s).to_owned()).collect();
    if may_run_filters(git_args) {
        out.extend(filters_async(dir).await?);
    }
    Ok(out)
}

/// The command line with `--ignore-submodules=dirty` after a `status` or
/// `diff` subcommand. Otherwise Git runs `git status` inside each submodule or
/// nested repository to see whether its files changed, and that child Git
/// obeys the nested repository's own configuration, whose filter drivers
/// [`args`] never looks up. Only the command-line option wins over a
/// repository's `submodule.<name>.ignore`; `diff.ignoreSubmodules` does not.
/// Committed submodule changes still show.
pub fn harden(args: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = args.iter().map(|s| (*s).to_owned()).collect();
    let mut index = 0;
    while index < args.len() {
        match args[index] {
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" => index += 2,
            word if word.starts_with('-') => index += 1,
            "status" | "diff"
                if !args
                    .iter()
                    .any(|a| *a == "--no-index" || a.starts_with("--ignore-submodules")) =>
            {
                out.insert(index + 1, "--ignore-submodules=dirty".into());
                return out;
            }
            _ => return out,
        }
    }
    out
}

/// Mark every gitlink (a submodule or nested repository) in an index as
/// `assume-unchanged`, so a following `git add` does not run `git status`
/// inside it: `add` always checks submodules for changes, no setting turns
/// that off, and the child Git obeys the nested repository's own filter
/// drivers. The entries keep their recorded commit. Use it only on a
/// temporary index or a throwaway worktree's index.
pub async fn freeze_gitlinks(dir: &Path, index: Option<&Path>) -> Result<()> {
    let filters = filters_async(dir).await?;
    let run = |args: Vec<&str>| {
        let mut command = tokio::process::Command::new("git");
        command
            .args([
                "--no-pager",
                "--literal-pathspecs",
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "core.fsmonitor=false",
            ])
            .args(STATIC)
            .args(&filters)
            .args(&args)
            .current_dir(dir)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        for name in REDIRECTS {
            command.env_remove(name);
        }
        if let Some(index) = index {
            command.env("GIT_INDEX_FILE", index);
        }
        async move {
            let output = command
                .output()
                .await
                .context("Git failed to start while listing submodules")?;
            ensure!(
                output.status.success(),
                "Git could not list this repository's submodules: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
            Ok::<_, anyhow::Error>(output.stdout)
        }
    };
    let listed = run(vec!["ls-files", "-s", "-z"]).await?;
    let gitlinks: Vec<String> = listed
        .split(|b| *b == 0)
        .filter_map(|record| {
            let record = std::str::from_utf8(record).ok()?;
            let (meta, path) = record.split_once('\t')?;
            meta.starts_with("160000 ").then(|| path.to_owned())
        })
        .collect();
    for chunk in gitlinks.chunks(64) {
        let mut args = vec!["update-index", "--assume-unchanged", "--"];
        args.extend(chunk.iter().map(String::as_str));
        run(args).await?;
    }
    Ok(())
}

/// The repository's own configuration files for `dir` (its local and
/// worktree-scope files), or `None` when they cannot be worked out here.
fn local_config_files(dir: &Path) -> Option<Vec<std::path::PathBuf>> {
    let mut current = dir.canonicalize().ok()?;
    let dot_git = loop {
        let candidate = current.join(".git");
        if candidate.symlink_metadata().is_ok() {
            break candidate;
        }
        current = current.parent()?.to_path_buf();
    };
    let git_dir = if dot_git.is_dir() {
        dot_git
    } else {
        let text = std::fs::read_to_string(&dot_git).ok()?;
        let target = text.strip_prefix("gitdir:")?.trim();
        dot_git.parent()?.join(target)
    };
    let common = match std::fs::read_to_string(git_dir.join("commondir")) {
        Ok(text) => git_dir.join(text.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir.clone(),
        Err(_) => return None,
    };
    Some(vec![
        common.join("config"),
        common.join("config.worktree"),
        git_dir.join("config.worktree"),
    ])
}

/// True when the repository's own configuration plainly defines no filter
/// driver: none of its files mentions `filter` or `include` (a driver needs a
/// `[filter …]` section, and an include could pull one in from elsewhere).
/// Anything unclear answers `false`, and the caller asks Git instead.
fn plainly_filterless(dir: &Path) -> bool {
    let Some(files) = local_config_files(dir) else {
        return false;
    };
    files.iter().all(|file| match std::fs::read(file) {
        Ok(bytes) => {
            let lower = bytes.to_ascii_lowercase();
            let mentions = |word: &[u8]| lower.windows(word.len()).any(|w| w == word);
            !mentions(b"filter") && !mentions(b"include")
        }
        Err(error) => error.kind() == std::io::ErrorKind::NotFound,
    })
}

fn filters(dir: &Path) -> Result<Vec<String>> {
    if plainly_filterless(dir) {
        return Ok(Vec::new());
    }
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

async fn filters_async(dir: &Path) -> Result<Vec<String>> {
    if plainly_filterless(dir) {
        return Ok(Vec::new());
    }
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
    let mut args = Vec::new();
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
        assert!(
            !text.contains("log.showSignature"),
            "static flags come from args()"
        );
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
    fn only_content_moving_commands_need_the_filter_lookup() {
        for may in [
            &["add", "-u", "--", "."][..],
            &["status", "--porcelain"],
            &["diff", "--no-ext-diff"],
            &["checkout", "--", "a"],
            &["read-tree", "-u", "-m", "HEAD"],
            &["cat-file", "--filters", "HEAD:a"],
            &["worktree", "add", "x"],
            &["worktree", "remove", "--force", "x"],
            &["-c", "core.quotepath=false", "ls-files", "-z"],
            &["hash-object", "a"],
            &["write-tree"],
            &["read-tree", "HEAD"],
            &["frobnicate"],
            &[],
        ] {
            assert!(may_run_filters(may), "{may:?}");
        }
        for object_only in [
            &["rev-parse", "HEAD"][..],
            &["-c", "user.name=x", "commit-tree", "t", "-m", "m"],
            &["cat-file", "blob", "HEAD:a"],
            &["update-ref", "refs/x", "y"],
            &["log", "-12", "--oneline"],
            &["worktree", "list", "--porcelain"],
        ] {
            assert!(!may_run_filters(object_only), "{object_only:?}");
        }
    }

    /// Every subcommand treated as object-only runs without the filter
    /// lookup, so check each against a repository whose index was just
    /// written (racily clean entries make index readers re-hash files).
    #[test]
    fn object_only_commands_never_run_filters_after_an_index_write() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), b"one\n").unwrap();
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-qm", "base"]);
        let marker = root.path().join("filter-ran");
        let command = format!("touch '{}'; cat", marker.display());
        git(&repo, &["config", "filter.probe.clean", &command]);
        git(&repo, &["config", "filter.probe.smudge", &command]);
        std::fs::write(repo.join(".gitattributes"), b"* filter=probe\n").unwrap();
        std::fs::write(repo.join("a.txt"), b"two\n").unwrap();
        let guard = args(&repo, &["add"]).unwrap();
        let run = |extra: &[String], words: &[&str]| {
            let out = Command::new("git")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "core.fsmonitor=false",
                ])
                .args(extra)
                .args(words)
                .current_dir(&repo)
                .output()
                .unwrap();
            assert!(!marker.exists(), "git {words:?} ran a repository filter");
            out
        };
        // Write the index with the filters off, as the checkpoint does.
        run(&guard, &["add", "-A"]);
        let head = String::from_utf8(run(&[], &["rev-parse", "HEAD"]).stdout).unwrap();
        let head = head.trim();
        for words in [
            &["rev-parse", "--is-inside-work-tree"][..],
            &["rev-parse", "--show-prefix"],
            &["rev-parse", "--path-format=absolute", "--git-path", "index"],
            &[
                "commit-tree",
                "4b825dc642cb6eb9a060e54bf8d69288fbee4904",
                "-m",
                "x",
            ],
            &["update-ref", "refs/probe/x", head],
            &["symbolic-ref", "HEAD"],
            &["for-each-ref", "refs/"],
            &["show-ref"],
            &["config", "--get-regexp", "^filter\\."],
            &["merge-base", head, head],
            &["ls-tree", head],
            &["check-ref-format", "--branch", "main"],
            &["var", "GIT_AUTHOR_IDENT"],
            &["rev-list", "--max-count=1", head],
            &["log", "-1", "--oneline"],
            &["show", &format!("{head}:a.txt")],
            &["name-rev", head],
            &["count-objects"],
            &["branch", "--list"],
            &["remote"],
            &["cat-file", "blob", &format!("{head}:a.txt")],
        ] {
            assert!(!may_run_filters(words), "{words:?} should be object-only");
            let static_only: Vec<String> = STATIC.iter().map(|s| (*s).to_owned()).collect();
            run(&static_only, words);
        }
    }

    #[test]
    fn status_and_diff_stay_out_of_submodule_working_trees() {
        let words = |list: &[&str]| harden(list);
        assert_eq!(
            words(&["status", "--porcelain=v1", "-b"]),
            [
                "status",
                "--ignore-submodules=dirty",
                "--porcelain=v1",
                "-b"
            ]
        );
        assert_eq!(
            words(&["-c", "x=y", "diff", "--numstat", "--"]),
            [
                "-c",
                "x=y",
                "diff",
                "--ignore-submodules=dirty",
                "--numstat",
                "--"
            ]
        );
        // Unchanged: other subcommands, --no-index, an explicit choice.
        assert_eq!(words(&["add", "-u"]), ["add", "-u"]);
        assert_eq!(
            words(&["diff", "--no-index", "--", "/dev/null", "a"]),
            ["diff", "--no-index", "--", "/dev/null", "a"]
        );
        assert_eq!(
            words(&["status", "--ignore-submodules=all"]),
            ["status", "--ignore-submodules=all"]
        );
    }

    /// A nested repository (gitlink) carries its own configuration, which the
    /// filter lookup never sees; Git runs `status` inside it from `status`,
    /// `diff` and `add`. The hardened command lines and a frozen temporary
    /// index keep its filter from running; without them it would run.
    #[tokio::test]
    async fn nested_repository_filters_never_run() {
        let root = tempfile::tempdir().unwrap();
        let outer = root.path().join("outer");
        let inner = outer.join("sub");
        std::fs::create_dir_all(&inner).unwrap();
        git(&outer, &["init", "-q"]);
        std::fs::write(outer.join("top.txt"), b"top\n").unwrap();
        git(&outer, &["add", "top.txt"]);
        git(&inner, &["init", "-q"]);
        std::fs::write(inner.join("f.txt"), b"one\n").unwrap();
        git(&inner, &["add", "f.txt"]);
        git(&inner, &["commit", "-qm", "inner"]);
        let head = Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(&inner)
            .output()
            .unwrap();
        let head = String::from_utf8(head.stdout).unwrap();
        let entry = format!("160000,{},sub", head.trim());
        git(&outer, &["update-index", "--add", "--cacheinfo", &entry]);
        git(&outer, &["commit", "-qm", "outer"]);
        // What a sandboxed command could plant inside the nested repository.
        let marker = root.path().join("nested-filter-ran");
        let command = format!("touch '{}'; cat", marker.display());
        git(&inner, &["config", "filter.nested.clean", &command]);
        std::fs::write(inner.join(".gitattributes"), b"* filter=nested\n").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        std::fs::write(inner.join("f.txt"), b"two\n").unwrap();
        // A repository setting that would override diff.ignoreSubmodules.
        git(&outer, &["config", "submodule.sub.ignore", "none"]);

        let base = [
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "core.fsmonitor=false",
        ];
        let run = |index: Option<&Path>, words: &[&str]| {
            let guard = args(&outer, words).unwrap();
            let mut command = Command::new("git");
            command
                .args(base)
                .args(&guard)
                .args(harden(words))
                .current_dir(&outer);
            if let Some(index) = index {
                command.env("GIT_INDEX_FILE", index);
            }
            command.output().unwrap()
        };
        for words in [&["status", "--porcelain"][..], &["diff", "--no-ext-diff"]] {
            run(None, words);
            assert!(!marker.exists(), "git {words:?} ran the nested filter");
        }
        // `add` on a frozen temporary index.
        let real = outer.join(".git/index");
        let frozen = root.path().join("frozen-index");
        std::fs::copy(&real, &frozen).unwrap();
        freeze_gitlinks(&outer, Some(&frozen)).await.unwrap();
        run(Some(&frozen), &["add", "-u", "--", "."]);
        assert!(!marker.exists(), "add ran the nested filter after freezing");
        let listed = Command::new("git")
            .args(["ls-files", "-s", "sub"])
            .env("GIT_INDEX_FILE", &frozen)
            .current_dir(&outer)
            .output()
            .unwrap();
        assert!(
            String::from_utf8_lossy(&listed.stdout).starts_with("160000 "),
            "the gitlink stays in the snapshot"
        );
        // Control: the same add without freezing does run it.
        let unfrozen = root.path().join("unfrozen-index");
        std::fs::copy(&real, &unfrozen).unwrap();
        run(Some(&unfrozen), &["add", "-u", "--", "."]);
        assert!(
            marker.exists(),
            "the test setup must reach the nested filter"
        );
    }

    #[test]
    fn the_fast_path_only_skips_git_when_no_filter_can_be_defined() {
        let root = tempfile::tempdir().unwrap();
        let repo = root.path().join("repo");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        std::fs::write(repo.join("a.txt"), b"a\n").unwrap();
        git(&repo, &["add", "a.txt"]);
        git(&repo, &["commit", "-qm", "base"]);
        std::fs::create_dir(repo.join("sub")).unwrap();
        assert!(plainly_filterless(&repo));
        assert!(
            plainly_filterless(&repo.join("sub")),
            "found from a subfolder"
        );
        // A linked worktree reads the source's configuration too.
        let linked = root.path().join("linked");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "--detach",
                linked.to_str().unwrap(),
            ],
        );
        assert!(plainly_filterless(&linked));
        let config = repo.join(".git/config");
        let original = std::fs::read_to_string(&config).unwrap();
        for added in [
            "[filter \"x\"]\n\tclean = cat\n",
            "[FILTER \"x\"]\n\tclean = cat\n",
            "[filter.x]\n\tclean = cat\n",
            "[include]\n\tpath = other.cfg\n",
            "[includeIf \"gitdir:/\"]\n\tpath = other.cfg\n",
        ] {
            std::fs::write(&config, format!("{original}{added}")).unwrap();
            assert!(!plainly_filterless(&repo), "{added}");
            assert!(!plainly_filterless(&linked), "worktree: {added}");
        }
        std::fs::write(&config, &original).unwrap();
        // Worktree-scope configuration counts as well.
        std::fs::write(repo.join(".git/config.worktree"), "[filter \"y\"]\n").unwrap();
        assert!(!plainly_filterless(&repo));
        // Outside a repository, ask Git.
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let _ = plainly_filterless(&outside);
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

        let guard = args(&repo, &["status"]).unwrap();
        assert!(
            guard.iter().any(|a| a == "filter.probe.clean="),
            "{guard:?}"
        );
        assert!(guard.iter().any(|a| a == "log.showSignature=false"));
        // Object-database commands need no lookup, only the static settings.
        let cheap = args(&repo, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(cheap.len(), STATIC.len(), "{cheap:?}");
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
