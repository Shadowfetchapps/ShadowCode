use super::*;
use crate::workflows;

const BUILTINS: &[(&str, &str, &str)] = &[
    (
        "understand",
        "Inspect project structure and test candidates without a model",
        "[--save]",
    ),
    (
        "doctor",
        "Inspect native runtime, profile and project diagnostics",
        "[--test-model]",
    ),
    (
        "why",
        "Show recorded change history and current diffs",
        "[path]",
    ),
    ("help", "List native commands and project workflows", ""),
    ("shadowcode", "Show workspace and runtime information", ""),
    (
        "status",
        "Show the selected model, permissions, and active work",
        "",
    ),
    ("model", "Show or select a registered model", "[id]"),
    ("models", "List registered and detected models", ""),
    ("router", "Show or enable model routing", "[on|off]"),
    ("plan", "Plan a task with read-only tools", "<task>"),
    (
        "review",
        "Review project changes with read-only tools",
        "[focus]",
    ),
    (
        "test",
        "Run an explicit test command or ask the Test model to verify",
        "[command]",
    ),
    ("run", "Run an exact terminal command", "<command>"),
    ("git", "Show repository status", ""),
    ("diff", "Show pending changes", "[path]"),
    ("new", "Start a new conversation", ""),
    (
        "clear",
        "Start a new conversation without deleting history",
        "",
    ),
    ("sessions", "Open saved conversations", ""),
    ("branch", "Branch this conversation", "[title]"),
    (
        "resume",
        "Open a saved conversation by unique ID prefix",
        "<id>",
    ),
    (
        "pin",
        "Bookmark the latest response (kept word for word when the conversation is shortened)",
        "[label]",
    ),
    (
        "compact",
        "Shorten this conversation at its next message",
        "[what to keep]",
    ),
    ("cost", "Show recorded tokens; prices are not assumed", ""),
    ("context", "Show context usage and the configured limit", ""),
    ("skills", "Open reusable project skills", ""),
    ("skill", "Run a selected project skill", "<name> [context]"),
    ("goals", "Open durable goals", ""),
    ("goal", "Create and run a goal", "<instruction>"),
    (
        "background",
        "Manage project servers and watchers",
        "[list|start <name> <command>|stop <id>]",
    ),
    (
        "memory",
        "Read or append project notes; --task selects task notes",
        "[--task <id>] [note]",
    ),
    (
        "checkpoints",
        "List file-tool checkpoints in this conversation",
        "",
    ),
    (
        "undo",
        "Rewind the latest available file-tool checkpoint",
        "",
    ),
    ("rollback", "Restore a task checkpoint", "<task-id>"),
    ("settings", "Open settings", ""),
    ("health", "Open workspace health and routing", ""),
    ("expand", "Toggle the latest tool card", ""),
    ("quit", "Close ShadowCode with managed cleanup", ""),
    ("exit", "Close ShadowCode with managed cleanup", ""),
];
fn card(headline: &str, body: impl Into<String>) -> Value {
    json!({"handled":true,"text":"","kind":"card","icon":"◆","headline":headline,"body":body.into(),"items":[],"diff":"","path":"","approval_action":"","approval_reason":"","overlay":"","quit":false,"passthrough":false,"metadata":{}})
}
fn list(headline: &str, items: Vec<Value>) -> Value {
    let mut result = card(headline, "");
    result["kind"] = json!("list");
    result["items"] = json!(items);
    result
}
fn panel(name: &str) -> Value {
    let mut result = card("Open panel", "");
    result["metadata"]["panel"] = json!(name);
    result
}
fn split(value: &str) -> (&str, &str) {
    value
        .trim()
        .split_once(char::is_whitespace)
        .map(|(a, b)| (a, b.trim_start()))
        .unwrap_or((value.trim(), ""))
}

impl Service {
    /// Project skills and commands merged with the user's profile.
    pub(super) fn workflow_catalog(&self, workspace: &Workspace) -> workflows::Catalog {
        crate::rulebook::Book::load(self.engine.paths(), Some(&workspace.path)).catalog(workspace)
    }
    pub(super) fn command_catalog(&self) -> Result<Value> {
        let catalog = self.workflow_catalog(&Workspace::open(&self.workspace()?)?);
        let mut commands:Vec<Value>=BUILTINS.iter().map(|(name,description,args)|json!({"name":name,"description":description,"arg_spec":args,"alias":"","source":"builtin","kind":"builtin"})).collect();
        let mut issues = catalog.issues.clone();
        for definition in &catalog.definitions {
            if BUILTINS.iter().any(|entry| entry.0 == definition.info.name) {
                if definition.info.kind == "skill" {
                    let mut row = definition.command_row();
                    row["name"] = json!(format!("skill {}", definition.info.name));
                    commands.push(row);
                } else {
                    issues.push(format!(
                        "{} conflicts with built-in /{}; rename this project command",
                        definition.info.path, definition.info.name
                    ));
                }
            } else if catalog.resolve(&definition.info.name, None).is_ok() {
                commands.push(definition.command_row());
            } else {
                issues.push(format!(
                    "Ambiguous workflow name '{}' in {}",
                    definition.info.name, definition.info.path
                ));
            }
        }
        commands.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        Ok(json!({"commands":commands,"issues":issues}))
    }
    fn command_api(
        &self,
        method: &str,
        path: String,
        body: Value,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value>> + Send + '_>> {
        let request = Request {
            method: method.into(),
            path,
            body,
        };
        Box::pin(async move { self.dispatch(request).await })
    }
    pub(super) async fn run_command(&self, body: &Value) -> Result<Value> {
        let name = body["name"].as_str().unwrap_or("");
        let args = body["args"].as_str().unwrap_or("");
        ensure!(workflows::valid_name(name), "Invalid slash command name");
        ensure!(args.len() <= 64000, "Command arguments exceed 64 KB");
        if self.job_owner.is_some() {
            ensure!(
                !BUILTINS.iter().any(|entry| entry.0 == name)
                    || matches!(name, "plan" | "review" | "skill")
                    || (name == "test" && args.trim().is_empty()),
                "Task ownership accepts only model workflows; use a test job for terminal commands"
            );
        }
        let selection = self.snapshot_selection()?;
        let workspace = selection.workspace.clone();
        let config = Config::load(self.engine.paths(), Some(&workspace))?;
        let store = self.engine.store();
        let sid = body["session_id"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
            .or(selection.session.clone());
        if let Some(sid) = &sid {
            ensure!(
                store.session(sid)?.context("Session not found")?["workspace"].as_str()
                    == workspace.to_str(),
                "Session belongs to a different workspace"
            );
        }
        let api =
            |method: &str, path: &str, value: Value| self.command_api(method, path.into(), value);
        let result = match name {
            "understand" => {
                ensure!(
                    matches!(args.trim(), "" | "--save"),
                    "Usage: /understand [--save]"
                );
                let map = self.project_map(args.trim() == "--save").await?;
                let mut value = card(
                    if map["saved"] == true {
                        "Project map saved"
                    } else {
                        "Project map"
                    },
                    map["text"].as_str().unwrap_or(""),
                );
                value["metadata"] = map;
                value
            }
            "doctor" => {
                ensure!(
                    matches!(args.trim(), "" | "--test-model"),
                    "Usage: /doctor [--test-model]"
                );
                let report = self.doctor(args.trim() == "--test-model").await?;
                let mut value=list("Native diagnostics",report["checks"].as_array().unwrap().iter().map(|c|json!({"label":format!("{} · {}",c["status"].as_str().unwrap_or(""),c["label"].as_str().unwrap_or("")),"value":c["detail"]})).collect());
                value["metadata"] = report;
                value
            }
            "why" => {
                let report = self.change_history(args.trim(), 8).await?;
                let mut value = card(
                    "Recorded change history",
                    format!(
                        "{}\n\nWorking tree:\n{}\nStaged:\n{}\n{}",
                        report["log"].as_str().unwrap_or(""),
                        report["diff"]["diff"].as_str().unwrap_or(""),
                        report["diff"]["staged"].as_str().unwrap_or(""),
                        report["note"].as_str().unwrap_or("")
                    ),
                );
                value["diff"] = report["diff"]["diff"].clone();
                value["metadata"] = report;
                value
            }
            "help" => {
                let catalog = self.command_catalog()?;
                let mut result=list("Commands",catalog["commands"].as_array().unwrap().iter().map(|row|json!({"label":format!("/{} {}",row["name"].as_str().unwrap_or(""),row["arg_spec"].as_str().unwrap_or("")),"value":row["description"]})).collect());
                result["body"] = json!(catalog["issues"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n"));
                result
            }
            "shadowcode" | "status" => list(
                "ShadowCode",
                vec![
                    json!({"label":"Version","value":crate::VERSION}),
                    json!({"label":"Workspace","value":workspace}),
                    json!({"label":"Model","value":format!("{} · {}",config.model.name,config.model.provider)}),
                    json!({"label":"Permission mode","value":format!("{:?}",config.permissions.level)}),
                    json!({"label":"Active tasks","value":store.jobs(1000)?.iter().filter(|job|job["workspace"].as_str()==workspace.to_str()&&active(job)).count().to_string()}),
                ],
            ),
            "models" => {
                let models = api("GET", "/api/models", Value::Null).await?;
                list("Models",models["models"].as_array().unwrap().iter().map(|model|json!({"label":format!("{} · {}",model["name"].as_str().unwrap_or(""),model["provider"].as_str().unwrap_or("")),"value":model["id"]})).collect())
            }
            "model" => {
                if args.trim().is_empty() {
                    card(
                        "Selected model",
                        format!(
                            "{} · {}\n{}",
                            config.model.name, config.model.provider, config.model.default
                        ),
                    )
                } else {
                    let selected =
                        api("POST", "/api/models/select", json!({"id":args.trim()})).await?;
                    let mut result = card(
                        "Default model selected",
                        selected["model"]["name"].as_str().unwrap_or("Model"),
                    );
                    result["metadata"]["reload_config"] = json!(true);
                    result
                }
            }
            "router" => {
                let view = match args.trim() {
                    "" => api("GET", "/api/routing", Value::Null).await?,
                    "on" | "auto" => {
                        api("PUT", "/api/routing", json!({"values":{"enabled":true}})).await?
                    }
                    "off" => {
                        api("PUT", "/api/routing", json!({"values":{"enabled":false}})).await?
                    }
                    _ => bail!("Usage: /router [on|off]"),
                };
                let mut result=list("Model routing",view["decisions"].as_object().context("Routing details unavailable")?.iter().map(|(purpose,decision)|json!({"label":purpose,"value":format!("{} · {}{}",decision["model_name"].as_str().unwrap_or(""),decision["provider"].as_str().unwrap_or(""),decision["fallback_reason"].as_str().map(|reason|format!(" · fallback: {reason}")).unwrap_or_default())})).collect());
                result["body"] = json!(if view["enabled"] == true {
                    "Routing enabled"
                } else {
                    "Routing disabled"
                });
                result["metadata"]["reload_config"] = json!(true);
                result
            }
            "sessions" | "skills" | "goals" | "health" => {
                ensure!(
                    args.trim().is_empty(),
                    "This command opens a panel and takes no arguments"
                );
                panel(name)
            }
            "settings" => {
                ensure!(
                    args.trim().is_empty(),
                    "Use the Settings form to change configuration"
                );
                let mut result = card("Settings", "");
                result["kind"] = json!("overlay");
                result["overlay"] = json!("settings");
                result
            }
            "expand" | "quit" | "exit" => {
                ensure!(args.trim().is_empty(), "This command takes no arguments");
                let mut result = card("Command", "");
                result["metadata"]["action"] = json!(if name == "exit" { "quit" } else { name });
                result
            }
            "new" | "clear" => {
                let session = api("POST", "/api/sessions", json!({"workspace":workspace})).await?;
                let mut result = card("New conversation", "");
                result["metadata"]["session_id"] = session["id"].clone();
                result
            }
            "branch" => {
                let sid = sid
                    .as_ref()
                    .context("Start a conversation before branching")?;
                let session = api(
                    "POST",
                    &format!("/api/sessions/{sid}/branch"),
                    json!({"title":args}),
                )
                .await?;
                let mut result = card("Conversation branched", "");
                result["metadata"]["session_id"] = session["id"].clone();
                result
            }
            "resume" => {
                ensure!(!args.trim().is_empty(), "Usage: /resume <id-prefix>");
                let matches: Vec<_> = store
                    .sessions("", 10000)?
                    .into_iter()
                    .filter(|session| {
                        session["id"]
                            .as_str()
                            .is_some_and(|id| id.starts_with(args.trim()))
                    })
                    .collect();
                ensure!(
                    matches.len() == 1,
                    "Choose a unique saved session ID prefix"
                );
                let session = &matches[0];
                let id = session["id"].as_str().context("Session ID missing")?;
                api("POST", &format!("/api/sessions/{id}/activate"), json!({})).await?;
                let mut result = card("Conversation resumed", "");
                result["metadata"]["session_id"] = json!(id);
                result
            }
            "compact" => {
                let sid = sid
                    .as_ref()
                    .context("Start a conversation before shortening it")?;
                let focus = crate::tools::truncate(args.trim(), 1000).to_owned();
                store.set_session_meta(sid, crate::store::keys::COMPACT_REQUEST, &focus)?;
                card(
                    "Compact",
                    &if focus.is_empty() {
                        "The conversation will be shortened when you send the next message: earlier steps become a summary, and pinned answers and folder rules are kept.".to_owned()
                    } else {
                        format!("The conversation will be shortened when you send the next message, keeping: {focus}. Pinned answers and folder rules are kept too.")
                    },
                )
            }
            "pin" => {
                let sid = sid
                    .as_ref()
                    .context("Start a conversation before pinning a response")?;
                let events = store.recent_events(sid, 2000)?;
                let text = events
                    .iter()
                    .rev()
                    .find_map(|event| match event["type"].as_str() {
                        Some("model.delta") => event["payload"]["text"].as_str(),
                        Some("agent.completed") => event["payload"]["summary"].as_str(),
                        _ => None,
                    })
                    .filter(|text| !text.is_empty())
                    .context("No completed response to pin")?;
                store.add_pin(
                    sid,
                    if args.trim().is_empty() {
                        "Saved response"
                    } else {
                        args.trim()
                    },
                    text,
                )?;
                card(
                    "Response pinned",
                    "Available from this conversation's pins.",
                )
            }
            "cost" | "context" => {
                let usage = if let Some(sid) = &sid {
                    store
                        .session(sid)?
                        .and_then(|session| {
                            session["usage_json"]
                                .as_str()
                                .and_then(|text| serde_json::from_str::<Value>(text).ok())
                        })
                        .unwrap_or(json!({}))
                } else {
                    json!({})
                };
                let mut result = card("Recorded usage", serde_json::to_string_pretty(&usage)?);
                result["body"] = json!(format!(
                    "{}\n{}.\nConfigured context limit: {} tokens.",
                    result["body"].as_str().unwrap_or(""),
                    crate::usage::describe(&crate::usage::parse(&usage)),
                    config.model.context_limit
                ));
                result
            }
            "git" => {
                let state = self.git_status().await?;
                card(
                    "Repository status",
                    state["status"].as_str().unwrap_or("No repository"),
                )
            }
            "diff" => {
                let diff = self.git_diff(args.trim()).await?;
                let mut result = card("Changes", "");
                result["kind"] = json!("diff");
                result["path"] = json!(args);
                result["diff"] = diff["diff"].clone();
                result
            }
            "run" | "test" if !args.trim().is_empty() => {
                let output = api(
                    "POST",
                    "/api/workspace/exec",
                    json!({"command":args,"timeout":config.agent.tool_timeout_sec,"session_id":sid,"workspace":workspace}),
                )
                .await?;
                let mut result = card(
                    if output["ok"] == true {
                        "Command completed"
                    } else {
                        "Command failed"
                    },
                    format!(
                        "$ {args}\n{}{}\nExit: {}{}",
                        output["stdout"].as_str().unwrap_or(""),
                        output["stderr"].as_str().unwrap_or(""),
                        output["exit_code"],
                        if output["truncated"] == true {
                            " · output truncated"
                        } else {
                            ""
                        }
                    ),
                );
                if output["ok"] != true {
                    result["kind"] = json!("error");
                }
                result
            }
            "run" => bail!("Usage: /run <command>"),
            "goal" => {
                ensure!(!args.trim().is_empty(), "Usage: /goal <instruction>");
                let goal = api(
                    "POST",
                    "/api/goals",
                    json!({"instruction":args,"run":true,"workspace":workspace}),
                )
                .await?;
                let mut result = panel("goals");
                result["metadata"]["session_id"] = goal["session_id"].clone();
                result
            }
            "background" => {
                let (action, rest) = split(args);
                match action {
                    ""=>panel("background"),
                    "list"=>list("Project processes",self.engine.background().list(&workspace)?.into_iter().map(|task|json!({"label":format!("{} · {}",task.name,task.status),"value":task.id})).collect()),
                    "start" => {
                        let (name, command) = split(rest);
                        let task = self.engine.background().start(&workspace, &config, sid.clone(), name, command)?;
                        card("Background process started", format!("{} · {}", task.name, task.id))
                    }
                    "stop" => {
                        let task = self.engine.background().get(rest.trim())?;
                        ensure!(Path::new(&task.cwd) == workspace, "Switch to this process's project before managing it");
                        let task = self.engine.background().stop(&task.id).await?;
                        card("Background process", format!("{} · {}", task.name, task.status))
                    }
                    _=>bail!("Usage: /background [list|start <name> <command>|stop <id>]"),
                }
            }
            "memory" => {
                let (scope, task_id, note) = if let Some(rest) = args.trim().strip_prefix("--task ")
                {
                    let (id, note) = split(rest);
                    ("task", Some(id), note)
                } else {
                    ("project", None, args.trim())
                };
                let report=self.memory(&json!({"scope":scope,"task_id":task_id,"session_id":sid,"action":if note.is_empty(){"read"}else{"append"},"note":note}))?;
                let mut result = card(
                    if note.is_empty() {
                        "Project and task notes"
                    } else {
                        "Note saved"
                    },
                    report["text"].as_str().unwrap_or(""),
                );
                result["metadata"] = report;
                result
            }
            "checkpoints" | "undo" | "rollback" => {
                let sid = sid
                    .as_ref()
                    .context("Select a conversation with file changes")?;
                let ws = Workspace::open(&workspace)?;
                let mut checkpoints = Vec::new();
                for task in store.tasks(sid, 10000)? {
                    let id = task["id"].as_str().context("Task ID missing")?;
                    let checkpoint = checkpoint::summary(&store, &ws, id)?;
                    if checkpoint["changes"].as_u64().unwrap_or(0) > 0 {
                        checkpoints.push(checkpoint);
                    }
                }
                if name == "checkpoints" {
                    list("File checkpoints",checkpoints.iter().map(|point|json!({"label":point["task_id"],"value":format!("{} file(s){}",point["changes"],if point["restored"]==true{" · restored"}else{""})})).collect())
                } else {
                    let target = if name == "rollback" {
                        checkpoints
                            .iter()
                            .find(|point| point["task_id"] == args.trim())
                            .context("Choose a task ID from /checkpoints")?
                    } else {
                        checkpoints
                            .iter()
                            .find(|point| point["restored"] != true)
                            .context("No file checkpoint is available to rewind")?
                    };
                    let id = target["task_id"].as_str().unwrap();
                    let restored = api(
                        "POST",
                        &format!("/api/checkpoints/tasks/{id}/restore"),
                        json!({}),
                    )
                    .await?;
                    card(
                        "File checkpoint restored",
                        serde_json::to_string_pretty(&restored["restored"])?,
                    )
                }
            }
            _ => {
                return self
                    .run_workflow(name, args, body, sid, selection, config)
                    .await
            }
        };
        // Durable command cards survive reload without pretending to be model output.
        if result["metadata"]["panel"].is_null()
            && result["metadata"]["action"].is_null()
            && result["metadata"]["session_id"].is_null()
            && result["metadata"]["job"].is_null()
            && result["kind"] != "overlay"
        {
            if let Some(sid) = sid {
                // The stored transcript is redacted; `result` stays raw for
                // the caller and follows the API's own redaction on return.
                let mut event = json!({"name":name,"result":result});
                crate::redaction::redact_value(&mut event);
                store.add_event("command.completed", &event, Some(&sid), None)?;
            }
        }
        Ok(result)
    }
    async fn run_workflow(
        &self,
        name: &str,
        args: &str,
        body: &Value,
        sid: Option<String>,
        selection: Selection,
        config: Config,
    ) -> Result<Value> {
        let workspace = selection.workspace;
        ensure!(
            config.is_trusted(&workspace),
            "Trust this project before starting a workflow"
        );
        let catalog = self.workflow_catalog(&Workspace::open(&workspace)?);
        let invocation = format!("/{name} {args}").trim().to_owned();
        let (definition, args) = if name == "skill" {
            let (skill, args) = split(args);
            ensure!(!skill.is_empty(), "Usage: /skill <name> [context]");
            (Some(catalog.resolve(skill, Some("skill"))?), args)
        } else if BUILTINS.iter().any(|entry| entry.0 == name) {
            (None, args)
        } else {
            (Some(catalog.resolve(name, None)?), args)
        };
        let requested = body["purpose"].as_str().unwrap_or("coder");
        let requested_mode = match requested {
            "planner" | "plan" => "plan",
            "reviewer" | "review" => "review",
            _ => "code",
        };
        let chosen = match name {
            "plan" => "plan",
            "review" => "review",
            "test" => "test",
            _ => definition
                .map(|item| item.info.mode.as_str())
                .filter(|mode| !mode.is_empty())
                .unwrap_or(requested_mode),
        };
        let mode = if requested_mode != "code" {
            requested_mode
        } else if chosen == "test" {
            "code"
        } else {
            chosen
        };
        let purpose = if mode == "plan" {
            "planner"
        } else if mode == "review" {
            "reviewer"
        } else if chosen == "test" || requested == "tester" {
            "tester"
        } else {
            "coder"
        };
        if name == "plan" {
            ensure!(!args.trim().is_empty(), "Usage: /plan <task>");
        }
        let task=match name {"review" if args.trim().is_empty()=>"Review the current repository changes for concrete bugs and missing verification. Inspect the files and diff; report only supported findings.".into(),"test" if args.trim().is_empty()=>"Inspect the project, determine its relevant test command, run the checks with approval when required, and report the actual result.".into(),_=>invocation};
        let model = body["model"]
            .as_str()
            .filter(|value| !value.is_empty())
            .map(|id| self.resolve_model(id, &config.model))
            .transpose()?;
        let request = StartRequest {
            workspace: workspace.clone(),
            task,
            session_id: sid,
            model,
            mode: mode.into(),
            queue: body["queue"].as_bool().unwrap_or(false),
            images: Vec::new(),
            web: false,
        };
        let turn = crate::engine::TurnOptions {
            max_cost_usd: crate::spending::parse_max_cost(body.get("max_cost_usd"))?,
            ..Default::default()
        };
        let guidance = definition.map(|d| d.guidance(args)).transpose()?;
        let job = self
            .engine
            .start_workflow_owned(request, purpose, guidance, self.job_owner.as_ref(), turn)
            .await?;
        self.select_if(
            &workspace,
            Some(job.session_id.clone()),
            Some(selection.generation),
        )?;
        let mut result = card("Workflow started", "");
        result["metadata"]["job"] = json!(job);
        Ok(result)
    }
}
