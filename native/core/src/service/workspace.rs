//! `/api/workspace…`: the selected project's files, instructions, skills,
//! attachments, the terminal Run button, project inspection and Git (the Git
//! helpers live in `git.rs`). Git, exec and inspection await processes; the
//! file routes are synchronous and run on the blocking pool.
use super::*;

#[derive(Default, Deserialize)]
#[serde(default)]
struct ExecBody {
    workspace: Text,
    command: Text,
    session_id: Text,
    timeout: Loose<u64>,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct FileBody {
    workspace: Text,
    name: Text,
    content: Text,
    expected_hash: Text,
    filename: Text,
    text: Text,
    data_base64: Text,
    base: Text,
    draft: Text,
    base_hash: Text,
    expected_revision: Text,
}

impl Service {
    pub(super) async fn workspace_routes(&self, call: &Arc<Call>) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/workspace/understand") => self.project_map(false).await,
            ("POST", "/api/workspace/understand") => {
                self.project_map(call.body["save"] == true).await
            }
            ("GET", "/api/workspace/why") => {
                self.change_history(
                    call.q("path"),
                    if call.q("count").is_empty() {
                        8
                    } else {
                        call.q("count").parse().context("Invalid history count")?
                    },
                )
                .await
            }
            ("POST", "/api/workspace/exec") => self.exec(call).await,
            ("GET", "/api/workspace/mentions") => self.blocking(call, Self::mention_search).await,
            ("POST", "/api/workspace/context-preview") => {
                self.blocking(call, Self::context_preview).await
            }
            ("PUT" | "DELETE", "/api/workspace/editor-draft") => {
                self.locked_editor_draft(call).await
            }
            ("PUT", "/api/workspace/file")
            | ("PUT", "/api/workspace/instructions")
            | ("PUT", "/api/workspace/skills")
            | ("POST", "/api/workspace/attach")
            | ("POST", "/api/workspace/attach-image") => self.locked_workspace_write(call).await,
            ("GET", "/api/workspace/git") => self.git_status().await,
            ("GET", "/api/workspace/diff") => self.git_diff(call.q("path")).await,
            ("POST", "/api/workspace/diffstat") => self.diff_stats(&call.body).await,
            ("POST", "/api/workspace/diff/hunk") => {
                let (service, _project, _ownership) = self.selected_workspace_mutation().await?;
                service.hunk_action(&call.body).await
            }
            ("POST", "/api/workspace/git/add") => {
                let (service, _project, _ownership) = self.selected_workspace_mutation().await?;
                service.git_add(&call.body).await
            }
            ("POST", "/api/workspace/git/commit") => {
                let (service, _project, _ownership) = self.selected_workspace_mutation().await?;
                service.git_commit(&call.body).await
            }
            ("POST", "/api/workspace/git/unstage") => {
                let (service, _project, _ownership) = self.selected_workspace_mutation().await?;
                service.git_unstage(&call.body).await
            }
            ("POST", "/api/workspace/git/ignore") => {
                let (service, _project, _ownership) = self.selected_workspace_mutation().await?;
                service.git_ignore(&call.body).await
            }
            ("GET", "/api/workspace/git/hooks") => self.git_hooks(None).await,
            ("POST", "/api/workspace/git/hooks") => self.git_hooks(Some(&call.body)).await,
            _ => self.blocking(call, Self::workspace_files).await,
        }
    }

    async fn selected_workspace_mutation(
        &self,
    ) -> Result<(
        Service,
        tokio::sync::OwnedMutexGuard<()>,
        Option<std::fs::File>,
    )> {
        let selected = self.snapshot_selection()?;
        let workspace = Workspace::open(&selected.workspace)?.path;
        let (guard, ownership) = self.workspace_mutation_guards_at(&workspace).await?;
        ensure!(
            self.snapshot_selection()?.generation == selected.generation,
            "Project selection changed; retry the workspace mutation"
        );
        let service = self.fork_selection(workspace, selected.session)?;
        Ok((service, guard, ownership))
    }

    pub(super) async fn workspace_mutation_guards_at(
        &self,
        path: &Path,
    ) -> Result<(tokio::sync::OwnedMutexGuard<()>, Option<std::fs::File>)> {
        let workspace = Workspace::open(path)?;
        let guard = crate::compare::project_lock(&workspace.path)?
            .lock_owned()
            .await;
        let cancel = CancellationToken::new();
        let ownership = crate::compare::workspace_mutation_lock(&workspace.path, &cancel).await?;
        Ok((guard, ownership))
    }

    /// Keep project and repository ownership until the database or blocking
    /// workspace mutation completes.
    async fn locked_editor_draft(&self, call: &Arc<Call>) -> Result<Value> {
        self.locked_workspace_write(call).await
    }

    async fn locked_workspace_write(&self, call: &Arc<Call>) -> Result<Value> {
        let (service, guard, ownership) = self.selected_workspace_mutation().await?;
        let call = call.clone();
        match tokio::task::spawn_blocking(move || {
            let _guard = guard;
            let _ownership = ownership;
            service.workspace_files(&call)
        })
        .await
        {
            Ok(result) => result,
            Err(error) if error.is_panic() => std::panic::resume_unwind(error.into_panic()),
            Err(error) => Err(anyhow::anyhow!("Application command stopped: {error}")),
        }
    }

    fn workspace_files(&self, call: &Call) -> Result<Value> {
        let body: FileBody = call.body()?;
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/workspace/status") => {
                let cfg = self.config()?;
                Ok(
                    json!({"workspace":self.workspace()?,"model":cfg.model,"permissions":cfg.permissions,"onboarding":cfg.onboarding,"routing":cfg.routing,"trusted":cfg.is_trusted(&self.workspace()?)}),
                )
            }
            ("GET", "/api/workspace/files") => {
                let workspace = Workspace::open(&self.workspace()?)?;
                let path = if call.q("path").is_empty() {
                    "."
                } else {
                    call.q("path")
                };
                let relative = workspace.relative(path)?;
                Ok(
                    json!({"entries":workspace.list(path)?,"workspace":workspace.path,"path":relative,"parent":if relative==Path::new("."){String::new()}else{relative.parent().filter(|p|!p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_string_lossy().into_owned()}}),
                )
            }
            ("GET", "/api/workspace/file") => {
                let workspace = Workspace::open(&self.workspace()?)?;
                let head = call.q("head") == "true";
                let full = call.q("full") == "true";
                if head || full {
                    let (bytes, size) = workspace
                        .inspect(call.q("path"), crate::workspace::MAX_FILE_BYTES + 1)
                        .map_err(|error| {
                            if error.chain().any(|cause| {
                                cause
                                    .downcast_ref::<std::io::Error>()
                                    .is_some_and(|io| io.kind() == std::io::ErrorKind::NotFound)
                            }) {
                                anyhow::anyhow!("File not found")
                            } else {
                                error
                            }
                        })?;
                    ensure!(
                        size <= crate::workspace::MAX_FILE_BYTES as u64
                            && bytes.len() == size as usize,
                        "File exceeds the 4 MB edit limit or changed while being read"
                    );
                    let path = workspace.relative(call.q("path"))?;
                    let hash = crate::workspace::hash(&bytes);
                    if full && !head {
                        ensure!(
                            !bytes.contains(&0),
                            "Binary files cannot be displayed as text"
                        );
                        let content =
                            String::from_utf8(bytes).context("File is not valid UTF-8")?;
                        return Ok(
                            json!({"path":path.to_string_lossy(),"hash":hash,"bytes":size,"content":content,"truncated":false,"secret_target":workspace.is_secret_target(call.q("path"))}),
                        );
                    }
                    return Ok(json!({"path":path.to_string_lossy(),"hash":hash,"bytes":size}));
                }
                let file = workspace.read(call.q("path"))?;
                Ok(
                    json!({"path":file.path,"content":truncate(&file.content,200000),"hash":file.hash,"bytes":file.bytes,"truncated":file.content.len()>200000,"secret_target":workspace.is_secret_target(call.q("path"))}),
                )
            }
            ("PUT", "/api/workspace/file") => {
                let expected = body.expected_hash.as_str();
                ensure!(
                    expected == "missing"
                        || (expected.len() == 64
                            && expected.bytes().all(|b| b.is_ascii_hexdigit())),
                    "A valid opened-file revision is required to save"
                );
                let (workspace, turn) = self.editable_workspace()?;
                let path = call.q("path");
                let hash =
                    workspace.write(path, body.content.as_str().as_bytes(), Some(expected))?;
                let relative = workspace.relative(path)?.to_string_lossy().into_owned();
                // Saved while a subscription turn runs: Rewind of that turn
                // keeps this file as you saved it.
                if let Some(task) = &turn {
                    crate::checkpoint::turn_edits::note_save(
                        &self.engine.store(),
                        task,
                        &relative,
                        &hash,
                    )?;
                }
                Ok(
                    json!({"path":relative,"hash":hash,"bytes":body.content.as_str().len(),"during_turn":turn.is_some()}),
                )
            }
            ("GET", "/api/workspace/editor-drafts") => {
                let workspace = Workspace::open(&self.workspace()?)?;
                ensure!(
                    call.q("workspace") == workspace.path.to_string_lossy(),
                    "Project selection changed; reopen its editor drafts"
                );
                let drafts = self.engine.store().editor_drafts(&workspace.path)?;
                Ok(json!({"workspace":workspace.path,"drafts":drafts}))
            }
            ("PUT", "/api/workspace/editor-draft") => {
                let workspace = Workspace::open(&self.workspace()?)?;
                ensure!(
                    body.workspace.as_str() == workspace.path.to_string_lossy(),
                    "Project selection changed; this editor draft was not stored"
                );
                let path = workspace.writable(call.q("path"))?;
                let path = path.to_string_lossy();
                let expected = body.expected_revision.as_str();
                ensure!(
                    expected == "missing"
                        || (expected.len() == 32
                            && expected.bytes().all(|b| b.is_ascii_hexdigit())),
                    "A valid recovery-draft revision is required"
                );
                ensure!(
                    body.base_hash.as_str().len() == 64
                        && body
                            .base_hash
                            .as_str()
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit())
                        && crate::workspace::hash(body.base.as_str().as_bytes())
                            == body.base_hash.as_str(),
                    "The recovery draft's original file revision is invalid"
                );
                let record = self.engine.store().put_editor_draft(
                    &workspace.path,
                    &path,
                    body.base.as_str(),
                    body.draft.as_str(),
                    body.base_hash.as_str(),
                    expected,
                )?;
                Ok(json!(record))
            }
            ("DELETE", "/api/workspace/editor-draft") => {
                let workspace = Workspace::open(&self.workspace()?)?;
                ensure!(
                    body.workspace.as_str() == workspace.path.to_string_lossy(),
                    "Project selection changed; this editor draft was not removed"
                );
                let path = workspace.writable(call.q("path"))?;
                let expected = body.expected_revision.as_str();
                ensure!(
                    expected == "missing"
                        || (expected.len() == 32
                            && expected.bytes().all(|b| b.is_ascii_hexdigit())),
                    "A valid recovery-draft revision is required"
                );
                self.engine.store().delete_editor_draft(
                    &workspace.path,
                    &path.to_string_lossy(),
                    expected,
                )?;
                Ok(json!({"removed":true}))
            }
            ("GET", "/api/workspace/instructions") => {
                let ws = Workspace::open(&self.workspace()?)?;
                let snapshot = ws.snapshot(".shadow/instructions.md")?;
                Ok(
                    json!({"exists":snapshot.bytes.is_some(),"content":snapshot.bytes.map(String::from_utf8).transpose()?.unwrap_or_default(),"path":".shadow/instructions.md"}),
                )
            }
            ("PUT", "/api/workspace/instructions") => {
                let ws = self.mutable_workspace()?;
                ws.write(
                    ".shadow/instructions.md",
                    body.content.as_str().as_bytes(),
                    None,
                )?;
                Ok(json!({"ok":true}))
            }
            ("GET", "/api/workspace/skills") => {
                let ws = Workspace::open(&self.workspace()?)?;
                let catalog = self.workflow_catalog(&ws);
                let skills: Vec<_> = catalog
                    .definitions
                    .into_iter()
                    .filter(|item| item.info.kind == "skill")
                    .collect();
                Ok(json!({"skills":skills,"issues":catalog.issues}))
            }
            ("PUT", "/api/workspace/skills") => {
                let name = body.name.as_str();
                ensure!(
                    !name.is_empty()
                        && name.len() <= 80
                        && name
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
                    "Skill name may contain letters, numbers, - and _"
                );
                let ws = self.mutable_workspace()?;
                let path = format!(".shadow/skills/{name}.md");
                crate::workflows::Definition::parse(&path, "skill", body.content.as_str(), "")?;
                ws.write(
                    &path,
                    body.content.as_str().as_bytes(),
                    body.expected_hash.0.as_deref(),
                )?;
                Ok(json!({"ok":true}))
            }
            ("POST", "/api/workspace/attach") => {
                let name = Path::new(body.filename.as_str())
                    .file_name()
                    .and_then(|v| v.to_str())
                    .context("Invalid attachment name")?;
                ensure!(
                    !name.is_empty() && name.len() <= 200,
                    "Invalid attachment name"
                );
                let path = format!(".shadow/attachments/{}-{name}", crate::id());
                self.attachment_workspace()?.write(
                    &path,
                    body.text.as_str().as_bytes(),
                    Some("missing"),
                )?;
                Ok(json!({"path":path,"kind":"text"}))
            }
            ("POST", "/api/workspace/attach-image") => {
                let name = Path::new(body.filename.as_str())
                    .file_name()
                    .and_then(|v| v.to_str())
                    .context("Invalid image filename")?;
                let bytes = crate::vision::decode_data_base64(body.data_base64.as_str())?;
                let ws = self.attachment_workspace()?;
                let stored = crate::vision::store_attachment(&ws, name, &bytes)?;
                Ok(
                    json!({"path":stored.path,"mime":stored.mime,"bytes":stored.bytes,"kind":"image"}),
                )
            }
            _ => Err(call.unavailable()),
        }
    }
    /// POST /api/workspace/exec: the terminal Run button. The exact command
    /// was supplied by the user; agent commands use ApprovalHub instead.
    async fn exec(&self, call: &Call) -> Result<Value> {
        let body: ExecBody = call.body()?;
        let store = self.engine.store();
        let selection = self.snapshot_selection()?;
        let workspace = if body.workspace.is_empty() {
            selection.workspace.clone()
        } else {
            expand_path(body.workspace.as_str())?
        };
        let ws = self.mutable_workspace_at(&workspace)?;
        let cfg = Config::load(self.engine.paths(), Some(&ws.path))?;
        let command = body.command.as_str();
        ensure!(
            !command.trim().is_empty() && command.len() <= 64000,
            "Invalid command"
        );
        if let Decision::Deny(reason) =
            permissions::check(&cfg.permissions, "exec", &json!({"command":command}))
        {
            bail!(reason);
        }
        let session = body.session_id.0.clone().or_else(|| {
            (ws.path == selection.workspace)
                .then_some(selection.session)
                .flatten()
        });
        if let Some(sid) = &session {
            ensure!(
                store.session(sid)?.context("Session not found")?["workspace"].as_str()
                    == ws.path.to_str(),
                "Session belongs to a different workspace"
            );
        }
        let spec = ProcessSpec::shell(
            command,
            ws.path.clone(),
            Duration::from_secs(
                body.timeout
                    .0
                    .unwrap_or(60)
                    .clamp(1, cfg.agent.tool_timeout_sec),
            ),
        );
        let result = process::run(spec, ws.reservation.cancellation(), None).await?;
        // The stored transcript is redacted; `result` stays raw for the caller.
        let mut event = json!({"command":command,"result":result});
        crate::redaction::redact_value(&mut event);
        store.add_event("terminal.completed", &event, session.as_deref(), None)?;
        let mut result = json!(result);
        result["command"] = json!(command);
        Ok(result)
    }
}
