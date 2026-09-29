//! `/api/sessions…`, `/api/projects…`, `/api/events` and `/api/resolve`:
//! conversations, their transcripts and exports, and the project list. Every
//! route here is synchronous database work, so the router runs this module on
//! the blocking pool.
use super::*;
use crate::store::keys;

#[derive(Default, Deserialize)]
#[serde(default)]
struct SessionBody {
    workspace: Text,
    title: Text,
    target_id: Text,
    label: Text,
    body: Text,
    event_id: Option<Value>,
    /// Fork: keep only the events before `event_id` (Edit & resend forks
    /// just before the edited message).
    before: Flag,
    /// Scheduled resume: the task stopped at a plan limit (default: the
    /// conversation's latest), and whether the user agreed to hand the
    /// conversation to that cloud route.
    job_id: Text,
    handoff_consent: Flag,
}

impl Service {
    pub(super) fn session_routes(&self, call: &Call) -> Result<Value> {
        let store = self.engine.store();
        let body: SessionBody = call.body()?;
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/resolve") => {
                return Ok(json!({"id":store.resolve_id(call.q("kind"),call.q("prefix"))?}))
            }
            ("GET", "/api/projects") => return Ok(json!({"projects":store.projects()?})),
            ("POST", "/api/projects" | "/api/projects/trust") => return self.open_project(call),
            ("GET", "/api/sessions") => {
                return Ok(
                    json!({"sessions":store.sessions_listed_with(call.q("q"),call.limit(100,10000),(!call.q("workspace").is_empty()).then(||Path::new(call.q("workspace"))),matches!(call.q("include_compare"),"true"|"1"),matches!(call.q("include_subagents"),"true"|"1"))?}),
                )
            }
            ("POST", "/api/sessions") => {
                let workspace = if body.workspace.is_empty() {
                    self.workspace()?
                } else {
                    Workspace::open(&expand_path(body.workspace.as_str())?)?.path
                };
                let cfg = Config::load(self.engine.paths(), Some(&workspace))?;
                let session =
                    store.create_session(&workspace, &cfg.model.default, body.title.as_str())?;
                self.select(&workspace, session["id"].as_str().map(str::to_owned))?;
                return Ok(session);
            }
            ("GET", "/api/events") => {
                let sid = if !call.q("session_id").is_empty() {
                    call.q("session_id").to_owned()
                } else {
                    self.current_session()?.unwrap_or_default()
                };
                return Ok(json!({"events":store.recent_events(&sid,call.limit(240,10000))?}));
            }
            _ => {}
        }
        let parts = call.parts();
        if call.family() != "sessions" || parts.len() < 3 {
            return Err(call.unavailable());
        }
        let sid = parts[2];
        let session = store.session(sid)?.context("Session not found")?;
        match (call.method.as_str(), parts.get(3).copied()) {
            ("GET", None) => {
                if call.q("summary") == "true" {
                    Ok(session)
                } else {
                    self.session(sid, call.q("view") == "window")
                }
            }
            ("PATCH", None) => {
                store.rename_session(sid, body.title.as_str())?;
                Ok(json!({"ok":true}))
            }
            ("POST", Some("target")) => {
                // Remembered per conversation; a running job keeps the
                // target it started with, so this applies to the next turn.
                let target = body.target_id.as_str().trim();
                ensure!(
                    !target.is_empty() && target.len() <= 1024,
                    "target_id must be a picker id"
                );
                let workspace = PathBuf::from(
                    session["workspace"]
                        .as_str()
                        .context("Session missing workspace")?,
                );
                let cfg = Config::load(self.engine.paths(), Some(&workspace))?;
                let model = self.resolve_model(target, &cfg.model)?;
                store.set_session_meta(sid, keys::EXECUTION_TARGET, target)?;
                store.set_native_meta(&keys::execution_target(&workspace), target)?;
                let running = store.current_job(sid, false)?.is_some();
                Ok(json!({
                    "ok": true,
                    "execution_target": target,
                    "provider": model.provider,
                    "applies_to": if running { "next_turn" } else { "this_turn" },
                }))
            }
            // "Resume at <time>" after a plan limit (`crate::resume`).
            ("GET", Some("scheduled-resume")) => Ok(json!({
                "resume": crate::resume::for_session(&store, sid)?,
                "scheduler": self.engine.automations_scheduled(),
            })),
            ("POST", Some("scheduled-resume")) => {
                let job = store
                    .current_job(sid, true)?
                    .and_then(|job| job["id"].as_str().map(str::to_owned));
                let job_id = body.job_id.non_empty().map(str::to_owned).or(job);
                let job_id = job_id.context("Choose the task to resume")?;
                let owner = self.engine.job(&job_id)?.context("Job not found")?;
                ensure!(
                    owner.session_id == sid,
                    "That task belongs to another conversation"
                );
                let resume = self
                    .engine
                    .schedule_resume(&job_id, body.handoff_consent.is_true())?;
                Ok(json!({"resume": resume, "scheduler": self.engine.automations_scheduled()}))
            }
            ("DELETE", Some("scheduled-resume")) => Ok(json!({
                "resume": self.engine.cancel_resume(sid)?,
            })),
            ("DELETE", None) => {
                self.engine.delete_session(sid)?;
                let mut selection = self
                    .selection
                    .write()
                    .map_err(|_| anyhow::anyhow!("Project lock poisoned"))?;
                if selection.session.as_deref() == Some(sid) {
                    selection.session = None;
                }
                Ok(json!({"ok":true}))
            }
            ("POST", Some("activate")) => {
                self.select(
                    Path::new(
                        session["workspace"]
                            .as_str()
                            .context("Session missing workspace")?,
                    ),
                    Some(sid.into()),
                )?;
                self.session(sid, call.q("view") == "window")
            }
            ("POST", Some("branch")) => {
                let workspace = PathBuf::from(
                    store.session(sid)?.context("Conversation does not exist")?["workspace"]
                        .as_str()
                        .context("Conversation has no workspace")?,
                );
                let memory = crate::memory::archive(self.engine.paths(), &store, &workspace, sid)?;
                store.branch_session_with_memory(sid, body.title.as_str(), &memory)
            }
            ("POST", Some("fork")) => {
                let event_id = body
                    .event_id
                    .as_ref()
                    .and_then(|id| id.as_i64().or_else(|| id.as_u64().map(|v| v as i64)))
                    .context("event_id required")?;
                if body.before.is_true() {
                    store.fork_session_before_event(sid, event_id, body.title.as_str())
                } else {
                    store.fork_session_from_event(sid, event_id, body.title.as_str())
                }
            }
            ("GET", Some("events")) => self.session_events(call, sid),
            ("GET", Some("export")) => self.export(sid, call.q("format")),
            ("GET", Some("cost")) => {
                let tasks: Vec<_> = store.tasks(sid, 10000)?.into_iter().map(|task| json!({"task_id":task["id"],"prompt":task["prompt"],"status":task["status"],"usage":task["usage_json"].as_str().and_then(|v|serde_json::from_str::<Value>(v).ok()).unwrap_or(json!({}))})).collect();
                Ok(
                    json!({"session_id":sid,"tasks":tasks,"usage":session["usage"],"cost":session["usage"]["cost_usd"],"cost_estimated":session["usage"]["cost_estimated"],"note":crate::usage::describe(&crate::usage::parse(&session["usage"]))}),
                )
            }
            ("GET", Some("pins")) => Ok(json!({"pins":store.pins(sid)?})),
            ("POST", Some("pins")) => {
                Ok(json!({"id":store.add_pin(sid,body.label.as_str(),body.body.as_str())?}))
            }
            ("DELETE", Some("pins")) => {
                store.delete_pin(sid, parts.get(4).context("Pin ID required")?.parse()?)?;
                Ok(json!({"ok":true}))
            }
            _ => Err(call.unavailable()),
        }
    }
    /// POST /api/projects (open) and /api/projects/trust (trust, then open):
    /// an untrusted folder answers `needs_trust` instead of opening.
    fn open_project(&self, call: &Call) -> Result<Value> {
        let store = self.engine.store();
        let workspace = Workspace::open(&expand_path(call.text("path"))?)?.path;
        let cfg = if call.path.ends_with("trust") {
            Config::update(self.engine.paths(), |cfg| {
                cfg.grant_trust(&workspace);
                Ok(())
            })?
        } else {
            Config::load(self.engine.paths(), None)?
        };
        if !cfg.is_trusted(&workspace) {
            return Ok(
                json!({"needs_trust":true,"path":workspace,"name":workspace.file_name(),"permissions":cfg.permissions}),
            );
        }
        let session = store.create_session(
            &workspace,
            &cfg.model.default,
            workspace
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("Project"),
        )?;
        let sid = session["id"]
            .as_str()
            .context("Missing session ID")?
            .to_owned();
        self.select(&workspace, Some(sid.clone()))?;
        Ok(json!({"ok":true,"needs_trust":false,"path":workspace,"session_id":sid}))
    }
    /// GET /api/sessions/<id>/events: a history window (`view=window`), a
    /// page before a cursor, or everything after one.
    fn session_events(&self, call: &Call, sid: &str) -> Result<Value> {
        let store = self.engine.store();
        if call.q("view") == "window" {
            let through = if call.q("before").is_empty() {
                store.event_cursor(sid)?
            } else {
                let before: i64 = call.q("before").parse().context("Invalid history cursor")?;
                ensure!(before > 0, "History cursor must be positive");
                (before - 1).min(store.event_cursor(sid)?)
            };
            return store.history_page(sid, through);
        }
        if !call.q("before").is_empty() {
            let before: i64 = call.q("before").parse().context("Invalid history cursor")?;
            ensure!(before > 0, "History cursor must be positive");
            return Ok(
                json!({"events":store.recent_events_through(sid,before-1,call.limit(256,2000))?}),
            );
        }
        Ok(
            json!({"events":store.events_after(sid,call.q("after").parse().unwrap_or(0),None,call.limit(512,10000))?}),
        )
    }
    pub(super) fn session(&self, id: &str, window: bool) -> Result<Value> {
        let store = self.engine.store();
        let mut session = store.session(id)?.context("Session not found")?;
        let cursor = store.event_cursor(id)?;
        if window {
            let page = store.history_page(id, cursor)?;
            session["tasks"] = json!([]);
            session["events"] = page["events"].clone();
            session["history_page"] =
                json!({"first_cursor":page["first_cursor"],"has_older":page["has_older"]});
        } else {
            session["tasks"] = json!(store.tasks(id, 10000)?);
            session["events"] = json!(store.recent_events_through(id, cursor, 10000)?);
        }
        session["event_cursor"] = json!(cursor);
        session["execution_target"] = json!(store.session_meta(id, keys::EXECUTION_TARGET)?);
        let (compare_id, compare_lane) = crate::compare::session_tags(&store, id)?;
        session["compare_id"] = json!(compare_id);
        session["compare_lane"] = json!(compare_lane);
        // The worktree task this conversation runs in (as last saved; GET
        // /api/worktree-tasks/{id} refreshes it).
        session["worktree"] = match crate::worktree_tasks::session_task(&store, id)? {
            Some(task) => crate::worktree_tasks::load(&store, &task)
                .map(|record| record.to_json())
                .unwrap_or(Value::Null),
            None => Value::Null,
        };
        session["subagent_parent"] = json!(store.session_meta(id, keys::SUBAGENT_PARENT)?);
        session["subagent_run"] = json!(store.session_meta(id, keys::SUBAGENT_RUN)?);
        let native: serde_json::Map<String, Value> = store
            .session_meta_prefixed(id, keys::NATIVE_SESSION_PREFIX)?
            .into_iter()
            .map(|(key, value)| {
                (
                    key[keys::NATIVE_SESSION_PREFIX.len()..].to_owned(),
                    json!(value),
                )
            })
            .collect();
        session["native_sessions"] = Value::Object(native);
        Ok(session)
    }
    fn export(&self, id: &str, format: &str) -> Result<Value> {
        let store = self.engine.store();
        let mut session = store.session(id)?.context("Session not found")?;
        let cursor = store.event_cursor(id)?;
        let mut after = 0;
        let mut events = Vec::new();
        let mut bytes = 0;
        loop {
            let page = store.events_after(id, after, Some(cursor), 1000)?;
            if page.is_empty() {
                break;
            }
            for event in &page {
                bytes += event.to_string().len();
            }
            ensure!(
                bytes <= 32_000_000,
                "This conversation exceeds the 32 MB export limit"
            );
            after = page
                .last()
                .and_then(|v| v["id"].as_i64())
                .context("Missing event cursor")?;
            events.extend(page);
        }
        session["events"] = json!(events);
        session["event_cursor"] = json!(cursor);
        session["tasks"] = json!(store.tasks(id, 10000)?);
        if format == "json" {
            let content = serde_json::to_string_pretty(&self.export_memory(session)?)?;
            ensure!(
                content.len() <= 32_000_000,
                "This conversation exceeds the 32 MB export limit"
            );
            return Ok(
                json!({"filename":format!("shadowcode-{id}.json"),"content":content,"mime":"application/json"}),
            );
        }
        let mut text = format!(
            "# {}\n\nWorkspace: `{}`\n\n",
            session["title"].as_str().unwrap_or("ShadowCode task"),
            session["workspace"].as_str().unwrap_or("")
        );
        for event in session["events"].as_array().into_iter().flatten() {
            match event["type"].as_str().unwrap_or("") {
                "user.message" => text.push_str(&format!(
                    "## User\n\n{}\n\n",
                    event["payload"]["text"].as_str().unwrap_or("")
                )),
                "model.delta" => text.push_str(&format!(
                    "## ShadowCode\n\n{}\n\n",
                    event["payload"]["text"].as_str().unwrap_or("")
                )),
                "tool.completed" => text.push_str(&format!(
                    "- Tool `{}`: {}\n",
                    event["payload"]["tool"].as_str().unwrap_or(""),
                    if event["payload"]["success"] == true {
                        "succeeded"
                    } else {
                        "failed"
                    }
                )),
                _ => {}
            }
        }
        let session = self.export_memory(session)?;
        if let Some(notes) = session["inherited_notes"]
            .as_str()
            .filter(|s| !s.is_empty())
        {
            text.push_str(&format!("\n## Inherited task notes\n\n{notes}\n"));
        }
        for (id, notes) in session["task_notes"].as_object().into_iter().flatten() {
            text.push_str(&format!(
                "\n## Task notes · {id}\n\n{}\n",
                notes.as_str().unwrap_or("")
            ));
        }
        ensure!(
            text.len() <= 32_000_000,
            "This conversation exceeds the 32 MB export limit"
        );
        Ok(json!({"filename":format!("shadowcode-{id}.md"),"content":text,"mime":"text/markdown"}))
    }
    fn export_memory(&self, mut session: Value) -> Result<Value> {
        let store = self.engine.store();
        let workspace = Path::new(
            session["workspace"]
                .as_str()
                .context("Conversation has no workspace")?,
        );
        let sid = session["id"].as_str().context("Conversation has no ID")?;
        let seed = store
            .query(
                "SELECT value FROM session_meta WHERE session_id=? AND key='memory_seed'",
                [sid],
            )?
            .pop();
        let mut notes = serde_json::Map::new();
        let mut bytes = session.to_string().len();
        for task in session["tasks"].as_array().into_iter().flatten() {
            let id = task["id"].as_str().context("Task has no ID")?;
            let note = crate::memory::task_notes(self.engine.paths(), &store, workspace, id)?;
            if !note.is_empty() {
                bytes += note.len();
                ensure!(
                    bytes <= 32_000_000,
                    "This conversation exceeds the 32 MB export limit"
                );
                notes.insert(id.to_owned(), json!(note));
            }
        }
        session["task_notes"] = json!(notes);
        session["inherited_notes"] = seed.map(|v| v["value"].clone()).unwrap_or(Value::Null);
        Ok(session)
    }
}
