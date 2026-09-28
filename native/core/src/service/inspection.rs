use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

fn check(id: &str, status: &str, label: &str, detail: impl Into<String>, fix: &str) -> Value {
    json!({"id":id,"status":status,"ok":status=="pass","label":label,"detail":detail.into(),"fix":if status=="pass"{""}else{fix}})
}
fn executable(name: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .take(128)
        .map(|dir| dir.join(name))
        .find(|path| {
            std::fs::metadata(path)
                .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        })
}
impl Service {
    pub(super) async fn project_map(&self, save: bool) -> Result<Value> {
        let selection = self.snapshot_selection()?;
        let path = selection.workspace.clone();
        let workspace = Arc::new(Workspace::open(&path)?);
        let map = crate::project::inspect(workspace.clone()).await?;
        let rendered = crate::project::render(&map);
        if save {
            let (_project, _ownership) = self.workspace_mutation_guards_at(&workspace.path).await?;
            ensure!(
                self.snapshot_selection()?.generation == selection.generation,
                "Project selection changed; retry saving the project map"
            );
            let ws = self.mutable_workspace_at(&workspace.path)?;
            let file = ".shadow/memory/project.md";
            let before = ws.snapshot(file)?;
            let old = before
                .bytes
                .map(String::from_utf8)
                .transpose()?
                .unwrap_or_default();
            let next = crate::project::merge_memory(&old, &rendered)?;
            ws.write(
                file,
                next.as_bytes(),
                before.hash.as_deref().or(Some("missing")),
            )?;
        }
        Ok(
            json!({"ok":true,"project_map":map,"text":rendered,"saved":save,"memory_file":if save{Some(".shadow/memory/project.md")}else{None}}),
        )
    }
    pub(super) async fn change_history(&self, path: &str, count: usize) -> Result<Value> {
        ensure!(
            (1..=50).contains(&count),
            "History count must be between 1 and 50"
        );
        let workspace = Workspace::open(&self.workspace()?)?;
        let path = if path.is_empty() {
            String::new()
        } else {
            workspace.relative(path)?.to_string_lossy().into_owned()
        };
        let mut args = vec![
            "log".into(),
            format!("-{count}"),
            "--no-show-signature".into(),
            "--no-ext-diff".into(),
            "--no-textconv".into(),
            "--date=iso-strict".into(),
            "--format=%h %ad %s".into(),
            "--".into(),
        ];
        if !path.is_empty() {
            args.push(path.clone());
        }
        let log = Self::git_in(&workspace.path, args, CancellationToken::new()).await?;
        let empty_history = if log["ok"] == true {
            false
        } else {
            let repo = Self::git_in(
                &workspace.path,
                vec!["rev-parse".into(), "--is-inside-work-tree".into()],
                CancellationToken::new(),
            )
            .await?;
            let head = Self::git_in(
                &workspace.path,
                vec![
                    "rev-parse".into(),
                    "--verify".into(),
                    "--quiet".into(),
                    "HEAD".into(),
                ],
                CancellationToken::new(),
            )
            .await?;
            ensure!(repo["ok"]==true&&repo["stdout"].as_str().is_some_and(|v|v.trim()=="true")&&head["exit_code"]==1,"Could not read commit history; check that this project is a readable Git repository");
            true
        };
        let diff = Self::git_diff_in(&workspace.path, &path, CancellationToken::new()).await?;
        Ok(
            json!({"ok":true,"path":path,"count":count,"log":if empty_history{json!("")}else{log["stdout"].clone()},"empty_history":empty_history,"diff":diff,"truncated":log["truncated"],"note":"Commit subjects and current diffs are recorded evidence; they do not establish an author's intent."}),
        )
    }
    pub(super) async fn doctor(&self, test_model: bool) -> Result<Value> {
        let paths = self.engine.paths();
        let mut checks = vec![check(
            "runtime",
            "pass",
            "Native runtime",
            format!(
                "ShadowCode {} (Rust); no Python runtime or browser server is required",
                crate::VERSION
            ),
            "",
        )];
        let config = self.config();
        checks.push(check(
            "config",
            if config.is_ok() { "pass" } else { "fail" },
            "Configuration",
            if config.is_ok() {
                "Configuration loaded and validated"
            } else {
                "Configuration could not be loaded"
            },
            "Review config.yaml and the project's settings overlay.",
        ));
        for (name, path) in [
            ("config-directory", &paths.config),
            ("data-directory", &paths.data),
            ("state-directory", &paths.state),
        ] {
            let private = std::fs::symlink_metadata(path).is_ok_and(|m| {
                m.is_dir()
                    && !m.file_type().is_symlink()
                    && m.uid() == unsafe { libc::geteuid() }
                    && m.mode() & 0o077 == 0
            });
            checks.push(check(
                name,
                if private { "pass" } else { "warn" },
                "Private profile directory",
                path.display().to_string(),
                "Keep profile directories owned by your account with mode 700.",
            ));
        }
        let secrets = paths.secrets_file();
        let secret_status = match std::fs::symlink_metadata(&secrets) {
            Ok(m)
                if m.is_file()
                    && !m.file_type().is_symlink()
                    && m.uid() == unsafe { libc::geteuid() }
                    && m.mode() & 0o077 == 0 =>
            {
                "pass"
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => "pass",
            _ => "fail",
        };
        checks.push(check(
            "secrets-permissions",
            secret_status,
            "Credential file permissions",
            "Only file metadata is reported; no credential values are included",
            "Keep secrets.env as a regular file owned by your account with mode 600.",
        ));
        let database_ok = self
            .engine
            .store()
            .query("PRAGMA quick_check(1)", [])
            .is_ok_and(|rows| rows.len() == 1 && rows[0]["quick_check"] == "ok");
        checks.push(check(
            "database",
            if database_ok { "pass" } else { "fail" },
            "History database integrity",
            if database_ok {
                "SQLite quick_check passed"
            } else {
                "SQLite integrity check failed"
            },
            "Back up the profile before recovering its database; do not delete your history.",
        ));
        let git = executable("git");
        checks.push(check(
            "git",
            if git.is_some() { "pass" } else { "warn" },
            "Git executable",
            git.map(|p| p.display().to_string())
                .unwrap_or_else(|| "Not found on PATH".into()),
            "Install Git for repository history, review and worktree features.",
        ));
        let map = crate::project::inspect(Arc::new(Workspace::open(&self.workspace()?)?)).await?;
        checks.push(check(
            "project-scan",
            if map["inspection"]["truncated"] == true {
                "warn"
            } else {
                "pass"
            },
            "Project inspection",
            format!(
                "{} source files inspected; heuristic findings are not test results",
                map["inspection"]["source_files"]
            ),
            "Large projects use a bounded partial scan; inspect relevant files explicitly.",
        ));
        checks.push(check(
            "project-docs",
            if map["documentation_present"] == true {
                "pass"
            } else {
                "info"
            },
            "Project documentation",
            if map["documentation_present"] == true {
                "README or docs directory found"
            } else {
                "No README or docs directory found in the scan"
            },
            "Add project setup and usage documentation where useful.",
        ));
        let mut runners = std::collections::BTreeSet::new();
        for candidate in map["test_commands"].as_array().into_iter().flatten() {
            if let Some(runner) = candidate["runner"].as_str() {
                runners.insert(runner);
            }
        }
        for runner in runners {
            checks.push(check(&format!("project-tool-{runner}"),if executable(runner).is_some(){"pass"}else{"warn"},"Project test toolchain",format!("{runner}: {}",if executable(runner).is_some(){"available"}else{"not found"}),"Install this project's toolchain to run its tests; it is not required by ShadowCode itself."));
        }
        if let Ok(cfg) = config {
            checks.push(check(
                "model-configured",
                if cfg.model.provider == "mock" {
                    "warn"
                } else {
                    "pass"
                },
                "Coding model selected",
                format!("{} / {}", cfg.model.provider, cfg.model.name),
                "Select a local or compatible coding model in Settings.",
            ));
            if test_model {
                let cancel = CancellationToken::new();
                let _guard = cancel.clone().drop_guard();
                let result = match ModelClient::new(cfg.model, self.engine.paths()) {
                    Ok(client) => {
                        tokio::time::timeout(Duration::from_secs(15), client.test(cancel))
                            .await
                            .ok()
                            .and_then(Result::ok)
                    }
                    Err(_) => None,
                };
                checks.push(check("model-response",if result.is_some(){"pass"}else{"fail"},"Actual model response",result.map(|v|format!("Received a response in {} ms",v["latency_ms"])).unwrap_or_else(||"Model test failed or exceeded 15 seconds; no remote error body is displayed".into()),"Check the selected model, endpoint and credential reference in Settings."));
            } else {
                checks.push(check("model-response","not_checked","Actual model response","Not contacted; use --test-model or test_model=true to send a small diagnostic prompt",""));
            }
        }
        if let Ok(stats) = self.engine.store().local_stats() {
            checks.push(check(
                "history-scale",
                "info",
                "Local history size",
                format!(
                    "{} events, {} sessions, {} bytes; journal {}; no telemetry",
                    stats["events"], stats["sessions"], stats["bytes"], stats["journal_mode"]
                ),
                "",
            ));
        }
        if let Ok(cfg) = self.config() {
            let caps = crate::autonomy::effective_caps(
                &cfg.agent.autonomy_profile,
                cfg.agent.max_steps,
                cfg.agent.max_task_tokens,
            );
            let profile =
                crate::autonomy::capability_profile(&cfg.model.provider, cfg.model.context_limit);
            checks.push(check(
                "autonomy-budget",
                "info",
                "Autonomy budget",
                format!(
                    "profile {} · {} steps · {} tokens; not a silent kill switch",
                    cfg.agent.autonomy_profile, caps.max_steps, caps.max_tokens
                ),
                "",
            ));
            checks.push(check(
                "provider-profile",
                "info",
                "Provider capability profile",
                format!(
                    "{} context {} streaming={} parallel_tools={}",
                    profile.provider,
                    profile.context_window,
                    profile.streaming,
                    profile.parallel_tools
                ),
                "",
            ));
            checks.push(check(
                "shell-policy",
                "info",
                "Shell policy is heuristic",
                crate::autonomy::shell_policy_limits()["do_not_claim"]
                    .as_str()
                    .unwrap_or("not an OS sandbox"),
                "",
            ));
            checks.extend(crate::sandbox::doctor_checks());
            for mut check in crate::cli_agent::doctor::checks(&cfg.cli_agents).await {
                let status = check["status"].as_str().unwrap_or("info").to_owned();
                check["ok"] = json!(status == "pass");
                if check.get("title").is_none() {
                    check["title"] = check["label"].clone();
                }
                checks.push(check);
            }
        }
        let failures = checks.iter().filter(|c| c["status"] == "fail").count();
        let mut report = json!({"ok":failures==0,"version":crate::VERSION,"runtime":"rust","checks":checks,"failures":failures,"project_map":map,"suggestions":checks.iter().filter(|c|c["status"]!="pass"&&c["fix"]!="").map(|c|c["fix"].clone()).collect::<Vec<_>>(),"telemetry":false});
        report["diagnostic_export"] = self.diagnostic_exports.prepare(&report)?;
        Ok(report)
    }
}
