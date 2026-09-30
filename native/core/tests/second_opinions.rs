//! Second opinions: a read-only review of staged changes or of one task's
//! changes, and another model's view of an answer. Local models are two
//! registered loopback models on one fake OpenAI-compatible server (the
//! reply depends on the requested model name); the cloud reviewer is the
//! fake Codex app-server. No real vendor CLI or network is used.
mod support;
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};
use vendor_support::{cli_agents, FakeCodex};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Review Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn response(text: &str, calls: Value) -> Value {
    let reason = if calls.as_array().is_some_and(|v| !v.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
}
fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

/// A messy review: reasoning, prose, a fenced block with other key names
/// and a trailing comma.
const MESSY_REVIEW: &str = "<think>The diff changes value.</think>Here is what I found.\n```json\n{\"overview\": \"One real problem.\", \"issues\": [{\"path\": \"b/lib.txt:1\", \"priority\": \"Critical\", \"description\": \"The value doubled without a reason.\", \"recommendation\": \"Keep value = 1.\"},]}\n```";

/// `writer` (model alpha) edits lib.txt; `reviewer` (model beta) first
/// tries to write a file, then answers: findings when asked for a review,
/// prose when asked for a second opinion.
fn reply(body: &Value) -> Value {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    if body["tools"].as_array().is_none_or(Vec::is_empty) {
        return response("Title", json!([]));
    }
    let after_tools = messages.iter().any(|m| m["role"] == "tool");
    let prompt = messages
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].to_string())
        .collect::<String>();
    match body["model"].as_str() {
        Some("beta") if !after_tools => response(
            "Let me fix it myself.",
            json!([tool(
                "r1",
                "write_file",
                json!({"path":"lib.txt","content":"reviewer was here\n","expected_hash":"missing"})
            )]),
        ),
        Some("beta") if prompt.contains("\\\"findings\\\"") => response(MESSY_REVIEW, json!([])),
        Some("beta") => response(
            "I agree with the answer, but lib.txt needs a test.",
            json!([]),
        ),
        _ if after_tools => response("Done.", json!([])),
        _ => response(
            "Editing",
            json!([tool(
                "w1",
                "edit_file",
                json!({"path":"lib.txt","old_string":"value = 1","new_string":"value = 2"})
            )]),
        ),
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    server: support::Server,
    project: PathBuf,
    service: Service,
}

async fn fixture(extra: Value) -> Fixture {
    let server = support::server(|_, body| (reply(body), Duration::ZERO)).await;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    fs::write(project.join("lib.txt"), "value = 1\n").unwrap();
    fs::write(project.join(".env"), "TOKEN=committed\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "Base"]);
    let project = project.canonicalize().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let mut config = json!({
        "trusted_workspaces": [project.clone()],
        "permissions": {"mode": "allow_edits", "approve_shell": false},
        "model": {"provider":"local","endpoint":server.endpoint,"name":"alpha","context_limit":16384},
        "agent": {"max_steps": 8},
        "cli_agents": {"enabled": false}
    });
    for (key, value) in extra.as_object().unwrap() {
        config[key] = value.clone();
    }
    Config::patch(&paths, config).unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    for (id, name) in [("writer", "alpha"), ("reviewer", "beta")] {
        call(
            &service,
            "POST",
            "/api/models/register",
            json!({"id":id,"name":name,"provider":"local","endpoint":server.endpoint,"context_limit":16384}),
        )
        .await;
    }
    Fixture {
        _root: root,
        server,
        project,
        service,
    }
}

async fn try_call(
    service: &Service,
    method: &str,
    path: &str,
    body: Value,
) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}
async fn call(service: &Service, method: &str, path: &str, body: Value) -> Value {
    try_call(service, method, path, body)
        .await
        .unwrap_or_else(|e| panic!("{method} {path}: {e:#}"))
}

/// Wait for a second opinion to finish.
async fn finished(service: &Service, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let record = call(
            service,
            "GET",
            &format!("/api/second-opinions/{id}"),
            Value::Null,
        )
        .await;
        if !matches!(record["status"].as_str(), Some("queued" | "running")) {
            return record;
        }
        assert!(started.elapsed() < Duration::from_secs(120), "{record:#}");
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
}

async fn job_done(service: &Service, id: &str) -> Value {
    // Generous: the suite runs beside other heavy builds.
    match tokio::time::timeout(Duration::from_secs(120), service.engine.wait(id)).await {
        Ok(job) => json!(job.unwrap()),
        Err(_) => {
            let job = service.engine.job(id).unwrap().unwrap();
            let events = service
                .engine
                .store()
                .events_after(&job.session_id, 0, None, 10_000)
                .unwrap();
            panic!("job did not finish: {:#}\n{events:#?}", json!(job));
        }
    }
}

fn requests_for(server: &support::Server, model: &str) -> Vec<Value> {
    server
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r["model"] == model && r["tools"].as_array().is_some_and(|t| !t.is_empty()))
        .cloned()
        .collect()
}

fn tool_names(request: &Value) -> Vec<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|t| t["function"]["name"].as_str().map(str::to_owned))
        .collect()
}

#[tokio::test]
async fn staged_review_is_read_only_and_reads_messy_findings_onto_hunks() {
    let f = fixture(json!({})).await;
    let service = &f.service;
    let workspace = f.project.to_string_lossy().to_string();

    // Nothing staged: refused before anything is written.
    let empty = try_call(
        service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"staged","workspace":workspace,"model":"reviewer"}),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{empty:#}").contains("Nothing is staged yet"),
        "{empty:#}"
    );

    fs::write(f.project.join("lib.txt"), "value = 2\n").unwrap();
    fs::write(f.project.join(".env"), "TOKEN=very-secret-value\n").unwrap();
    // A credential inside an ordinary file is hidden, not sent.
    fs::write(
        f.project.join("config.py"),
        format!("TOKEN = \"{}\"\n", fake_token()),
    )
    .unwrap();
    git(&f.project, &["add", "lib.txt", ".env", "config.py"]);
    let current = call(
        service,
        "GET",
        &format!("/api/second-opinions/current?workspace={workspace}&source=staged"),
        Value::Null,
    )
    .await;
    assert_eq!(current["files"], json!(["config.py", "lib.txt"]));
    assert_eq!(current["omitted"], json!([".env"]));

    let record = call(
        service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"staged","workspace":workspace,"model":"reviewer","question":"Is the new value right?"}),
    )
    .await;
    assert!(
        matches!(record["status"].as_str(), Some("queued" | "running")),
        "{record:#}"
    );
    assert_eq!(record["reviewer"]["model"], "reviewer");
    assert_eq!(record["reviewer"]["local"], true);
    assert_eq!(record["diff_hash"], current["hash"]);
    let id = record["id"].as_str().unwrap().to_owned();

    let done = finished(service, &id).await;
    assert_eq!(done["status"], "completed", "{done:#}");
    // The reviewer tried to write a file: it had no write tool and the call
    // was refused, so the project is untouched.
    assert_eq!(
        fs::read_to_string(f.project.join("lib.txt")).unwrap(),
        "value = 2\n"
    );
    assert_eq!(done["reviewer_changed"], json!([]));
    let asked = requests_for(&f.server, "beta");
    assert!(!asked.is_empty());
    for request in &asked {
        let names = tool_names(request);
        assert!(names.contains(&"read_file".to_string()), "{names:?}");
        for write in [
            "write_file",
            "edit_file",
            "apply_patch",
            "delete_file",
            "exec",
        ] {
            assert!(
                !names.contains(&write.to_string()),
                "{write} offered: {names:?}"
            );
        }
    }
    let prompt = asked[0]["messages"].to_string();
    assert!(prompt.contains("Is the new value right?"));
    assert!(prompt.contains("+value = 2"));
    assert!(
        !prompt.contains("very-secret-value"),
        "a secret file reached the reviewer"
    );
    assert!(prompt.contains("Secret files left out: .env"));
    // The review request carries the change with the credential hidden.
    // (The native loop's own repository map is outside the request.)
    let request = asked[0]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == "user")
        .map(|m| m["content"].to_string())
        .collect::<String>();
    assert!(request.contains("config.py"));
    assert!(!request.contains(&fake_token()));
    assert!(done["redacted"].as_u64().unwrap() >= 1);
    let refused = asked
        .iter()
        .flat_map(|r| r["messages"].as_array().unwrap().clone())
        .find(|m| m["role"] == "tool")
        .expect("the write attempt's result");
    assert!(refused.to_string().contains("read-only"), "{refused}");

    // Findings read from the messy reply, placed on the reviewed hunk.
    assert_eq!(done["summary"], "One real problem.");
    assert_eq!(done["format_note"], "");
    let finding = &done["findings"][0];
    assert_eq!(finding["file"], "lib.txt");
    assert_eq!(finding["line"], 1);
    assert_eq!(finding["severity"], "high");
    assert_eq!(finding["suggested_fix"], "Keep value = 1.");
    assert_eq!(finding["status"], "open");
    assert!(finding["hunk"].as_str().unwrap().starts_with("@@ -1"));
    assert_eq!(done["diff"][1]["path"], "lib.txt");
    // Usage is recorded like any job's and shown with the model.
    assert!(done["usage"]["total_tokens"].as_u64().unwrap() > 0);
    assert!(!done["model_name"].as_str().unwrap().is_empty());

    // The reviewer's job ran in review mode in a hidden conversation.
    let job = job_done(service, done["job_id"].as_str().unwrap()).await;
    assert_eq!(job["mode"], "review");
    let sessions = call(service, "GET", "/api/sessions", Value::Null).await;
    assert!(!sessions
        .to_string()
        .contains(done["review_session"].as_str().unwrap()));
    let feed = call(service, "GET", "/api/feed", Value::Null).await;
    let summary = feed["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|j| j["id"] == done["job_id"])
        .unwrap();
    assert_eq!(summary["second_opinion"], id.as_str());

    // The choice is remembered for the project; dismissing is kept.
    let options = call(
        service,
        "GET",
        &format!("/api/second-opinions/options?workspace={workspace}"),
        Value::Null,
    )
    .await;
    assert_eq!(options["prefs"]["model"], "reviewer");
    assert_eq!(options["offline"], false);
    let dismissed = call(
        service,
        "POST",
        &format!("/api/second-opinions/{id}/findings/f1"),
        json!({"status":"dismissed"}),
    )
    .await;
    assert_eq!(dismissed["findings"][0]["status"], "dismissed");
    let listed = call(
        service,
        "GET",
        &format!("/api/second-opinions?workspace={workspace}&source=staged"),
        Value::Null,
    )
    .await;
    assert_eq!(listed["second_opinions"][0]["id"], id.as_str());
    assert_eq!(
        listed["second_opinions"][0]["findings"][0]["status"],
        "dismissed"
    );
    // Lists leave the reviewed diff out unless asked for it.
    assert_eq!(listed["second_opinions"][0]["diff"], json!([]));
    let with_diff = call(
        service,
        "GET",
        &format!("/api/second-opinions?workspace={workspace}&source=staged&diff=1&limit=1"),
        Value::Null,
    )
    .await;
    assert_eq!(
        with_diff["second_opinions"][0]["diff"][1]["path"],
        "lib.txt"
    );

    // "Review before every commit" is a per-project preference.
    let prefs = call(
        service,
        "POST",
        "/api/second-opinions/prefs",
        json!({"workspace":workspace,"before_commit":true}),
    )
    .await;
    assert_eq!(prefs, json!({"model":"reviewer","before_commit":true}));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_task_gets_a_second_opinion_a_review_and_a_queued_fix() {
    let f = fixture(json!({})).await;
    let service = &f.service;
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"task":"Double the value","model":"writer"}),
    )
    .await;
    let job = job_done(service, job["id"].as_str().unwrap()).await;
    assert_eq!(job["status"], "completed", "{job:#}");
    let (task, sid) = (
        job["task_id"].as_str().unwrap().to_owned(),
        job["session_id"].as_str().unwrap().to_owned(),
    );

    let options = call(
        service,
        "GET",
        &format!("/api/second-opinions/options?task_id={task}"),
        Value::Null,
    )
    .await;
    assert_eq!(options["writer"]["model"], "writer");
    assert_eq!(options["local_only"], true);

    // Ask: the request, the answer and the task's changes, as context.
    let asked = call(
        service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"ask","source":"task","task_id":task,"model":"reviewer","question":"Is this safe?"}),
    )
    .await;
    assert_eq!(asked["session_id"], sid.as_str());
    assert_eq!(asked["writer"]["model"], "writer");
    let asked = finished(service, asked["id"].as_str().unwrap()).await;
    assert_eq!(asked["status"], "completed", "{asked:#}");
    assert_eq!(
        asked["summary"],
        "I agree with the answer, but lib.txt needs a test."
    );
    assert_eq!(asked["findings"], json!([]));
    let prompt = requests_for(&f.server, "beta")[0]["messages"].to_string();
    assert!(prompt.contains("Double the value"), "{prompt}");
    assert!(prompt.contains("Done."));
    assert!(prompt.contains("+value = 2"));
    assert!(prompt.contains("Is this safe?"));

    // Review of the same task's changes: structured findings.
    let reviewed = call(
        service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"task","task_id":task,"model":"reviewer"}),
    )
    .await;
    let reviewed = finished(service, reviewed["id"].as_str().unwrap()).await;
    assert_eq!(reviewed["findings"][0]["file"], "lib.txt");
    let listed = call(
        service,
        "GET",
        &format!("/api/second-opinions?task_id={task}"),
        Value::Null,
    )
    .await;
    assert_eq!(listed["second_opinions"].as_array().unwrap().len(), 2);
    let current = call(
        service,
        "GET",
        &format!("/api/second-opinions/current?source=task&task_id={task}"),
        Value::Null,
    )
    .await;
    assert_eq!(current["hash"], reviewed["diff_hash"]);

    // "Ask the agent to fix this" queues a follow-up in the conversation.
    let id = reviewed["id"].as_str().unwrap();
    let fixed = call(
        service,
        "POST",
        &format!("/api/second-opinions/{id}/findings/f1/fix"),
        json!({}),
    )
    .await;
    assert_eq!(fixed["job"]["session_id"], sid.as_str());
    let text = fixed["job"]["task"].as_str().unwrap();
    assert!(
        text.contains("found a serious problem in lib.txt at line 1"),
        "{text}"
    );
    assert!(text.contains("Suggested fix: Keep value = 1."));
    assert_eq!(fixed["second_opinion"]["findings"][0]["status"], "fixing");
    assert_eq!(
        fixed["second_opinion"]["findings"][0]["fix_job_id"],
        fixed["job"]["id"]
    );
    let again = try_call(
        service,
        "POST",
        &format!("/api/second-opinions/{id}/findings/f1/fix"),
        json!({}),
    )
    .await
    .unwrap_err();
    assert!(format!("{again:#}").contains("already queued"));
    job_done(service, fixed["job"]["id"].as_str().unwrap()).await;

    // Deleting the conversation deletes its reviewers' conversations.
    let hidden = reviewed["review_session"].as_str().unwrap().to_owned();
    assert!(service.engine.store().session(&hidden).unwrap().is_some());
    call(
        service,
        "DELETE",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await;
    assert!(service.engine.store().session(&hidden).unwrap().is_none());
    service.engine.shutdown().await.unwrap();
}

/// A finished turn that ran on this computer and changed `parser.rs`.
fn local_turn(service: &Service, project: &Path, sid: &str) -> String {
    let store = service.engine.store();
    let task_id = format!("task{}", shadowcode_core::id());
    store
        .create_job(&json!({
            "id": shadowcode_core::id(),
            "workspace": project,
            "session_id": sid,
            "task_id": task_id,
            "task": "fix the parser",
            "status": "completed",
            "mode": "code",
            "summary": "Fixed the parser.",
            "routing": {"provider":"llamacpp","model_id":"local:gguf:abc","model_name":"qwen3-14b","inference":"local","route":"local_llamacpp"},
        }))
        .unwrap();
    store
        .add_event(
            "files.changed",
            &json!({"paths":["parser.rs"]}),
            Some(sid),
            Some(&task_id),
        )
        .unwrap();
    task_id
}

#[tokio::test]
async fn local_work_reaches_a_cloud_reviewer_only_with_consent() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    fs::write(project.join("parser.rs"), "fn parse() {}\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "Base"]);
    fs::write(project.join("parser.rs"), "fn parse() { todo!() }\n").unwrap();
    let project = project.canonicalize().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt","turn":"ok"}));
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"fixture","context_limit":16384},
            "trusted_workspaces":[project],
            "cli_agents": cli_agents(&fake),
        }),
    )
    .unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let session = call(&service, "POST", "/api/sessions", json!({})).await;
    let sid = session["id"].as_str().unwrap().to_owned();
    let task = local_turn(&service, &project, &sid);
    let store = service.engine.store();
    let (jobs, sessions) = (
        store.jobs(100).unwrap().len(),
        store.sessions("", 100).unwrap().len(),
    );

    let refused = call(
        &service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"task","task_id":task,"model":"cli:codex"}),
    )
    .await;
    assert_eq!(refused["status"], 409, "{refused:#}");
    assert_eq!(refused["needs_consent"], true);
    assert_eq!(refused["handoff"]["purpose"], "second_opinion");
    assert!(refused["handoff"]["to"]
        .as_str()
        .unwrap()
        .starts_with("Codex"));
    assert!(refused["handoff"]["excerpt_chars"].as_u64().unwrap() > 0);
    // Nothing was written or sent.
    assert_eq!(store.jobs(100).unwrap().len(), jobs);
    assert_eq!(store.sessions("", 100).unwrap().len(), sessions);
    assert!(fake.marker("prompts.log").is_none());
    let listed = call(&service, "GET", "/api/second-opinions", Value::Null).await;
    assert_eq!(listed["second_opinions"], json!([]));

    let record = call(
        &service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"task","task_id":task,"model":"cli:codex","consent":true}),
    )
    .await;
    assert_eq!(record["consented"], true);
    assert_eq!(record["reviewer"]["local"], false);
    assert_eq!(record["writer"]["local"], true);
    let done = finished(&service, record["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done:#}");
    // Codex ran in its read-only sandbox and got the review request.
    assert!(fake.marker("sandbox.log").unwrap().contains("read-only"));
    let prompts = fake.marker("prompts.log").unwrap();
    assert!(prompts.contains("second opinion as a code reviewer"));
    assert!(prompts.contains("todo!()"));
    // A reply that is not in the requested form stays visible as text.
    assert_eq!(done["summary"], "fake answer");
    assert!(done["format_note"]
        .as_str()
        .unwrap()
        .contains("not list findings"));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn offline_mode_accepts_only_reviewers_on_this_computer() {
    let f = fixture(json!({"network": {"mode": "offline"}})).await;
    let service = &f.service;
    fs::write(f.project.join("lib.txt"), "value = 3\n").unwrap();
    git(&f.project, &["add", "lib.txt"]);
    let store = service.engine.store();
    let sessions = store.sessions("", 100).unwrap().len();
    let refused = try_call(
        service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"staged","model":"cli:codex"}),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("Offline mode"),
        "{refused:#}"
    );
    assert_eq!(store.sessions("", 100).unwrap().len(), sessions);
    let options = call(service, "GET", "/api/second-opinions/options", Value::Null).await;
    assert_eq!(options["offline"], true);
    // A model on this computer still reviews.
    let record = call(
        service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"staged","model":"reviewer"}),
    )
    .await;
    let done = finished(service, record["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done:#}");
    assert_eq!(done["findings"].as_array().unwrap().len(), 1);
    // A running review can be stopped; a finished one stays as it is.
    let stopped = call(
        service,
        "POST",
        &format!(
            "/api/second-opinions/{}/cancel",
            done["id"].as_str().unwrap()
        ),
        json!({}),
    )
    .await;
    assert_eq!(stopped["status"], "completed");
    service.engine.shutdown().await.unwrap();
}

/// Live: one short read-only review of a tiny staged change by a real model.
/// With `SHADOWCODE_LIVE_REVIEW_ENDPOINT` (an OpenAI-compatible server on
/// this computer, such as Ollama's `http://127.0.0.1:11434/v1`) and
/// `SHADOWCODE_LIVE_REVIEW_MODEL`, the model is registered and the review runs
/// offline; otherwise `SHADOWCODE_LIVE_REVIEWER` names a picker id (default
/// `cli:claude:haiku`, the installed Claude Code with its own sign-in). Run by
/// hand: `cargo test -p shadowcode-core --test second_opinions live_review -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "live: needs a real model (a local server or a signed-in vendor CLI)"]
async fn live_review_of_a_staged_change_by_a_real_model() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    fs::write(
        project.join("calc.py"),
        "def add(a, b):\n    \"\"\"Return the sum of a and b.\"\"\"\n    return a + b\n",
    )
    .unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "Base"]);
    let changed = "def add(a, b):\n    \"\"\"Return the sum of a and b.\"\"\"\n    return a - b\n";
    fs::write(project.join("calc.py"), changed).unwrap();
    git(&project, &["add", "calc.py"]);
    let project = project.canonicalize().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let endpoint = std::env::var("SHADOWCODE_LIVE_REVIEW_ENDPOINT").ok();
    Config::patch(
        &paths,
        json!({
            "trusted_workspaces": [project.clone()],
            "model": {"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"unused","context_limit":16384},
            "agent": {"max_steps": 6},
            "network": {"mode": if endpoint.is_some() { "offline" } else { "online" }},
        }),
    )
    .unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let reviewer = match &endpoint {
        Some(endpoint) => {
            let name = std::env::var("SHADOWCODE_LIVE_REVIEW_MODEL")
                .expect("SHADOWCODE_LIVE_REVIEW_MODEL");
            call(
                &service,
                "POST",
                "/api/models/register",
                json!({"id":"live-reviewer","name":name,"provider":"ollama","endpoint":endpoint,"context_limit":16384}),
            )
            .await;
            "live-reviewer".to_owned()
        }
        None => {
            std::env::var("SHADOWCODE_LIVE_REVIEWER").unwrap_or_else(|_| "cli:claude:haiku".into())
        }
    };
    let started = Instant::now();
    let record = call(
        &service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"staged","model":reviewer}),
    )
    .await;
    let id = record["id"].as_str().unwrap().to_owned();
    let done = loop {
        let now = call(
            &service,
            "GET",
            &format!("/api/second-opinions/{id}"),
            Value::Null,
        )
        .await;
        if !matches!(now["status"].as_str(), Some("queued" | "running")) {
            break now;
        }
        assert!(started.elapsed() < Duration::from_secs(600), "{now:#}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    eprintln!(
        "{} by {} in {:?}\nsummary: {}\nnote: {}\nfindings: {:#}\nusage: {}",
        done["status"],
        done["model_name"],
        started.elapsed(),
        done["summary"],
        done["format_note"],
        done["findings"],
        done["usage"]
    );
    assert_eq!(done["status"], "completed", "{done:#}");
    // Read-only: the project is as the user left it.
    assert_eq!(
        fs::read_to_string(project.join("calc.py")).unwrap(),
        changed
    );
    assert_eq!(done["reviewer_changed"], json!([]));
    // The subtraction is the bug a reviewer has to see.
    assert!(
        done["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["file"] == "calc.py"),
        "{done:#}"
    );
    assert!(done["usage"]["total_tokens"].as_u64().unwrap_or(0) > 0);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_second_opinion_that_hits_a_plan_limit_stays_on_its_model() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    fs::write(project.join("a.txt"), "one\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "Base"]);
    fs::write(project.join("a.txt"), "two\n").unwrap();
    git(&project, &["add", "a.txt"]);
    let project = project.canonicalize().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt","turn":"limit"}));
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"fixture","context_limit":16384},
            "trusted_workspaces":[project],
            "cli_agents": cli_agents(&fake),
        }),
    )
    .unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let record = call(
        &service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"staged","model":"cli:codex"}),
    )
    .await;
    let done = finished(&service, record["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "limit_reached", "{done:#}");
    let hidden = done["review_session"].as_str().unwrap();
    // The limit's follow-up is decided just after the job ends.
    let started = Instant::now();
    let fallback = loop {
        let found = service
            .engine
            .store()
            .events_after(hidden, 0, None, 10_000)
            .unwrap()
            .into_iter()
            .find(|e| e["type"] == "limit.fallback");
        if let Some(found) = found {
            break found;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "no limit.fallback"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(fallback["payload"]["ok"], false, "{fallback:#}");
    assert!(fallback["payload"]["reason"]
        .as_str()
        .unwrap()
        .contains("keeps the model you chose"));
    // No other job ran in the reviewer's conversation.
    assert_eq!(
        service
            .engine
            .store()
            .session_jobs(hidden, 10)
            .unwrap()
            .len(),
        1
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_paid_reviewer_at_a_spending_limit_stops_instead_of_waiting() {
    let f = fixture(json!({"spending":{"task_usd":null,"daily_usd":0.5}})).await;
    let service = &f.service;
    let store = service.engine.store();
    // A reviewer billed per token, on the fake server.
    store
        .upsert_model(&json!({
            "id":"paid","name":"beta","provider":"openrouter","endpoint":f.server.endpoint,
            "context_limit":16384,"metadata":{"api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY"},
        }))
        .unwrap();
    // Paid models already cost more than today's limit.
    let today = shadowcode_core::spending::day_of(shadowcode_core::now());
    store
        .set_native_meta(
            shadowcode_core::spending::DAY_KEY,
            &json!({"day": today, "usd": 0.6}).to_string(),
        )
        .unwrap();
    fs::write(f.project.join("lib.txt"), "value = 3\n").unwrap();
    git(&f.project, &["add", "lib.txt"]);
    let record = call(
        service,
        "POST",
        "/api/second-opinions",
        json!({"kind":"review","source":"staged","workspace":f.project,"model":"paid"}),
    )
    .await;
    let done = finished(service, record["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "failed", "{done:#}");
    assert!(
        done["error"]
            .as_str()
            .unwrap()
            .starts_with("Today's spending limit for paid models ($0.50) is reached."),
        "{done:#}"
    );
    assert!(
        requests_for(&f.server, "beta").is_empty(),
        "nothing was sent"
    );
    // No card waits unseen in the hidden conversation, so nothing holds up
    // the project's queue.
    let hidden = done["review_session"].as_str().unwrap();
    assert!(store
        .events_after(hidden, 0, None, 10_000)
        .unwrap()
        .iter()
        .all(|e| e["type"] != "spend.limit_reached"));
    assert!(service.engine.spending_waiting().unwrap().is_empty());
    service.engine.shutdown().await.unwrap();
}

/// A GitHub-token-shaped placeholder, built at runtime so the repository's
/// secret scanner never sees a token-shaped literal.
fn fake_token() -> String {
    format!("ghp_{}", "0123456789abcdefghijklmnopqrstuvwxyzAB")
}
