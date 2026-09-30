//! Native `exec`: the sandbox for the command, a project checkpoint around
//! it (so rewind can undo what it wrote), and the bounded process run.
use super::*;
use crate::checkpoint::capture;

impl ToolExecutor {
    pub(super) async fn shell(&self, args: &Value) -> Result<Value> {
        let command = string(args, "command")?;
        ensure!(
            !command.is_empty() && command.len() <= 64_000 && !command.contains('\0'),
            "Invalid shell command"
        );
        let seconds = integer(
            args,
            "timeout_sec",
            self.config.agent.tool_timeout_sec as usize,
            1,
            3600,
        )?
        .min(self.config.agent.tool_timeout_sec as usize);
        let cwd = if let Some(path) = args["cwd"].as_str() {
            let relative = self.workspace.relative(path)?;
            let cwd = self.workspace.path.join(relative).canonicalize()?;
            ensure!(
                cwd.starts_with(&self.workspace.path),
                "Command directory escapes the workspace"
            );
            cwd
        } else {
            self.workspace.path.clone()
        };
        let policy = crate::sandbox::ShellPolicy::from_config(&self.config)?;
        let (workspace, run_in, text) =
            (self.workspace.path.clone(), cwd.clone(), command.to_owned());
        // Fails closed (the command does not run) when the sandbox is
        // required, or the allow-list is on, and bubblewrap is unavailable.
        let prepared = tokio::task::spawn_blocking(move || {
            crate::sandbox::prepare_shell(&workspace, &run_in, &text, &policy)
        })
        .await??;
        if let Some(warning) = &prepared.warning {
            if crate::sandbox::first_warning(&self.events.session_id) {
                self.events
                    .emit("agent.warning", json!({"text":warning,"kind":"sandbox"}))?;
            }
        }
        let checkpoint = if self.config.checkpoints.shell && !capture::cannot_write(command) {
            capture::before(
                &self.workspace.path,
                &self.events.session_id,
                "a shell command",
                &self.config.checkpoints,
            )
            .await
        } else {
            capture::not_needed()
        };
        let proxied = prepared.gate.is_some();
        let spec = ProcessSpec {
            program: prepared.program,
            args: prepared.args,
            cwd: cwd.clone(),
            timeout: Duration::from_secs(seconds as u64),
            output_limit: self.config.agent.max_output_bytes,
            env: self.task_env.clone(),
            child: prepared.child,
        };
        // Never replay a command after a process failure: it may have changed files.
        let result = process::run(spec, self.cancel.clone(), None).await;
        let mut sandbox_note = prepared.note;
        if let Some(gate) = &prepared.gate {
            sandbox_note["proxy"] = gate.report();
        }
        if let Some(path) = prepared.scratch {
            // The command already ran; a cleanup failure must not discard its
            // recorded output and exit status.
            if let Err(error) = crate::sandbox::discard_scratch(&path) {
                sandbox_note["scratch_cleanup_error"] = json!(format!("{error:#}"));
            }
        }
        // Record what the command wrote even when it failed or was cancelled.
        let mut outside = Vec::new();
        let checkpoint_note = match capture::after(
            checkpoint,
            &self.events.store,
            &self.workspace,
            &self.events.task_id,
        )
        .await
        {
            Ok(outcome) => {
                if !outcome.paths.is_empty() {
                    if let Ok(mut observed) = self.observed.lock() {
                        for path in &outcome.paths {
                            observed.remove(path);
                        }
                    }
                    let mut summary = checkpoint::summary(
                        &self.events.store,
                        &self.workspace,
                        &self.events.task_id,
                    )?;
                    summary["source"] = json!("shell");
                    summary["changed"] = json!(outcome.paths);
                    summary["ignored_saved"] = json!(outcome.ignored);
                    self.events.emit("checkpoint.updated", summary)?;
                }
                // "Only change these": a command is not asked about file by
                // file, so what it changed elsewhere is named after it ran.
                outside = self.changed_outside_scope(
                    outcome
                        .paths
                        .iter()
                        .map(String::as_str)
                        .chain(outcome.skipped.iter().filter_map(|s| s["path"].as_str())),
                );
                if !outside.is_empty() {
                    self.events
                        .emit("scope.outside", json!({"paths":outside,"tool":"exec"}))?;
                }
                let mut note = outcome.to_json();
                if let Some(warning) = outcome.warning("this command") {
                    note["warning"] = json!(warning);
                }
                note
            }
            Err(error) => json!({"method":"none","error":format!("{error:#}")}),
        };
        let result = result.map_err(|error| {
            if proxied {
                error.context("The network allow-list sandbox could not start (unprivileged user namespaces may be disabled on this system); the command was not run or was stopped")
            } else {
                error
            }
        })?;
        let mut value = serde_json::to_value(result)?;
        value["sandbox"] = sandbox_note;
        value["checkpoint"] = checkpoint_note;
        if !outside.is_empty() {
            value["outside_scope"] = json!({
                "paths": outside,
                "note": "This command changed files outside the ones the user chose for this task. Change only the chosen files unless the user asks; the user can undo these in Review.",
            });
        }
        Ok(value)
    }
}
