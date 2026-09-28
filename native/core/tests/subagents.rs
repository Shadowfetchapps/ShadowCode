//! Subagents with fake OpenAI-compatible models: parallel children,
//! read-only enforcement, write-mode diffs, cancellation, approvals routed to
//! the parent, `@agent` mentions, and model-loaded skills.
#![cfg(unix)]
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::{Engine, Job, StartRequest},
    paths::AppPaths,
    subagents,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

type Handler = dyn Fn(&Value) -> (Value, Duration) + Send + Sync;

/// A fake chat-completions server that answers connections concurrently and
/// records the highest number of requests in flight.
struct Fake {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    peak: Arc<AtomicUsize>,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.worker.abort();
    }
}
async fn fake(handler: impl Fn(&Value) -> (Value, Duration) + Send + Sync + 'static) -> Fake {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let peak = Arc::new(AtomicUsize::new(0));
    let inflight = Arc::new(AtomicUsize::new(0));
    let handler: Arc<Handler> = Arc::new(handler);
    let (captured, top) = (requests.clone(), peak.clone());
    let worker = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let (handler, captured, top, inflight) = (
                handler.clone(),
                captured.clone(),
                top.clone(),
                inflight.clone(),
            );
            tokio::spawn(async move {
                let mut wire = Vec::new();
                let mut buffer = [0; 8192];
                let body = loop {
                    let count = socket.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    wire.extend_from_slice(&buffer[..count]);
                    if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&wire[..end]).to_lowercase();
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if wire.len() >= end + 4 + len {
                            break serde_json::from_slice::<Value>(&wire[end + 4..end + 4 + len])
                                .unwrap();
                        }
                    }
                };
                captured.lock().unwrap().push(body.clone());
                let now = inflight.fetch_add(1, Ordering::SeqCst) + 1;
                top.fetch_max(now, Ordering::SeqCst);
                let (response, delay) = handler(&body);
                tokio::time::sleep(delay).await;
                inflight.fetch_sub(1, Ordering::SeqCst);
                let text = response.to_string();
                let _ = socket
                    .write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).as_bytes())
                    .await;
            });
        }
    });
    Fake {
        endpoint,
        requests,
        peak,
        worker,
    }
}

fn response(text: &str, calls: Value) -> Value {
    let reason = if calls.as_array().is_some_and(|v| !v.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}})
}
fn tool(name: &str, args: Value) -> Value {
    json!({"id":shadowcode_core::id(),"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn system(body: &Value) -> String {
    body["messages"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_owned()
}
/// `Some(agent)` when the request comes from a subagent.
fn child_of(body: &Value) -> Option<String> {
    let system = system(body);
    let start = system.find("You are the subagent '")? + "You are the subagent '".len();
    Some(system[start..].split('\'').next()?.to_owned())
}
fn last(body: &Value) -> Value {
    body["messages"].as_array().unwrap().last().unwrap().clone()
}
fn last_user(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "user")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
        .to_owned()
}
fn tool_names(body: &Value) -> Vec<String> {
    body["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}
fn tool_output(message: &Value) -> Value {
    serde_json::from_str(message["content"].as_str().unwrap_or("{}")).unwrap_or(Value::Null)
}

fn setup(endpoint: &str, extra: Value) -> (tempfile::TempDir, Engine, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let mut config = json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","context_limit":32768},"trusted_workspaces":[project],"permissions":{"approve_shell":false},"agent":{"max_steps":12,"model_retries":0}});
    merge(&mut config, extra);
    Config::patch(&paths, config).unwrap();
    (root, Engine::open(paths).unwrap(), project)
}
fn merge(base: &mut Value, extra: Value) {
    if let (Value::Object(base), Value::Object(extra)) = (base, extra) {
        for (key, value) in extra {
            match base.get_mut(&key) {
                Some(existing) if existing.is_object() && value.is_object() => {
                    merge(existing, value)
                }
                _ => {
                    base.insert(key, value);
                }
            }
        }
    }
}
fn request(project: &Path, task: &str) -> StartRequest {
    StartRequest {
        workspace: project.to_path_buf(),
        task: task.into(),
        session_id: None,
        model: None,
        mode: "code".into(),
        queue: false,
        images: Vec::new(),
        web: false,
    }
}
async fn wait(engine: &Engine, id: &str, seconds: u64) -> Job {
    tokio::time::timeout(Duration::from_secs(seconds), engine.wait(id))
        .await
        .expect("job did not finish in time")
        .unwrap()
}
fn events(engine: &Engine, session: &str, kind: &str) -> Vec<Value> {
    engine
        .store()
        .recent_events(session, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == kind)
        .collect()
}
fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(status.status.success(), "{status:?}");
}
fn repository(project: &Path) {
    git(project, &["init", "-q"]);
    fs::write(project.join("note.txt"), "old\n").unwrap();
    git(project, &["add", "note.txt"]);
    git(project, &["commit", "-q", "-m", "init"]);
}

#[tokio::test]
async fn parallel_read_only_children_report_summaries_in_hidden_sessions() {
    let seen_child_tools = Arc::new(Mutex::new(Vec::<Vec<String>>::new()));
    let child_tools = seen_child_tools.clone();
    let server = fake(move |body| {
        if let Some(agent) = child_of(body) {
            assert_eq!(agent, "explore");
            child_tools.lock().unwrap().push(tool_names(body));
            let topic = last_user(body);
            return (
                response(&format!("Found it for {topic}"), json!([])),
                Duration::from_millis(400),
            );
        }
        if last(body)["role"] == "tool" {
            let output = tool_output(&last(body));
            let results = output["output"]["results"].as_array().unwrap().clone();
            assert_eq!(results.len(), 2);
            assert!(results.iter().all(|r| r["ok"] == true));
            assert!(output.to_string().contains("Found it for Find alpha"));
            assert!(output.to_string().contains("Found it for Find beta"));
            return (
                response("Both searches finished.", json!([])),
                Duration::ZERO,
            );
        }
        let system = system(body);
        assert!(system.contains("Subagents (spawn_agent)"));
        assert!(system.contains("- explore (read-only)"));
        assert!(system.contains("- general (write)"));
        assert!(tool_names(body).contains(&"spawn_agent".to_owned()));
        (
            response(
                "Delegating",
                json!([tool(
                    "spawn_agent",
                    json!({"tasks":[
                        {"agent":"explore","prompt":"Find alpha","description":"alpha"},
                        {"agent":"explore","prompt":"Find beta","description":"beta"}
                    ]})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({}));
    let job = engine
        .start(request(&project, "Look for alpha and beta"))
        .await
        .unwrap();
    let done = wait(&engine, &job.id, 20).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    // Both children were at the model at the same time.
    assert!(server.peak.load(Ordering::SeqCst) >= 2);
    for tools in seen_child_tools.lock().unwrap().iter() {
        // Read-only and no grandchildren by default.
        assert!(tools.contains(&"read_file".to_owned()));
        assert!(!tools.contains(&"write_file".to_owned()));
        assert!(!tools.contains(&"exec".to_owned()));
        assert!(!tools.contains(&"spawn_agent".to_owned()));
    }
    let started = events(&engine, &job.session_id, "subagent.started");
    let finished = events(&engine, &job.session_id, "subagent.finished");
    assert_eq!(started.len(), 2);
    assert_eq!(finished.len(), 2);
    assert!(finished
        .iter()
        .all(|e| e["payload"]["status"] == "completed" && e["payload"]["mode"] == "read-only"));
    // Child conversations are hidden from the sidebar list but viewable.
    let store = engine.store();
    let listed = store.sessions_listed("", 100, None, false).unwrap();
    assert_eq!(listed.len(), 1);
    let all = store
        .sessions_listed_with("", 100, None, false, true)
        .unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(
        all.iter()
            .filter(|s| s["subagent_parent"] == job.session_id.as_str())
            .count(),
        2
    );
    let runs = subagents::list(&store, &job.session_id).unwrap();
    assert_eq!(runs.len(), 2);
    for run in runs {
        let child = engine.job(&run.job_id).unwrap().unwrap();
        assert_eq!(child.status, "completed");
        assert!(child.summary.starts_with("Found it for"));
        assert!(store.session(&run.session_id).unwrap().is_some());
    }
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn read_only_children_and_tool_lists_cannot_change_files() {
    let server = fake(move |body| {
        match child_of(body).as_deref() {
            Some("grepper") => {
                let tools = tool_names(body);
                assert!(tools.contains(&"search_text".to_owned()));
                assert!(!tools.contains(&"read_file".to_owned()));
                if last(body)["role"] == "tool" {
                    let output = tool_output(&last(body));
                    assert_eq!(output["success"], false);
                    assert!(output["error"]
                        .as_str()
                        .unwrap()
                        .contains("not available to this agent"));
                    return (
                        response("read_file is not available", json!([])),
                        Duration::ZERO,
                    );
                }
                (
                    response("", json!([tool("read_file", json!({"path":"note.txt"}))])),
                    Duration::ZERO,
                )
            }
            Some(_) => {
                if last(body)["role"] == "tool" {
                    let output = tool_output(&last(body));
                    assert_eq!(output["success"], false);
                    assert!(output["error"].as_str().unwrap().contains("read-only"));
                    return (response("I could not write.", json!([])), Duration::ZERO);
                }
                (
                    response(
                        "",
                        json!([tool(
                            "write_file",
                            json!({"path":"note.txt","content":"changed"})
                        )]),
                    ),
                    Duration::ZERO,
                )
            }
            None => {
                if last(body)["role"] == "tool" {
                    let output = tool_output(&last(body));
                    // Asking a read-only agent for write mode is refused.
                    if output
                        .to_string()
                        .contains("is read-only; use a write agent")
                    {
                        return (response("Done.", json!([])), Duration::ZERO);
                    }
                    return (
                        response(
                            "",
                            json!([tool(
                                "spawn_agent",
                                json!({"agent":"review","prompt":"edit it","write":true})
                            )]),
                        ),
                        Duration::ZERO,
                    );
                }
                (
                    response(
                        "",
                        json!([tool(
                            "spawn_agent",
                            json!({"tasks":[
                                {"agent":"review","prompt":"Try to write note.txt"},
                                {"agent":"grepper","prompt":"Read note.txt"}
                            ]})
                        )]),
                    ),
                    Duration::ZERO,
                )
            }
        }
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({}));
    fs::write(project.join("note.txt"), "old\n").unwrap();
    fs::create_dir_all(project.join(".shadow/agents")).unwrap();
    fs::write(
        project.join(".shadow/agents/grepper.md"),
        "---\nname: grepper\ndescription: Greps only\ntools: Grep\n---\nSearch with grep only.\n",
    )
    .unwrap();
    let job = engine.start(request(&project, "Check")).await.unwrap();
    let done = wait(&engine, &job.id, 20).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert_eq!(
        fs::read_to_string(project.join("note.txt")).unwrap(),
        "old\n"
    );
    let finished = events(&engine, &job.session_id, "subagent.finished");
    assert_eq!(finished.len(), 2);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn write_child_returns_a_diff_that_the_parent_applies() {
    let server = fake(move |body| {
        if child_of(body).as_deref() == Some("general") {
            let tools = tool_names(body);
            assert!(tools.contains(&"edit_file".to_owned()));
            let turns = body["messages"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| m["role"] == "tool")
                .count();
            return match turns {
                0 => (
                    response("", json!([tool("read_file", json!({"path":"note.txt"}))])),
                    Duration::ZERO,
                ),
                1 => (
                    response(
                        "",
                        json!([tool(
                            "edit_file",
                            json!({"path":"note.txt","old_string":"old","new_string":"new"})
                        )]),
                    ),
                    Duration::ZERO,
                ),
                _ => (
                    response("Changed note.txt to say new.", json!([])),
                    Duration::ZERO,
                ),
            };
        }
        let message = last(body);
        if message["role"] == "tool" && message["name"] == "spawn_agent" {
            let output = tool_output(&message)["output"].clone();
            assert_eq!(output["mode"], "write");
            assert_eq!(output["status"], "completed");
            let diff = output["diff"].as_str().unwrap();
            assert!(diff.contains("-old"), "{diff}");
            assert!(diff.contains("+new"), "{diff}");
            let run = output["run_id"].as_str().unwrap();
            return (
                response(
                    "",
                    json!([tool("apply_agent_changes", json!({"run_id":run}))]),
                ),
                Duration::ZERO,
            );
        }
        if message["role"] == "tool" && message["name"] == "apply_agent_changes" {
            assert_eq!(tool_output(&message)["success"], true, "{message}");
            return (
                response("Applied the subagent's change.", json!([])),
                Duration::ZERO,
            );
        }
        (
            response(
                "",
                json!([tool(
                    "spawn_agent",
                    json!({"agent":"general","prompt":"Change old to new in note.txt"})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({}));
    repository(&project);
    let job = engine
        .start(request(&project, "Update the note"))
        .await
        .unwrap();
    let done = wait(&engine, &job.id, 30).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    // The child edited only its worktree; the parent applied the diff.
    assert_eq!(
        fs::read_to_string(project.join("note.txt")).unwrap(),
        "new\n"
    );
    let runs = subagents::list(&engine.store(), &job.session_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].applied && runs[0].patch);
    assert_eq!(runs[0].files.len(), 1);
    assert_eq!(runs[0].files[0].path, "note.txt");
    // The managed worktree was removed after the run.
    assert!(shadowcode_core::worktrees::list(engine.paths(), &project)
        .unwrap()
        .is_empty());
    assert_eq!(
        events(&engine, &job.session_id, "subagent.applied").len(),
        1
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelling_the_parent_cancels_running_children() {
    let server = fake(move |body| {
        if child_of(body).is_some() {
            return (response("too late", json!([])), Duration::from_secs(20));
        }
        (
            response(
                "",
                json!([tool(
                    "spawn_agent",
                    json!({"agent":"explore","prompt":"Search slowly"})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({}));
    let job = engine.start(request(&project, "Search")).await.unwrap();
    let child_job = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(event) = events(&engine, &job.session_id, "subagent.started").first() {
                return event["payload"]["job_id"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    // Let the child reach its model request.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !server
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|b| child_of(b).is_some())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let cancelled = tokio::time::timeout(Duration::from_secs(8), engine.cancel(&job.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, "cancelled");
    let child = wait(&engine, &child_job, 8).await;
    assert_eq!(child.status, "cancelled", "{}", child.summary);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn child_approvals_are_asked_in_the_parent_conversation_with_its_name() {
    let server = fake(move |body| {
        if child_of(body).is_some() {
            if last(body)["role"] == "tool" {
                assert_eq!(tool_output(&last(body))["success"], true);
                return (response("Ran the check.", json!([])), Duration::ZERO);
            }
            return (
                response("", json!([tool("exec", json!({"command":"true"}))])),
                Duration::ZERO,
            );
        }
        if last(body)["role"] == "tool" {
            return (response("Done.", json!([])), Duration::ZERO);
        }
        (
            response(
                "",
                json!([tool(
                    "spawn_agent",
                    json!({"agent":"general","prompt":"Run true"})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(
        &server.endpoint,
        json!({"permissions":{"approve_shell":true}}),
    );
    repository(&project);
    let job = engine.start(request(&project, "Check")).await.unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(record) = engine
                .approvals()
                .list(Some(job.session_id.as_str()))
                .first()
            {
                return record.clone();
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert!(
        approval.reason.starts_with("Subagent general:"),
        "{}",
        approval.reason
    );
    assert_eq!(approval.command, "true");
    assert_ne!(approval.task_id, job.task_id);
    engine
        .approvals()
        .decide(&approval.id, &job.session_id, true)
        .unwrap();
    let done = wait(&engine, &job.id, 20).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn mentions_start_the_agent_and_skills_load_on_demand() {
    let server = fake(move |body| {
        if let Some(agent) = child_of(body) {
            assert_eq!(agent, "explore");
            assert_eq!(last_user(body), "where is the config loaded?");
            return (response("In src/config.rs", json!([])), Duration::ZERO);
        }
        let system = system(body);
        // Skills are listed by name and description; bodies load lazily.
        assert!(system.contains("- deploy: Deploy the site"));
        assert!(!system.contains("hidden-skill"));
        assert!(!system.contains("SKILL-BODY-MARKER"));
        let messages = body["messages"].as_array().unwrap();
        // The mention ran the subagent before the first model turn.
        assert!(messages.iter().any(|m| m["role"] == "tool"
            && m["name"] == "spawn_agent"
            && m["content"].as_str().unwrap().contains("In src/config.rs")));
        let message = last(body);
        if message["name"] == "load_skill" {
            let output = tool_output(&message);
            assert!(output["output"]["instructions"]
                .as_str()
                .unwrap()
                .contains("SKILL-BODY-MARKER"));
            return (response("Loaded.", json!([])), Duration::ZERO);
        }
        (
            response("", json!([tool("load_skill", json!({"name":"deploy"}))])),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({}));
    fs::create_dir_all(project.join(".claude/skills/deploy")).unwrap();
    fs::write(
        project.join(".claude/skills/deploy/SKILL.md"),
        "---\nname: deploy\ndescription: Deploy the site\nallowed-tools: Bash\n---\nSKILL-BODY-MARKER steps\n",
    )
    .unwrap();
    fs::create_dir_all(project.join(".claude/skills/hidden")).unwrap();
    fs::write(
        project.join(".claude/skills/hidden/SKILL.md"),
        "---\nname: hidden-skill\ndescription: User only\ndisable-model-invocation: true\n---\nbody\n",
    )
    .unwrap();
    let job = engine
        .start(request(&project, "@explore where is the config loaded?"))
        .await
        .unwrap();
    let done = wait(&engine, &job.id, 20).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert_eq!(
        events(&engine, &job.session_id, "subagent.finished").len(),
        1
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn subagents_can_be_turned_off_and_nested_guidance_reaches_tool_results() {
    let server = fake(move |body| {
        assert!(!tool_names(body).contains(&"spawn_agent".to_owned()));
        let system = system(body);
        assert!(system.contains("Project guidance from CLAUDE.md"));
        let message = last(body);
        if message["role"] == "tool" {
            let output = tool_output(&message);
            let files = &output["output"]["project_guidance"]["files"];
            assert_eq!(files[0]["path"], "pkg/AGENTS.md");
            assert!(files[0]["content"]
                .as_str()
                .unwrap()
                .contains("Use tabs in pkg"));
            return (response("Read.", json!([])), Duration::ZERO);
        }
        (
            response("", json!([tool("read_file", json!({"path":"pkg/lib.rs"}))])),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({"subagents":{"enabled":false}}));
    fs::write(project.join("CLAUDE.md"), "Claude rules here\n").unwrap();
    fs::create_dir_all(project.join("pkg")).unwrap();
    fs::write(project.join("pkg/AGENTS.md"), "Use tabs in pkg\n").unwrap();
    fs::write(project.join("pkg/lib.rs"), "fn main() {}\n").unwrap();
    let job = engine
        .start(request(&project, "Look at pkg"))
        .await
        .unwrap();
    let done = wait(&engine, &job.id, 20).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let attached = events(&engine, &job.session_id, "context.attached");
    assert!(attached
        .iter()
        .any(|e| e["payload"]["origin"] == "nested_guidance"));
    engine.shutdown().await.unwrap();
}

#[test]
fn spawn_schema_accepts_single_and_batched_tasks() {
    let schema = subagents::spawn_schema();
    let function = &schema["function"];
    assert_eq!(function["name"], "spawn_agent");
    let properties = &function["parameters"]["properties"];
    for key in ["agent", "prompt", "description", "model", "write", "tasks"] {
        assert!(properties.get(key).is_some(), "{key}");
    }
    assert_eq!(properties["tasks"]["items"]["required"], json!(["prompt"]));
    assert_eq!(function["parameters"]["additionalProperties"], false);
    let filter = subagents::ToolFilter::new(
        &["read_file".into(), "mcp__docs".into()],
        &["mcp__docs__delete".into()],
    );
    assert!(filter.permits("read_file"));
    assert!(!filter.permits("write_file"));
    assert!(filter.permits("mcp__docs__search"));
    assert!(!filter.permits("mcp__docs__delete"));
    assert!(!filter.permits("mcp__other__search"));
}

/// The parent task's usage (and its conversation's total) includes what its
/// subagents used; each child job still reports its own.
#[tokio::test]
async fn parent_usage_includes_its_subagents() {
    let server = fake(move |body| {
        if child_of(body).is_some() {
            return (response("Found it.", json!([])), Duration::ZERO);
        }
        if last(body)["role"] == "tool" {
            return (response("Both finished.", json!([])), Duration::ZERO);
        }
        (
            response(
                "",
                json!([tool(
                    "spawn_agent",
                    json!({"tasks":[
                        {"agent":"explore","prompt":"Find alpha"},
                        {"agent":"explore","prompt":"Find beta"}
                    ]})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({}));
    let job = engine.start(request(&project, "Search")).await.unwrap();
    let done = wait(&engine, &job.id, 20).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    // Two parent requests and one request per child, 15 tokens each.
    assert_eq!(done.usage.total_tokens, 60, "{:?}", done.usage);
    assert_eq!(done.usage.turns, 4);
    assert_eq!(done.usage.cost_usd, Some(0.0));
    let store = engine.store();
    let session = store.session(&job.session_id).unwrap().unwrap();
    assert_eq!(session["usage"]["total_tokens"], 60);
    for run in subagents::list(&store, &job.session_id).unwrap() {
        let child = engine.job(&run.job_id).unwrap().unwrap();
        assert_eq!(child.usage.total_tokens, 15);
        assert_eq!(run.usage["total_tokens"], 15);
    }
    let rolled: Vec<_> = events(&engine, &job.session_id, "usage.updated")
        .into_iter()
        .filter(|e| e["payload"]["purpose"] == "subagent")
        .collect();
    assert_eq!(rolled.len(), 2);
    assert!(rolled
        .iter()
        .all(|e| e["payload"]["turn"]["total_tokens"] == 15));
    engine.shutdown().await.unwrap();
}

/// Deleting a conversation deletes its (hidden) subagent conversations, their
/// run records and saved patches. A fork that still shows a subagent's card
/// keeps that subagent conversation.
#[tokio::test]
async fn deleting_a_conversation_deletes_its_subagent_conversations() {
    let server = fake(move |body| {
        match child_of(body).as_deref() {
            Some("general") => {
                let turns = body["messages"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter(|m| m["role"] == "tool")
                    .count();
                return if turns == 0 {
                    (
                        response(
                            "",
                            json!([tool(
                                "edit_file",
                                json!({"path":"note.txt","old_string":"old","new_string":"new"})
                            )]),
                        ),
                        Duration::ZERO,
                    )
                } else {
                    (response("Edited.", json!([])), Duration::ZERO)
                };
            }
            Some(_) => return (response("Found it.", json!([])), Duration::ZERO),
            None => {}
        }
        if last(body)["role"] == "tool" {
            // The write child's diff is left unapplied: its patch stays saved.
            return (response("Done.", json!([])), Duration::ZERO);
        }
        let agent = if last_user(body).contains("edit") {
            json!({"agent":"general","prompt":"Change old to new in note.txt"})
        } else {
            json!({"agent":"explore","prompt":"Find the note"})
        };
        (
            response("", json!([tool("spawn_agent", agent)])),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(&server.endpoint, json!({}));
    repository(&project);
    let store = engine.store();
    let hidden = |store: &shadowcode_core::store::Store| {
        store
            .sessions_listed_with("", 100, None, true, true)
            .unwrap()
            .len()
    };

    // A write child: its conversation, run record and patch go with the parent.
    let job = engine
        .start(request(&project, "Please edit the note"))
        .await
        .unwrap();
    let done = wait(&engine, &job.id, 30).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let runs = subagents::list(&store, &job.session_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert!(runs[0].patch && !runs[0].applied);
    let patch = engine
        .paths()
        .data
        .join("subagents")
        .join(format!("{}.patch", runs[0].id));
    assert!(patch.exists());
    assert_eq!(hidden(&store), 2);
    assert!(engine.delete_session(&job.session_id).unwrap());
    assert!(store.session(&runs[0].session_id).unwrap().is_none());
    assert!(engine.job(&runs[0].job_id).unwrap().is_none());
    assert!(subagents::get(&store, &runs[0].id).is_err());
    assert!(subagents::list(&store, &job.session_id).unwrap().is_empty());
    assert!(!patch.exists());
    assert_eq!(hidden(&store), 0);
    assert_eq!(
        fs::read_to_string(project.join("note.txt")).unwrap(),
        "old\n"
    );

    // A fork keeps the subagent conversation its copied card opens.
    let job = engine
        .start(request(&project, "Find the note"))
        .await
        .unwrap();
    let done = wait(&engine, &job.id, 30).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let run = subagents::list(&store, &job.session_id).unwrap().remove(0);
    let fork = store.branch_session(&job.session_id, "").unwrap();
    let fork = fork["id"].as_str().unwrap().to_owned();
    assert!(engine.delete_session(&job.session_id).unwrap());
    let child = store.session(&run.session_id).unwrap();
    assert!(child.is_some(), "the fork still shows this subagent");
    assert_eq!(
        store
            .session_meta(&run.session_id, "subagent_parent")
            .unwrap()
            .as_deref(),
        Some(fork.as_str())
    );
    assert!(subagents::get(&store, &run.id).is_ok());
    assert!(engine.delete_session(&fork).unwrap());
    assert!(store.session(&run.session_id).unwrap().is_none());
    assert!(subagents::get(&store, &run.id).is_err());
    assert_eq!(hidden(&store), 0);
    engine.shutdown().await.unwrap();
}

/// A write subagent whose changes cannot be collected (a diff too large here)
/// keeps its worktree, so its work is not lost.
#[tokio::test]
async fn uncollectable_subagent_changes_keep_the_worktree() {
    let server = fake(move |body| {
        if child_of(body).as_deref() == Some("general") {
            if last(body)["role"] == "tool" {
                assert_eq!(tool_output(&last(body))["success"], true, "{}", last(body));
                return (response("Wrote big.txt.", json!([])), Duration::ZERO);
            }
            return (
                response(
                    "",
                    json!([tool(
                        "exec",
                        json!({"command":"python3 -c \"open('big.txt','w').write('a\\n'*4500000)\""})
                    )]),
                ),
                Duration::ZERO,
            );
        }
        if last(body)["role"] == "tool" {
            return (response("Done.", json!([])), Duration::ZERO);
        }
        (
            response(
                "",
                json!([tool(
                    "spawn_agent",
                    json!({"agent":"general","prompt":"Write a big file"})
                )]),
            ),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, engine, project) = setup(
        &server.endpoint,
        json!({"permissions":{"mode":"allow_edits","approve_shell":false}}),
    );
    repository(&project);
    let job = engine.start(request(&project, "Big")).await.unwrap();
    let done = wait(&engine, &job.id, 60).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let runs = subagents::list(&engine.store(), &job.session_id).unwrap();
    assert_eq!(runs.len(), 1);
    assert!(
        runs[0]
            .notes
            .iter()
            .any(|n| n.contains("Could not read") && n.contains("was kept")),
        "{:?}",
        runs[0].notes
    );
    let kept = shadowcode_core::worktrees::list(engine.paths(), &project).unwrap();
    assert_eq!(kept.len(), 1, "the worktree with the changes is kept");
    assert_eq!(
        fs::metadata(kept[0].path.join("big.txt")).unwrap().len(),
        9_000_000
    );
    engine.shutdown().await.unwrap();
}

/// A subagent still running when the app stopped is marked interrupted on
/// the next start, and its card in the parent conversation gets its
/// `subagent.finished` event instead of spinning forever.
#[tokio::test]
async fn subagent_runs_interrupted_by_a_restart_are_finished() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let engine = Engine::open(paths.clone()).unwrap();
    let store = engine.store();
    let parent = store.create_session(&project, "fixture", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let run_id = "0123456789abcdef0123456789abcdef";
    let record = json!({
        "id": run_id, "agent": "explore", "mode": "read-only", "model": "fixture",
        "parent_session": parent, "status": "running", "created_at": 1.0,
    });
    store
        .set_native_meta(&format!("subagent:{run_id}"), &record.to_string())
        .unwrap();
    store
        .add_event(
            "subagent.started",
            &json!({"run_id": run_id, "agent": "explore"}),
            Some(&parent),
            None,
        )
        .unwrap();
    drop(store);
    engine.shutdown().await.unwrap();
    drop(engine);

    let engine = Engine::open(paths).unwrap();
    let run = subagents::get(&engine.store(), run_id).unwrap();
    assert_eq!(run.status, "interrupted");
    assert!(run.error.unwrap().contains("stopped"));
    let finished = events(&engine, &parent, "subagent.finished");
    assert_eq!(finished.len(), 1);
    assert_eq!(finished[0]["payload"]["run_id"], run_id);
    assert_eq!(finished[0]["payload"]["status"], "interrupted");
    engine.shutdown().await.unwrap();
    drop(engine);
    // Recovered once only.
    let engine = Engine::open(AppPaths::isolated(&root.path().join("profile")).unwrap()).unwrap();
    assert_eq!(events(&engine, &parent, "subagent.finished").len(), 1);
    engine.shutdown().await.unwrap();
}
