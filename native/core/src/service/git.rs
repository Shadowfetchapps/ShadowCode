//! `/api/workspace/git…` and `/api/workspace/diff…`: status, diffs, hunk
//! staging and commits in the selected project. Every Git process runs with
//! hooks, fsmonitor and external diff/textconv drivers disabled.
use super::*;

impl Service {
    pub(super) async fn git_in(
        workspace: &Path,
        args: Vec<String>,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let mut base = vec![
            "--no-pager".into(),
            "--no-optional-locks".into(),
            "--literal-pathspecs".into(),
            "-c".into(),
            "core.fsmonitor=false".into(),
            "-c".into(),
            "core.hooksPath=/dev/null".into(),
            "-c".into(),
            "color.ui=false".into(),
            "-c".into(),
            "core.quotepath=false".into(),
        ];
        // Reading the project (status, diffs, logs) never runs the
        // repository's own filter drivers; staging and committing for you
        // keep them, as Git in a terminal would (`crate::git_guard`).
        if writes_for_the_user(&args) {
            base.extend(args);
        } else {
            // Status and diffs also stay out of submodules' working trees,
            // whose own configuration is not inspected.
            let words: Vec<&str> = args.iter().map(String::as_str).collect();
            base.extend(crate::git_guard::args_async(workspace, &words).await?);
            base.extend(crate::git_guard::harden(&words));
        }
        let refs: Vec<_> = base.iter().map(String::as_str).collect();
        Ok(json!(
            process::run(
                ProcessSpec::command("git", &refs, workspace.to_owned()),
                cancel,
                None
            )
            .await?
        ))
    }
    pub(super) async fn git_status(&self) -> Result<Value> {
        let workspace = self.workspace()?;
        let git = |args| Self::git_in(&workspace, args, CancellationToken::new());
        let status = git(vec!["status".into(), "--porcelain=v1".into(), "-b".into()]).await?;
        if status["ok"] != true {
            return Ok(
                json!({"repo":false,"status":"","log":"","diff":"","files":[],"error":status["stderr"]}),
            );
        }
        let log = git(vec![
            "log".into(),
            "-12".into(),
            "--oneline".into(),
            "--no-show-signature".into(),
        ])
        .await?;
        let raw = git(vec!["status".into(), "--porcelain=v1".into(), "-z".into()]).await?;
        ensure!(raw["ok"] == true, "Git status failed: {}", raw["stderr"]);
        let mut files = Vec::new();
        let mut records = raw["stdout"].as_str().unwrap_or("").split('\0');
        while let Some(record) = records.next() {
            if record.len() < 4 {
                continue;
            }
            let label = &record[..2];
            let mut entry = json!({"path":&record[3..],"label":label.trim(),"index":&record[..1],"work":&record[1..2]});
            if label.contains('R') || label.contains('C') {
                entry["original_path"] = json!(records.next().unwrap_or(""));
            }
            files.push(entry);
        }
        Ok(
            json!({"repo":true,"status":status["stdout"],"porcelain":status["stdout"],"log":log["stdout"],"diff":"","files":files,"truncated":raw["truncated"]}),
        )
    }
    pub(super) async fn git_diff(&self, path: &str) -> Result<Value> {
        Self::git_diff_in(&self.workspace()?, path, CancellationToken::new()).await
    }
    pub(super) async fn git_diff_in(
        workspace: &Path,
        path: &str,
        cancel: CancellationToken,
    ) -> Result<Value> {
        let path = if path.is_empty() {
            String::new()
        } else {
            Workspace::open(workspace)?
                .relative(path)?
                .to_string_lossy()
                .into_owned()
        };
        let git = |args| Self::git_in(workspace, args, cancel.clone());
        let mut args = vec![
            "diff".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--no-renames".into(),
        ];
        let mut staged = args.clone();
        staged.push("--cached".into());
        if !path.is_empty() {
            args.extend(["--".into(), path.clone()]);
            staged.extend(["--".into(), path.clone()]);
        }
        let mut normal = git(args).await?;
        let staged = git(staged).await?;
        ensure!(
            normal["ok"] == true && staged["ok"] == true,
            "Could not read Git changes: {} {}",
            normal["stderr"],
            staged["stderr"]
        );
        let mut untracked = false;
        if !path.is_empty() {
            let others = git(vec![
                "ls-files".into(),
                "--others".into(),
                "--exclude-standard".into(),
                "-z".into(),
                "--".into(),
                path.clone(),
            ])
            .await?;
            ensure!(
                others["ok"] == true && others["truncated"] != true,
                "Could not identify the selected file"
            );
            untracked = others["stdout"]
                .as_str()
                .unwrap_or("")
                .split('\0')
                .any(|entry| entry == path);
            if untracked {
                normal = git(vec![
                    "diff".into(),
                    "--no-index".into(),
                    "--no-ext-diff".into(),
                    "--no-textconv".into(),
                    "--".into(),
                    "/dev/null".into(),
                    path.clone(),
                ])
                .await?;
                ensure!(
                    normal["exit_code"] == 0 || normal["exit_code"] == 1,
                    "Could not preview new file: {}",
                    normal["stderr"]
                );
            }
        }
        let text = normal["stdout"].as_str().unwrap_or("");
        let staged_text = staged["stdout"].as_str().unwrap_or("");
        Ok(
            json!({"path":path,"diff":text,"staged":staged_text,"hunks":parse_hunks(text),"staged_hunks":parse_hunks(staged_text),"untracked":untracked,"binary":text.contains("Binary files ") || staged_text.contains("Binary files "),"truncated":normal["truncated"]==true || staged["truncated"]==true}),
        )
    }
    pub(super) async fn hunk_action(&self, body: &Value) -> Result<Value> {
        let ws = self.mutable_workspace()?;
        let path = ws
            .relative(body["path"].as_str().context("Choose a file")?)?
            .to_string_lossy()
            .into_owned();
        ensure!(
            !path.contains(['\n', '\r', '\t', '"']),
            "Stage files with special path characters as a whole file"
        );
        let action = body["action"].as_str().unwrap_or("");
        ensure!(
            matches!(action, "accept" | "reject"),
            "Choose accept or reject"
        );
        let current = Self::git_diff_in(&ws.path, &path, ws.reservation.cancellation()).await?;
        ensure!(
            current["untracked"] != true,
            "Stage new files as a whole file"
        );
        ensure!(
            current["binary"] != true && current["truncated"] != true,
            "Stage binary or truncated changes as a whole file"
        );
        ensure!(
            current["hunks"]
                .as_array()
                .is_some_and(|hunks| hunks.contains(&body["hunk"])),
            "This diff has changed. Refresh it before applying a hunk"
        );
        let diff = current["diff"].as_str().context("Missing diff")?;
        let prefix = diff.split_once("\n@@ ").context("Missing hunk header")?.0;
        let mut patch = format!(
            "{prefix}\n{}\n",
            body["hunk"]["header"]
                .as_str()
                .context("Missing hunk header")?
        );
        for line in body["hunk"]["lines"]
            .as_array()
            .context("Missing hunk lines")?
        {
            patch.push_str(match line["kind"].as_str() {
                Some("add") => "+",
                Some("del") => "-",
                Some("meta") => "",
                _ => " ",
            });
            patch.push_str(line["text"].as_str().context("Missing hunk text")?);
            patch.push('\n');
        }
        let mut input = tempfile::NamedTempFile::new()?;
        input.write_all(patch.as_bytes())?;
        input.flush()?;
        let result = Self::git_in(
            &ws.path,
            vec![
                "apply".into(),
                "--recount".into(),
                "--unidiff-zero".into(),
                if action == "accept" {
                    "--cached"
                } else {
                    "--reverse"
                }
                .into(),
                "--".into(),
                input.path().to_string_lossy().into_owned(),
            ],
            ws.reservation.cancellation(),
        )
        .await?;
        ensure!(
            result["ok"] == true,
            "Could not apply hunk: {}",
            result["stderr"]
        );
        Ok(json!({"ok":true,"action":action,"path":path}))
    }
    /// POST /api/workspace/git/add: stage whole files.
    pub(super) async fn git_add(&self, body: &Value) -> Result<Value> {
        let ws = self.mutable_workspace()?;
        let paths = body["paths"].as_array().context("paths must be an array")?;
        ensure!(
            !paths.is_empty() && paths.len() <= 200,
            "Choose files to stage"
        );
        let mut args = vec!["add".into(), "--".into()];
        for path in paths {
            args.push(
                ws.relative(path.as_str().context("Invalid path")?)?
                    .to_string_lossy()
                    .into_owned(),
            );
        }
        let result = Self::git_in(&ws.path, args, ws.reservation.cancellation()).await?;
        ensure!(result["ok"] == true, "{}", result["stderr"]);
        Ok(json!({"ok":true}))
    }
    /// POST /api/workspace/git/commit: commit what is staged (never signed).
    pub(super) async fn git_commit(&self, message: &str) -> Result<Value> {
        let ws = self.mutable_workspace()?;
        ensure!(
            !message.trim().is_empty() && message.len() <= 32000,
            "Commit message required"
        );
        let result = Self::git_in(
            &ws.path,
            vec![
                "-c".into(),
                "commit.gpgSign=false".into(),
                "commit".into(),
                "-m".into(),
                message.into(),
            ],
            ws.reservation.cancellation(),
        )
        .await?;
        ensure!(result["ok"] == true, "{}", result["stderr"]);
        Ok(json!({"ok":true}))
    }
}
pub fn parse_hunks(diff: &str) -> Vec<Value> {
    let mut hunks = Vec::new();
    let mut current = None;
    for line in diff.lines() {
        if line.starts_with("diff --git ") || line.starts_with("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            if line.starts_with("@@ ") {
                current = Some(json!({"header":line,"lines":[]}));
            }
        } else if let Some(hunk) = &mut current {
            if let Some(lines) = hunk["lines"].as_array_mut() {
                let (kind, text) = match line.as_bytes().first() {
                    Some(b'+') => ("add", &line[1..]),
                    Some(b'-') => ("del", &line[1..]),
                    Some(b' ') => ("ctx", &line[1..]),
                    _ => ("meta", line),
                };
                lines.push(json!({"kind":kind,"text":text}));
            }
        }
    }
    if let Some(hunk) = current {
        hunks.push(hunk);
    }
    hunks
}

/// Git commands that change the project's real index, history or files at
/// the user's request (Changes view buttons). They keep the repository's
/// filters so, for example, git-crypt still encrypts what is committed.
fn writes_for_the_user(args: &[String]) -> bool {
    let mut words = args.iter();
    while let Some(word) = words.next() {
        if matches!(word.as_str(), "-c" | "-C") {
            words.next();
        } else if !word.starts_with('-') {
            return matches!(
                word.as_str(),
                "add"
                    | "commit"
                    | "apply"
                    | "restore"
                    | "reset"
                    | "checkout"
                    | "switch"
                    | "stash"
                    | "rm"
                    | "mv"
                    | "merge"
                    | "rebase"
                    | "cherry-pick"
                    | "revert"
            );
        }
    }
    false
}

#[cfg(test)]
mod guard_tests {
    use super::writes_for_the_user;

    #[test]
    fn only_user_requested_writes_keep_repository_filters() {
        let args = |list: &[&str]| list.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for write in [
            &["add", "--", "a"][..],
            &["-c", "user.name=x", "commit", "-m", "m"],
            &["apply", "--cached", "-"],
        ] {
            assert!(writes_for_the_user(&args(write)), "{write:?}");
        }
        for read in [
            &["status", "--porcelain=v1"][..],
            &["diff", "--no-ext-diff"],
            &["-c", "x=commit", "log", "-1"],
            &["ls-files", "--others"],
        ] {
            assert!(!writes_for_the_user(&args(read)), "{read:?}");
        }
    }
}
