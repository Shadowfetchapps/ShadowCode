//! Compare: one task, several models, each in its own managed worktree.
//! Two registered loopback models share one fake OpenAI-compatible server;
//! the reply depends on the requested model name, so the lanes make
//! different edits. No vendor CLI or network is used.
mod support;
use anyhow::Result;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
    worktrees,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Compare Test",
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

/// alpha edits lib.txt and writes answer.txt; beta writes answer.txt and
/// beta.txt. Each answers once its tool results are in.
fn reply(body: &Value) -> Value {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    if body["tools"].as_array().is_none_or(Vec::is_empty) {
        return response("Compare lane", json!([]));
    }
    if messages.iter().any(|m| m["role"] == "tool") {
        return response("Done.", json!([]));
    }
    match body["model"].as_str() {
        Some("alpha") => response(
            "Editing",
            json!([
                tool(
                    "a1",
                    "edit_file",
                    json!({"path":"lib.txt","old_string":"value = 1","new_string":"value = 2 (alpha)"})
                ),
                tool(
                    "a2",
                    "write_file",
                    json!({"path":"answer.txt","content":"alpha\n","expected_hash":"missing"})
                ),
            ]),
        ),
        _ => response(
            "Editing",
            json!([
                tool(
                    "b1",
                    "write_file",
                    json!({"path":"answer.txt","content":"beta\n","expected_hash":"missing"})
                ),
                tool(
                    "b2",
                    "write_file",
                    json!({"path":"beta.txt","content":"only beta\nsecond\n","expected_hash":"missing"})
                ),
            ]),
        ),
    }
}

struct Fixture {
    _root: tempfile::TempDir,
    server: support::Server,
    project: PathBuf,
    paths: AppPaths,
    service: Service,
}

async fn fixture() -> Fixture {
    fixture_with(Duration::ZERO).await
}
/// `delay` holds every model reply, so lanes stay running.
async fn fixture_with(delay: Duration) -> Fixture {
    fixture_with_checks(delay, false).await
}
async fn fixture_with_checks(delay: Duration, checks: bool) -> Fixture {
    let server = support::server(move |_, body| {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let after_edits = messages.iter().any(|m| m["role"] == "tool");
        let checked = messages.iter().any(|m| m["name"] == "exec");
        let response = if checks && after_edits && !checked {
            response(
                "Checking",
                json!([tool("check", "exec", json!({"command":"printf check"}))]),
            )
        } else {
            reply(body)
        };
        (response, delay)
    })
    .await;
    let root = match std::env::var_os("SHADOWCODE_COMPARE_CRASH_ROOT") {
        Some(parent) => tempfile::tempdir_in(parent).unwrap(),
        None => tempfile::tempdir().unwrap(),
    };
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
    fs::write(project.join("tracked.txt"), "committed\n").unwrap();
    fs::write(project.join("lib.txt"), "value = 1\n").unwrap();
    fs::write(project.join(".gitignore"), "ignored.log\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "Base"]);
    let project = project.canonicalize().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "trusted_workspaces": [project.clone()],
            "permissions": {"mode": "allow_edits", "approve_shell": false},
            "model": {"provider":"local","endpoint":server.endpoint,"name":"alpha","context_limit":16384},
            "agent": {"max_steps": 8},
            "verification": {"commands":["printf check"]},
            // Never probe the vendor CLIs installed on the test machine.
            "cli_agents": {"enabled": false}
        }),
    )
    .unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    for name in ["alpha", "beta"] {
        call(
            &service,
            "POST",
            "/api/models/register",
            json!({"id":format!("lane-{name}"),"name":name,"provider":"local","endpoint":server.endpoint,"context_limit":16384}),
        )
        .await
        .unwrap();
    }
    Fixture {
        _root: root,
        server,
        project,
        paths,
        service,
    }
}

async fn call(service: &Service, method: &str, path: &str, body: Value) -> Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}

async fn start(f: &Fixture) -> Value {
    call(
        &f.service,
        "POST",
        "/api/compare",
        json!({"workspace":f.project,"task":"Set the answer","models":["lane-alpha","lane-beta"]}),
    )
    .await
    .unwrap()
}

async fn finished(f: &Fixture, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let record = call(
            &f.service,
            "GET",
            &format!("/api/compare/{id}"),
            Value::Null,
        )
        .await
        .unwrap();
        if record["state"] == "done" {
            return record;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "{record:#}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn lane<'a>(record: &'a Value, model: &str) -> &'a Value {
    record["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|lane| lane["model"] == model)
        .unwrap()
}
fn files(lane: &Value) -> Vec<(String, String, u64, u64)> {
    lane["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["path"].as_str().unwrap().to_owned(),
                f["status"].as_str().unwrap().to_owned(),
                f["additions"].as_u64().unwrap(),
                f["deletions"].as_u64().unwrap(),
            )
        })
        .collect()
}
fn managed_branches(project: &Path) -> String {
    git(project, &["branch", "--list", "shadowcode/*"])
}

#[tokio::test]
async fn compare_preserves_flagged_index_and_captures_hidden_working_edits() {
    let f = fixture().await;
    let project = &f.project;
    git(
        project,
        &["update-index", "--assume-unchanged", "tracked.txt"],
    );
    git(project, &["update-index", "--skip-worktree", "lib.txt"]);
    fs::write(project.join("tracked.txt"), "hidden user edit\n").unwrap();
    fs::write(project.join("lib.txt"), "value = 1\nuser annotation\n").unwrap();
    fs::write(project.join("intent.txt"), "intent-to-add user file\n").unwrap();
    git(project, &["add", "--intent-to-add", "intent.txt"]);
    let index = project.join(".git/index");
    let index_bytes = fs::read(&index).unwrap();
    let head = git(project, &["rev-parse", "HEAD"]);

    let started = start(&f).await;
    let id = started["id"].as_str().unwrap();
    let done = finished(&f, id).await;
    for model in ["lane-alpha", "lane-beta"] {
        let path = Path::new(lane(&done, model)["worktree"].as_str().unwrap());
        assert_eq!(
            fs::read_to_string(path.join("tracked.txt")).unwrap(),
            "hidden user edit\n"
        );
        assert_eq!(
            fs::read_to_string(path.join("intent.txt")).unwrap(),
            "intent-to-add user file\n"
        );
        assert!(fs::read_to_string(path.join("lib.txt"))
            .unwrap()
            .contains("user annotation\n"));
    }
    assert_eq!(fs::read(&index).unwrap(), index_bytes);
    let kept = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap();
    assert_eq!(kept["state"], "applied");
    assert_eq!(
        fs::read_to_string(project.join("lib.txt")).unwrap(),
        "value = 2 (alpha)\nuser annotation\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "hidden user edit\n"
    );
    assert_eq!(
        fs::read(&index).unwrap(),
        index_bytes,
        "Keep must preserve original index flags, intent and staged bytes"
    );
    assert_eq!(git(project, &["rev-parse", "HEAD"]), head);
    f.service.engine.shutdown().await.unwrap();
    drop(f.service);
    let reopened = Service::open(f.paths.clone(), Some(project.clone())).unwrap();
    let recovered = call(&reopened, "GET", &format!("/api/compare/{id}"), Value::Null)
        .await
        .unwrap();
    assert_eq!(recovered["state"], "applied");
    assert_eq!(fs::read(&index).unwrap(), index_bytes);
    reopened.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn lanes_start_from_uncommitted_work_and_keep_applies_one_result() {
    let f = fixture().await;
    let project = &f.project;
    // Staged and unstaged tracked edits, an untracked file and an ignored one.
    fs::write(project.join("tracked.txt"), "staged change\n").unwrap();
    git(project, &["add", "tracked.txt"]);
    fs::write(project.join("tracked.txt"), "uncommitted change\n").unwrap();
    fs::write(project.join("notes.txt"), "untracked\n").unwrap();
    fs::write(project.join("ignored.log"), "ignored\n").unwrap();
    let head = git(project, &["rev-parse", "HEAD"]);
    let status = git(project, &["status", "--porcelain=v1"]);
    let staged = git(project, &["show", ":tracked.txt"]);

    let record = start(&f).await;
    let id = record["id"].as_str().unwrap().to_owned();
    assert_eq!(record["state"], "running");
    assert_eq!(record["base"]["included_uncommitted"], true);
    assert_ne!(record["base"]["commit"], head);
    assert_eq!(record["lanes"].as_array().unwrap().len(), 2);
    assert_eq!(record["mode"], "code");
    for lane in record["lanes"].as_array().unwrap() {
        let worktree = Path::new(lane["worktree"].as_str().unwrap());
        assert_eq!(
            fs::read_to_string(worktree.join("tracked.txt")).unwrap(),
            "uncommitted change\n"
        );
        assert_eq!(
            fs::read_to_string(worktree.join("notes.txt")).unwrap(),
            "untracked\n"
        );
        assert!(!worktree.join("ignored.log").exists());
        assert_eq!(lane["base_commit"], record["base"]["commit"]);
        assert_eq!(
            git(worktree, &["log", "-1", "--format=%s"]),
            "ShadowCode compare base"
        );
        assert_eq!(git(worktree, &["rev-parse", "HEAD^"]), head);
        assert!(lane["branch"].as_str().unwrap().starts_with("shadowcode/"));
        assert!(!lane["session_id"].as_str().unwrap().is_empty());
    }
    // The source checkout is untouched.
    assert_eq!(git(project, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(project, &["status", "--porcelain=v1"]), status);
    assert_eq!(git(project, &["show", ":tracked.txt"]), staged);

    let record = finished(&f, &id).await;
    let alpha = lane(&record, "lane-alpha");
    let beta = lane(&record, "lane-beta");
    let requested: Vec<Value> = f
        .server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|body| body["model"].clone())
        .collect();
    assert!(requested.contains(&json!("alpha")) && requested.contains(&json!("beta")));
    assert_eq!(alpha["status"], "completed", "{alpha:#}");
    assert_eq!(beta["status"], "completed", "{beta:#}");
    assert_eq!(alpha["name"], "alpha");
    assert_eq!(
        files(alpha),
        vec![
            ("answer.txt".into(), "added".into(), 1, 0),
            ("lib.txt".into(), "modified".into(), 1, 1),
        ]
    );
    assert_eq!(
        files(beta),
        vec![
            ("answer.txt".into(), "added".into(), 1, 0),
            ("beta.txt".into(), "added".into(), 2, 0),
        ]
    );
    assert!(alpha["duration_s"].as_f64().unwrap() >= 0.0);
    assert!(alpha["usage"]["total_tokens"].as_u64().unwrap() > 0);
    assert!(alpha["checks"]["commands"].is_array());
    assert!(record.get("counted").is_none());
    let session = call(
        &f.service,
        "GET",
        &format!("/api/sessions/{}", alpha["session_id"].as_str().unwrap()),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(session["compare_id"], id.as_str());
    assert_eq!(session["compare_lane"], "lane-alpha");
    assert_eq!(session["execution_target"], "lane-alpha");

    // Lane conversations stay out of the default session list.
    let listed = call(&f.service, "GET", "/api/sessions", Value::Null)
        .await
        .unwrap();
    assert!(!listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| !s["compare_id"].is_null()));
    let all = call(
        &f.service,
        "GET",
        "/api/sessions?include_compare=true",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(
        all["sessions"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|s| s["compare_id"] == id.as_str())
            .count(),
        2
    );

    // Opening a lane's conversation does not make its worktree a project.
    let lane_session = all["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["compare_id"] == id.as_str())
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    call(
        &f.service,
        "POST",
        &format!("/api/sessions/{lane_session}/activate"),
        json!({}),
    )
    .await
    .unwrap();
    let projects = call(&f.service, "GET", "/api/projects", Value::Null)
        .await
        .unwrap();
    assert!(!projects["projects"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["path"].as_str().unwrap().contains("managed-worktrees")));

    // Keeping refuses a model that is not a lane.
    assert!(call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-gamma"})
    )
    .await
    .is_err());
    let kept = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap();
    assert_eq!(kept["state"], "applied", "{kept:#}");
    assert_eq!(kept["winner"], "lane-alpha");
    assert_eq!(kept["applied_files"], json!(["answer.txt", "lib.txt"]));
    assert_eq!(kept["recovery"]["phase"], "applied");
    let recovery_ref = format!("refs/shadowcode/recovery/{id}");
    assert_eq!(
        git(project, &["rev-parse", &recovery_ref]),
        kept["recovery"]["before_commit"].as_str().unwrap()
    );
    assert_eq!(
        git(project, &["show", &format!("{recovery_ref}:tracked.txt")]),
        "uncommitted change"
    );

    assert!(kept["notes"].as_array().unwrap().is_empty(), "{kept:#}");
    // alpha's result is in the working tree, not staged and not committed.
    assert_eq!(
        fs::read_to_string(project.join("lib.txt")).unwrap(),
        "value = 2 (alpha)\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("answer.txt")).unwrap(),
        "alpha\n"
    );
    assert!(!project.join("beta.txt").exists());
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "uncommitted change\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("notes.txt")).unwrap(),
        "untracked\n"
    );
    assert_eq!(git(project, &["rev-parse", "HEAD"]), head);
    assert_eq!(git(project, &["show", ":tracked.txt"]), staged);
    assert_eq!(git(project, &["show", ":lib.txt"]), "value = 1");
    // Lane worktrees and their managed branches are gone.
    for lane in kept["lanes"].as_array().unwrap() {
        assert_eq!(lane["removed"], true);
        assert!(!Path::new(lane["worktree"].as_str().unwrap()).exists());
    }
    assert_eq!(managed_branches(project), "");
    assert_eq!(git(project, &["worktree", "list"]).lines().count(), 1);
    assert!(worktrees::list(&f.paths, project).unwrap().is_empty());
    let cfg = Config::load(&f.paths, None).unwrap();
    assert_eq!(cfg.trusted_workspaces.len(), 1);

    // A second keep is refused; the record is persisted.
    assert!(call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-beta"})
    )
    .await
    .is_err());
    let again = call(
        &f.service,
        "GET",
        &format!("/api/compare/{id}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(again["state"], "applied");
    assert_eq!(
        lane(&again, "lane-beta")["changed_files"],
        beta["changed_files"]
    );
    let listed = call(
        &f.service,
        "GET",
        &format!("/api/compares?workspace={}", project.display()),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(listed["compares"][0]["id"], id.as_str());

    let board = call(
        &f.service,
        "GET",
        &format!("/api/compare/scoreboard?workspace={}", project.display()),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(
        board["rows"],
        json!([
            {"model":"lane-alpha","name":"alpha","wins":1,"runs":1},
            {"model":"lane-beta","name":"beta","wins":0,"runs":1},
        ])
    );
    f.service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn keep_refuses_conflicts_and_discard_removes_every_lane() {
    let f = fixture().await;
    let project = &f.project;
    let record = start(&f).await;
    let id = record["id"].as_str().unwrap().to_owned();
    assert_eq!(record["base"]["included_uncommitted"], false);
    assert_eq!(
        record["base"]["commit"],
        git(project, &["rev-parse", "HEAD"])
    );
    let record = finished(&f, &id).await;
    // The user edits the same line after the comparison started.
    fs::write(project.join("lib.txt"), "value = 99\n").unwrap();
    let error = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("lib.txt"), "{error}");
    assert!(error.contains("changed since"), "{error}");
    assert_eq!(
        fs::read_to_string(project.join("lib.txt")).unwrap(),
        "value = 99\n"
    );
    assert!(!project.join("answer.txt").exists());
    let after = call(
        &f.service,
        "GET",
        &format!("/api/compare/{id}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(after["state"], "done");
    assert!(after["winner"].is_null());
    for lane in record["lanes"].as_array().unwrap() {
        assert!(Path::new(lane["worktree"].as_str().unwrap()).is_dir());
    }
    assert_eq!(managed_branches(project).lines().count(), 2);

    let discarded = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(discarded["state"], "discarded");
    for lane in discarded["lanes"].as_array().unwrap() {
        assert_eq!(lane["removed"], true);
        assert!(!Path::new(lane["worktree"].as_str().unwrap()).exists());
    }
    assert_eq!(managed_branches(project), "");
    assert_eq!(git(project, &["worktree", "list"]).lines().count(), 1);
    assert_eq!(git(project, &["status", "--porcelain=v1"]), "M lib.txt");
    let board = call(&f.service, "GET", "/api/compare/scoreboard", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        board["rows"],
        json!([
            {"model":"lane-alpha","name":"alpha","wins":0,"runs":1},
            {"model":"lane-beta","name":"beta","wins":0,"runs":1},
        ])
    );
    f.service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_lane_deleted_outside_shadowcode_does_not_block_keeping_another() {
    let f = fixture().await;
    let project = &f.project;
    let record = start(&f).await;
    let id = record["id"].as_str().unwrap().to_owned();
    let record = finished(&f, &id).await;
    let beta = Path::new(lane(&record, "lane-beta")["worktree"].as_str().unwrap()).to_owned();
    let beta_id = lane(&record, "lane-beta")["worktree_id"].as_str().unwrap();
    let admin = project.join(".git/worktrees").join(beta_id);
    let original_index = fs::read(admin.join("index")).unwrap();
    fs::remove_dir_all(&beta).unwrap();
    let error = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-beta"}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("deleted outside ShadowCode"), "{error}");
    let kept = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap();
    assert_eq!(kept["state"], "applied");
    let notes = kept["notes"].to_string();
    assert!(notes.contains("deleted outside ShadowCode"), "{notes}");
    assert_eq!(
        fs::read_to_string(project.join("answer.txt")).unwrap(),
        "alpha\n"
    );
    assert_eq!(kept["cleanup_pending"], true);
    assert!(
        notes.contains("absent without cleanup ownership evidence"),
        "{notes}"
    );
    assert_eq!(fs::read(admin.join("index")).unwrap(), original_index);
    assert!(managed_branches(project).contains(beta_id));
    assert_eq!(git(project, &["worktree", "list"]).lines().count(), 2);
    f.service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn invalid_lineups_create_nothing() {
    let f = fixture().await;
    let project = &f.project;
    let attempt = |models: Value| {
        let service = f.service.clone();
        let project = project.clone();
        async move {
            call(
                &service,
                "POST",
                "/api/compare",
                json!({"workspace":project,"task":"Do it","models":models}),
            )
            .await
            .unwrap_err()
            .to_string()
        }
    };
    assert!(attempt(json!(["lane-alpha"])).await.contains("2 or 3"));
    assert!(attempt(json!(["lane-alpha", "lane-beta", "a", "b"]))
        .await
        .contains("2 or 3"));
    assert!(attempt(json!(["lane-alpha", "lane-alpha"]))
        .await
        .contains("different models"));
    let local = attempt(json!(["local:gguf:one", "local:gguf:two"])).await;
    assert!(!local.contains("one local model"), "{local}");
    assert!(attempt(json!(["lane-alpha", "lane-unknown"]))
        .await
        .contains("lane-unknown"));
    let error = call(
        &f.service,
        "POST",
        "/api/compare",
        json!({"workspace":project,"task":"Do it","models":["lane-alpha","lane-beta"],"mode":"review"}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("mode"), "{error}");

    // Offline mode refuses cloud lanes before anything starts.
    call(
        &f.service,
        "POST",
        "/api/models/register",
        json!({"id":"lane-cloud","name":"cloud","provider":"openai_compatible","endpoint":"https://models.example.invalid/v1","context_limit":16384}),
    )
    .await
    .unwrap();
    Config::patch(&f.paths, json!({"network":{"mode":"offline"}})).unwrap();
    let cloud = attempt(json!(["lane-alpha", "lane-cloud"])).await;
    assert!(cloud.contains("Offline"), "{cloud}");
    let router = attempt(json!(["lane-alpha", "api:openrouter:qwen/qwen3-coder"])).await;
    assert!(router.contains("Offline"), "{router}");
    Config::patch(&f.paths, json!({"network":{"mode":"online"}})).unwrap();

    // Untrusted projects and folders that are not a repository root.
    let nested = project.join("nested");
    fs::create_dir(&nested).unwrap();
    Config::patch(
        &f.paths,
        json!({"trusted_workspaces":[project.clone(), nested.clone()]}),
    )
    .unwrap();
    let error = call(
        &f.service,
        "POST",
        "/api/compare",
        json!({"workspace":nested,"task":"Do it","models":["lane-alpha","lane-beta"]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("repository root"), "{error}");
    Config::patch(&f.paths, json!({"trusted_workspaces":[]})).unwrap();
    let error = call(
        &f.service,
        "POST",
        "/api/compare",
        json!({"workspace":project,"task":"Do it","models":["lane-alpha","lane-beta"]}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("Trust"), "{error}");

    assert!(worktrees::list(&f.paths, project).unwrap().is_empty());
    assert_eq!(managed_branches(project), "");
    let listed = call(&f.service, "GET", "/api/compares", Value::Null)
        .await
        .unwrap();
    assert_eq!(listed["compares"], json!([]));
    f.service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancel_keeps_lanes_and_discard_stops_running_lanes() {
    let f = fixture_with(Duration::from_secs(2)).await;
    let project = &f.project;
    let record = start(&f).await;
    let id = record["id"].as_str().unwrap().to_owned();
    // Keeping a lane that is still working is refused.
    let error = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(error.contains("still working"), "{error}");
    call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/cancel"),
        Value::Null,
    )
    .await
    .unwrap();
    let stopped = finished(&f, &id).await;
    for lane in stopped["lanes"].as_array().unwrap() {
        assert_eq!(lane["status"], "cancelled", "{lane:#}");
        assert!(Path::new(lane["worktree"].as_str().unwrap()).is_dir());
    }
    assert_eq!(managed_branches(project).lines().count(), 2);
    call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();

    // Discarding a running comparison stops its lanes and does not count it.
    let running = start(&f).await;
    let second = running["id"].as_str().unwrap().to_owned();
    let discarded = call(
        &f.service,
        "POST",
        &format!("/api/compare/{second}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(discarded["state"], "discarded");
    for lane in discarded["lanes"].as_array().unwrap() {
        assert_eq!(lane["status"], "cancelled", "{lane:#}");
        assert_eq!(lane["removed"], true);
    }
    assert_eq!(managed_branches(project), "");
    assert_eq!(git(project, &["status", "--porcelain=v1"]), "");
    let board = call(&f.service, "GET", "/api/compare/scoreboard", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        board["rows"],
        json!([
            {"model":"lane-alpha","name":"alpha","wins":0,"runs":1},
            {"model":"lane-beta","name":"beta","wins":0,"runs":1},
        ])
    );
    let listed = call(&f.service, "GET", "/api/compares", Value::Null)
        .await
        .unwrap();
    assert_eq!(listed["compares"][0]["id"], second.as_str());
    assert_eq!(listed["compares"][1]["id"], id.as_str());
    f.service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn keep_revalidates_checks_and_requires_explicit_review_of_stale_evidence() {
    for edited in [false, true] {
        let f = fixture_with_checks(Duration::ZERO, true).await;
        let started = start(&f).await;
        let id = started["id"].as_str().unwrap();
        let done = finished(&f, id).await;
        let selected = lane(&done, "lane-alpha");
        assert_eq!(selected["checks"]["passed"], 1, "{selected}");
        let worktree = PathBuf::from(selected["worktree"].as_str().unwrap());
        let busy = f.service.engine.reserve_workspace(&worktree).unwrap();
        let refused = call(
            &f.service,
            "POST",
            &format!("/api/compare/{id}/keep"),
            json!({"model":"lane-alpha"}),
        )
        .await
        .unwrap_err();
        assert!(refused.to_string().contains("already using"));
        assert!(!f.project.join("answer.txt").exists());
        drop(busy);
        if edited {
            fs::write(worktree.join("answer.txt"), "edited after verification\n").unwrap();
            let observed = call(
                &f.service,
                "GET",
                &format!("/api/compare/{id}"),
                Value::Null,
            )
            .await
            .unwrap();
            assert_eq!(lane(&observed, "lane-alpha")["checks"]["passed"], 0);
            assert_eq!(lane(&observed, "lane-alpha")["checks"]["incomplete"], 1);
            let error = call(
                &f.service,
                "POST",
                &format!("/api/compare/{id}/keep"),
                json!({"model":"lane-alpha"}),
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains("current verification"),
                "{error}"
            );
            assert!(!f.project.join("answer.txt").exists());
            assert!(worktree.is_dir());
        }
        let applied = call(
            &f.service,
            "POST",
            &format!("/api/compare/{id}/keep"),
            json!({"model":"lane-alpha","accept_unverified":edited}),
        )
        .await
        .unwrap();
        assert_eq!(applied["state"], "applied");
        assert_eq!(
            lane(&applied, "lane-alpha")["checks"]["passed"],
            if edited { 0 } else { 1 }
        );
        if edited {
            assert!(applied["notes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|n| n.as_str().unwrap().contains("explicit review")));
            assert_eq!(
                fs::read_to_string(f.project.join("answer.txt")).unwrap(),
                "edited after verification\n"
            );
        }
        f.service.engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn applied_outcome_survives_pending_cleanup_and_restart_without_rescoring() {
    let f = fixture().await;
    let started = start(&f).await;
    let id = started["id"].as_str().unwrap();
    let ready = finished(&f, id).await;
    let beta = PathBuf::from(lane(&ready, "lane-beta")["worktree"].as_str().unwrap());
    // A manual owner is as authoritative as a running job at disposal time.
    let owner = f.service.engine.reserve_workspace(&beta).unwrap();
    let kept = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap();
    assert_eq!(kept["state"], "applied");
    assert_eq!(kept["cleanup_pending"], true);
    assert_eq!(lane(&kept, "lane-beta")["removed"], false);
    assert!(beta.is_dir());
    assert_eq!(
        fs::read_to_string(f.project.join("answer.txt")).unwrap(),
        "alpha\n"
    );
    drop(owner);
    f.service.engine.shutdown().await.unwrap();
    drop(f.service);
    let reopened = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let restored = call(&reopened, "GET", &format!("/api/compare/{id}"), Value::Null)
        .await
        .unwrap();
    assert_eq!(restored["winner"], "lane-alpha");
    assert_eq!(restored["cleanup_pending"], true);
    // User edits after application must survive a cleanup retry.
    fs::write(f.project.join("answer.txt"), "user edited after keep\n").unwrap();
    for _ in 0..2 {
        let cleaned = call(
            &reopened,
            "POST",
            &format!("/api/compare/{id}/discard"),
            Value::Null,
        )
        .await
        .unwrap();
        assert_eq!(cleaned["state"], "applied");
        assert_eq!(cleaned["cleanup_pending"], false);
        assert_eq!(cleaned["winner"], "lane-alpha");
    }
    assert!(!beta.exists());
    assert_eq!(
        fs::read_to_string(f.project.join("answer.txt")).unwrap(),
        "user edited after keep\n"
    );
    let board = call(&reopened, "GET", "/api/compare/scoreboard", Value::Null)
        .await
        .unwrap();
    assert_eq!(board["rows"][0]["wins"], 1);
    reopened.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn uncertain_keep_survives_restart_and_blocks_mutation_and_cleanup() {
    for partially_changed in [false, true] {
        let f = fixture().await;
        let started = start(&f).await;
        let id = started["id"].as_str().unwrap();
        let ready = finished(&f, id).await;
        // Seed the durable state left by interruption after intent persistence.
        // This tests restart handling, not an actual OS crash injection.
        let store = f.service.engine.store();
        let key = shadowcode_core::store::keys::compare_record(id);
        store.meta_transaction(|meta| {
            let mut record: Value = meta.json(&key)?.unwrap();
            record["state"] = json!("needs_review");
            record["recovery"] = json!({"phase":"applying","model":"lane-alpha","paths":["lib.txt","answer.txt"]});
            meta.set_json(&key, &record)
        }).unwrap();
        if partially_changed {
            fs::write(
                f.project.join("lib.txt"),
                "uncertain or user-edited content\n",
            )
            .unwrap();
        }
        let before = fs::read(f.project.join("lib.txt")).unwrap();
        f.service.engine.shutdown().await.unwrap();
        drop(store);
        drop(f.service);
        let reopened = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
        let restored = call(&reopened, "GET", &format!("/api/compare/{id}"), Value::Null)
            .await
            .unwrap();
        assert_eq!(restored["state"], "needs_review");
        for action in ["keep", "discard"] {
            assert!(call(
                &reopened,
                "POST",
                &format!("/api/compare/{id}/{action}"),
                json!({"model":"lane-alpha"})
            )
            .await
            .is_err());
        }
        assert_eq!(fs::read(f.project.join("lib.txt")).unwrap(), before);
        for lane in ready["lanes"].as_array().unwrap() {
            assert!(Path::new(lane["worktree"].as_str().unwrap()).is_dir());
        }
        reopened.engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn recovery_reconciles_exact_images_and_preserves_ambiguous_edits() {
    for outcome in ["before", "after", "ambiguous", "index"] {
        let f = fixture().await;
        let started = start(&f).await;
        let id = started["id"].as_str().unwrap();
        let ready = finished(&f, id).await;
        let alpha = PathBuf::from(lane(&ready, "lane-alpha")["worktree"].as_str().unwrap());
        git(&alpha, &["add", "--all"]);
        git(&alpha, &["commit", "-qm", "Result"]);
        let result = git(&alpha, &["rev-parse", "HEAD"]);
        let before = git(&f.project, &["rev-parse", "HEAD"]);
        let store = f.service.engine.store();
        let key = shadowcode_core::store::keys::compare_record(id);
        store
            .meta_transaction(|meta| {
                let mut record: Value = meta.json(&key)?.unwrap();
                record["state"] = json!("needs_review");
                record["recovery"] = json!({
                    "phase":"applying", "model":"lane-alpha", "result_commit": result,
                    "before_commit":before, "expected_head":before,
                    "expected_ref":git(&f.project, &["rev-parse","--symbolic-full-name","HEAD"]),
                    "index_tree":git(&f.project, &["write-tree"]),
                    "after_tree":git(&alpha, &["rev-parse","HEAD^{tree}"]),
                    "paths":["answer.txt","lib.txt"]
                });
                meta.set_json(&key, &record)
            })
            .unwrap();
        if outcome == "after" {
            fs::copy(alpha.join("lib.txt"), f.project.join("lib.txt")).unwrap();
            fs::copy(alpha.join("answer.txt"), f.project.join("answer.txt")).unwrap();
        } else if matches!(outcome, "ambiguous" | "index") {
            fs::write(f.project.join("lib.txt"), "user edit\n").unwrap();
            if outcome == "index" {
                git(&f.project, &["add", "lib.txt"]);
            }
        }
        let content = fs::read(f.project.join("lib.txt")).unwrap();
        f.service.engine.shutdown().await.unwrap();
        drop(store);
        drop(f.service);
        let reopened = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
        let assessed = call(
            &reopened,
            "POST",
            &format!("/api/compare/{id}/recover"),
            Value::Null,
        )
        .await;
        match outcome {
            "before" => assert_eq!(assessed.unwrap()["state"], "done"),
            "after" => {
                assert_eq!(assessed.unwrap()["state"], "applied");
                // Repeated assessment does not score again or apply a patch.
                call(
                    &reopened,
                    "POST",
                    &format!("/api/compare/{id}/recover"),
                    Value::Null,
                )
                .await
                .unwrap();
                let board = call(&reopened, "GET", "/api/compare/scoreboard", Value::Null)
                    .await
                    .unwrap();
                assert_eq!(board["rows"][0]["wins"], 1);
            }
            _ => assert!(
                assessed.is_err(),
                "{outcome} unexpectedly reconciled: {assessed:?}"
            ),
        }
        assert_eq!(fs::read(f.project.join("lib.txt")).unwrap(), content);
        assert!(alpha.is_dir());
        reopened.engine.shutdown().await.unwrap();
    }
}

// These hooks live only in the integration-test executable. Production code
// has no environment-controlled crash points.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn compare_crash_child() {
    let Some(parent) = std::env::var_os("SHADOWCODE_COMPARE_CRASH_ROOT") else {
        return;
    };
    let f = fixture().await;
    let record = start(&f).await;
    let id = record["id"].as_str().unwrap();
    let ready = finished(&f, id).await;
    let alpha = PathBuf::from(lane(&ready, "lane-alpha")["worktree"].as_str().unwrap());
    fs::write(alpha.join("binary.dat"), [0, 1, 255, 0, 42]).unwrap();
    fs::rename(alpha.join("tracked.txt"), alpha.join("renamed.txt")).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(alpha.join("answer.txt"), fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(
        Path::new(&parent).join("fixture.json"),
        json!({"root":f._root.path(),"id":id}).to_string(),
    )
    .unwrap();
    call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap();
    panic!("Crash interception did not stop Keep");
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn real_process_crash_before_and_after_apply_recovers_without_reapplication() {
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;
    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
    for phase in ["before", "after", "cleanup"] {
        let root = tempfile::tempdir().unwrap();
        let bin = root.path().join("bin");
        fs::create_dir(&bin).unwrap();
        let marker = root.path().join("paused");
        let real_git = Command::new("sh")
            .args(["-c", "command -v git"])
            .output()
            .unwrap();
        assert!(real_git.status.success());
        let real_git = String::from_utf8(real_git.stdout).unwrap();
        // Match only source working-tree application, never --check or
        // --cached projection. Pause with a bounded timeout as a backstop.
        let script = format!(
            r#"#!/bin/sh
apply=no
skip=no
worktree=no
remove=no
for arg do
  case "$arg" in apply) apply=yes;; --check|--cached) skip=yes;; worktree) worktree=yes;; remove) remove=yes;; esac
done
if {{ [ {phase} != cleanup ] && [ "$apply" = yes ] && [ "$skip" = no ]; }} || {{ [ {phase} = cleanup ] && [ "$worktree" = yes ] && [ "$remove" = yes ]; }}; then
  if [ {phase} = after ]; then {git} "$@" || exit $?; fi
  echo $$ > {marker}
  sleep 30
  exit 97
fi
exec {git} "$@"
"#,
            phase = quote(phase),
            git = quote(real_git.trim()),
            marker = quote(marker.to_str().unwrap())
        );
        fs::write(bin.join("git"), script).unwrap();
        fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
        let log = fs::File::create(root.path().join("child.log")).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "compare_crash_child", "--nocapture"])
            .env("SHADOWCODE_COMPARE_CRASH_ROOT", root.path())
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            )
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(20);
        while !marker.exists() && Instant::now() < deadline && child.try_wait().unwrap().is_none() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        // The recorded PID belongs to our wrapper, which process::run starts
        // in its own process group. Kill that owned group and our test child.
        if marker.exists() {
            // The parent is a distinct process/profile. It cannot acquire the
            // repository lock while the child is inside a mutation boundary.
            use fs2::FileExt;
            let fixture: Value =
                serde_json::from_slice(&fs::read(root.path().join("fixture.json")).unwrap())
                    .unwrap();
            let lock_path = Path::new(fixture["root"].as_str().unwrap())
                .join("project/.git/shadowcode-compare.lock");
            let other = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(lock_path)
                .unwrap();
            assert!(
                other.try_lock_exclusive().is_err(),
                "competing process acquired mutation ownership"
            );
        }
        // Stop the test process first: it must not unwind and delete its
        // temporary profile when the intercepted Git child is terminated.
        let _ = child.kill();
        child.wait().unwrap();
        if let Ok(pid) = fs::read_to_string(&marker) {
            let pid: i32 = pid.trim().parse().unwrap();
            assert!(pid > 1);
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
            }
        }
        assert!(
            marker.exists(),
            "child did not reach {phase}: {}",
            fs::read_to_string(root.path().join("child.log")).unwrap()
        );
        let fixture: Value =
            serde_json::from_slice(&fs::read(root.path().join("fixture.json")).unwrap()).unwrap();
        let location = PathBuf::from(fixture["root"].as_str().unwrap());
        let project = location.join("project");
        let id = fixture["id"].as_str().unwrap();
        let service = Service::open(
            AppPaths::isolated(&location.join("profile")).unwrap(),
            Some(project.clone()),
        )
        .unwrap();
        let interrupted = call(&service, "GET", &format!("/api/compare/{id}"), Value::Null)
            .await
            .unwrap();
        assert_eq!(
            interrupted["state"],
            if phase == "cleanup" {
                "applied"
            } else {
                "needs_review"
            }
        );
        let source = fs::read(project.join("lib.txt")).unwrap();
        let recovered = call(
            &service,
            "POST",
            &format!("/api/compare/{id}/recover"),
            Value::Null,
        )
        .await
        .unwrap();
        assert_eq!(
            recovered["state"],
            if phase == "before" { "done" } else { "applied" }
        );
        assert_eq!(fs::read(project.join("lib.txt")).unwrap(), source);
        assert_eq!(project.join("answer.txt").exists(), phase != "before");
        assert_eq!(project.join("tracked.txt").exists(), phase == "before");
        if phase != "before" {
            assert_eq!(
                fs::read(project.join("binary.dat")).unwrap(),
                [0, 1, 255, 0, 42]
            );
            assert_eq!(
                fs::read_to_string(project.join("renamed.txt")).unwrap(),
                "committed\n"
            );
            assert_ne!(
                fs::metadata(project.join("answer.txt"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o111,
                0
            );
        }

        for lane in recovered["lanes"].as_array().unwrap() {
            let original = Path::new(lane["worktree"].as_str().unwrap());
            if phase == "cleanup" && !original.exists() {
                let journal: Value = serde_json::from_slice(
                    &fs::read(
                        location
                            .join("profile/data/managed-worktrees/records/cleanup")
                            .join(format!("{}.json", lane["worktree_id"].as_str().unwrap())),
                    )
                    .unwrap(),
                )
                .unwrap();
                assert_eq!(journal["path"], lane["worktree"]);
                assert_eq!(journal["relocated"], true);
                assert_eq!(journal["removed"], false);
                let retained = Path::new(journal["quarantine"].as_str().unwrap());
                assert!(retained.is_dir());
                use std::os::unix::fs::MetadataExt;
                let identity = fs::metadata(retained).unwrap();
                assert_eq!(
                    identity.dev(),
                    journal["lane"]["root"]["dev"].as_u64().unwrap()
                );
                assert_eq!(
                    identity.ino(),
                    journal["lane"]["root"]["ino"].as_u64().unwrap()
                );
                assert_eq!(
                    fs::read(retained.join("binary.dat")).unwrap(),
                    [0, 1, 255, 0, 42]
                );
            } else {
                assert!(original.exists());
            }
        }
        if phase == "cleanup" {
            let cleaned = call(
                &service,
                "POST",
                &format!("/api/compare/{id}/discard"),
                Value::Null,
            )
            .await
            .unwrap();
            assert_eq!(cleaned["state"], "applied");
            assert_eq!(cleaned["winner"], "lane-alpha");
            assert_eq!(cleaned["cleanup_pending"], false);
            assert_eq!(fs::read(project.join("lib.txt")).unwrap(), source);
            assert_eq!(
                fs::read(project.join("binary.dat")).unwrap(),
                [0, 1, 255, 0, 42]
            );
            let score = call(&service, "GET", "/api/compare/scoreboard", Value::Null)
                .await
                .unwrap();
            assert_eq!(score["rows"][0]["wins"], 1);
            assert_eq!(score["rows"][0]["runs"], 1);
            assert_eq!(score["rows"][1]["runs"], 1);
        }
        service.engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn repository_ownership_refuses_keep_before_source_changes() {
    use fs2::FileExt;
    let f = fixture().await;
    let record = start(&f).await;
    let id = record["id"].as_str().unwrap();
    finished(&f, id).await;
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(f.project.join(".git/shadowcode-compare.lock"))
        .unwrap();
    file.try_lock_exclusive().unwrap();
    let error = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("owns this repository"),
        "{error:#}"
    );
    assert_eq!(
        fs::read_to_string(f.project.join("lib.txt")).unwrap(),
        "value = 1\n"
    );
    assert!(!f.project.join("answer.txt").exists());
    drop(file);
    let kept = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap();
    assert_eq!(kept["state"], "applied");
    f.service.engine.shutdown().await.unwrap();
}

// Deliberate overlap at real service/Git/process boundaries. Linux-only
// because the owned-process assertions and TERM gate use /proc and signals.
#[cfg(target_os = "linux")]
mod concurrency {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Stdio;

    fn quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }

    struct ReleaseOnDrop(PathBuf);
    impl Drop for ReleaseOnDrop {
        fn drop(&mut self) {
            let _ = fs::write(&self.0, b"release\n");
        }
    }

    async fn wait_file(path: &Path, limit: Duration) {
        tokio::time::timeout(limit, async {
            while !path.is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("No synchronization marker: {}", path.display()));
    }

    fn dirty_source(project: &Path) -> (String, String, String) {
        fs::write(project.join("tracked.txt"), "staged before comparison\n").unwrap();
        git(project, &["add", "tracked.txt"]);
        fs::write(project.join("tracked.txt"), "unstaged before comparison\n").unwrap();
        fs::write(project.join("notes.txt"), "untracked before comparison\n").unwrap();
        fs::write(project.join("ignored.log"), "ignored before comparison\n").unwrap();
        (
            git(project, &["rev-parse", "HEAD"]),
            git(project, &["write-tree"]),
            git(project, &["status", "--porcelain=v1"]),
        )
    }

    fn assert_user_state(project: &Path, head: &str, index: &str) {
        assert_eq!(git(project, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(project, &["write-tree"]), index);
        assert_eq!(
            fs::read_to_string(project.join("tracked.txt")).unwrap(),
            "unstaged before comparison\n"
        );
        assert_eq!(
            git(project, &["show", ":tracked.txt"]),
            "staged before comparison"
        );
        assert_eq!(
            fs::read_to_string(project.join("notes.txt")).unwrap(),
            "untracked before comparison\n"
        );
        assert_eq!(
            fs::read_to_string(project.join("ignored.log")).unwrap(),
            "ignored before comparison\n"
        );
    }

    // Invoked only in an isolated test subprocess. The wrapper's PATH and
    // control files never affect the parent test runner or installed program.
    #[tokio::test]
    async fn compare_race_child() {
        let Some(control) = std::env::var_os("SHADOWCODE_COMPARE_RACE_ROOT") else {
            return;
        };
        let control = PathBuf::from(control);
        let scenario = std::env::var("SHADOWCODE_COMPARE_RACE_CASE").unwrap();
        let discard_first = scenario == "discard_keep";
        let f = fixture().await;
        let (head, index, initial_status) = dirty_source(&f.project);
        let record = start(&f).await;
        let id = record["id"].as_str().unwrap().to_owned();
        assert_eq!(record["base"]["included_uncommitted"], true);
        let ready = finished(&f, &id).await;
        for row in ready["lanes"].as_array().unwrap() {
            assert_eq!(row["status"], "completed");
            let copy = Path::new(row["worktree"].as_str().unwrap());
            assert_eq!(
                fs::read_to_string(copy.join("tracked.txt")).unwrap(),
                "unstaged before comparison\n"
            );
            assert!(!copy.join("ignored.log").exists());
        }
        assert_eq!(
            git(&f.project, &["status", "--porcelain=v1"]),
            initial_status
        );
        assert_user_state(&f.project, &head, &index);

        fs::write(control.join("source"), f.project.to_str().unwrap()).unwrap();
        fs::write(
            control.join("gate"),
            if discard_first { "remove" } else { "apply" },
        )
        .unwrap();
        let release = ReleaseOnDrop(control.join("release"));
        let first_service = f.service.clone();
        let first_path = format!(
            "/api/compare/{id}/{}",
            if discard_first { "discard" } else { "keep" }
        );
        let first = tokio::spawn(async move {
            call(
                &first_service,
                "POST",
                &first_path,
                json!({"model":"lane-alpha"}),
            )
            .await
        });
        wait_file(&control.join("paused"), Duration::from_secs(15)).await;
        assert!(
            !first.is_finished(),
            "first operation did not remain at its real Git boundary"
        );

        let (first, second) = {
            let action = if scenario == "keep_discard" {
                "discard"
            } else {
                "keep"
            };
            let model = if scenario == "different_keep" {
                "lane-beta"
            } else {
                "lane-alpha"
            };
            let second_path = format!("/api/compare/{id}/{action}");
            let second = call(&f.service, "POST", &second_path, json!({"model":model}));
            tokio::pin!(second);
            // One explicit poll enters dispatch while the first mutation is
            // blocked. This cannot pass merely because two tasks were spawned
            // and happened to execute sequentially.
            assert!(futures_util::poll!(&mut second).is_pending());
            assert!(!first.is_finished());
            assert_user_state(&f.project, &head, &index);
            assert_eq!(
                fs::read_to_string(f.project.join("lib.txt")).unwrap(),
                "value = 1\n"
            );
            assert!(!f.project.join("answer.txt").exists());

            fs::write(&release.0, b"release\n").unwrap();
            tokio::time::timeout(Duration::from_secs(20), async {
                tokio::join!(first, &mut second)
            })
            .await
            .unwrap()
        };
        let first = first.unwrap().unwrap();
        if scenario == "keep_discard" {
            // Discard after durable Keep is an idempotent cleanup retry.
            let second = second.unwrap();
            assert_eq!(second["state"], "applied");
            assert_eq!(second["winner"], "lane-alpha");
        } else {
            let error = second.unwrap_err().to_string();
            assert!(
                error.contains(if discard_first {
                    "already discarded"
                } else {
                    "already applied"
                }),
                "{error}"
            );
        }
        assert_eq!(
            first["state"],
            if discard_first {
                "discarded"
            } else {
                "applied"
            }
        );
        assert_eq!(
            first["winner"],
            if discard_first {
                Value::Null
            } else {
                json!("lane-alpha")
            }
        );
        assert_eq!(first["cleanup_pending"], false);
        assert!(first["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["removed"] == true));
        assert_user_state(&f.project, &head, &index);
        assert_eq!(
            fs::read_to_string(f.project.join("lib.txt")).unwrap(),
            if discard_first {
                "value = 1\n"
            } else {
                "value = 2 (alpha)\n"
            }
        );
        if discard_first {
            assert!(!f.project.join("answer.txt").exists());
            assert_eq!(
                git(&f.project, &["status", "--porcelain=v1"]),
                initial_status
            );
        } else {
            assert_eq!(
                fs::read_to_string(f.project.join("answer.txt")).unwrap(),
                "alpha\n"
            );
        }
        assert!(!f.project.join("beta.txt").exists());
        assert_eq!(managed_branches(&f.project), "");
        assert!(worktrees::list(&f.paths, &f.project).unwrap().is_empty());
        assert_eq!(git(&f.project, &["worktree", "list"]).lines().count(), 1);
        let applies = fs::read_to_string(control.join("apply-count"))
            .unwrap_or_default()
            .lines()
            .count();
        assert_eq!(applies, usize::from(!discard_first));

        f.service.engine.shutdown().await.unwrap();
        drop(f.service);
        let reopened = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
        for _ in 0..2 {
            let saved = call(
                &reopened,
                "POST",
                &format!("/api/compare/{id}/discard"),
                Value::Null,
            )
            .await
            .unwrap();
            assert_eq!(saved["state"], first["state"]);
            assert_eq!(saved["winner"], first["winner"]);
            let board = call(&reopened, "GET", "/api/compare/scoreboard", Value::Null)
                .await
                .unwrap();
            assert_eq!(
                board["rows"],
                json!([
                    {"model":"lane-alpha","name":"alpha","wins":u64::from(!discard_first),"runs":1},
                    {"model":"lane-beta","name":"beta","wins":0,"runs":1},
                ])
            );
        }
        assert_user_state(&f.project, &head, &index);
        assert_eq!(
            fs::read_to_string(control.join("apply-count"))
                .unwrap_or_default()
                .lines()
                .count(),
            applies
        );
        reopened.engine.shutdown().await.unwrap();
        eprintln!(
            "{}",
            json!({"scenario":scenario,"source_apply_calls":applies,"state":first["state"],"reopened":true})
        );
    }

    #[tokio::test]
    async fn concurrent_keep_and_discard_have_one_durable_outcome() {
        for scenario in [
            "same_keep",
            "different_keep",
            "keep_discard",
            "discard_keep",
        ] {
            let root = tempfile::tempdir().unwrap();
            let bin = root.path().join("bin");
            fs::create_dir(&bin).unwrap();
            let real_git = Command::new("sh")
                .args(["-c", "command -v git"])
                .output()
                .unwrap();
            assert!(real_git.status.success());
            let real_git = String::from_utf8(real_git.stdout).unwrap();
            let wrapper = format!(
                r#"#!/bin/sh
control={control}
apply=no
skip=no
worktree=no
remove=no
for arg do
  case "$arg" in apply) apply=yes;; --check|--cached) skip=yes;; worktree) worktree=yes;; remove) remove=yes;; esac
done
if [ -f "$control/source" ] && [ "$(pwd -P)" = "$(cat "$control/source")" ]; then
  if [ "$apply" = yes ] && [ "$skip" = no ]; then printf 'apply\n' >> "$control/apply-count"; fi
  gate=$(cat "$control/gate")
  if {{ [ "$gate" = apply ] && [ "$apply" = yes ] && [ "$skip" = no ]; }} || {{ [ "$gate" = remove ] && [ "$worktree" = yes ] && [ "$remove" = yes ]; }}; then
    if mkdir "$control/claimed" 2>/dev/null; then
      printf '%s\n' "$$" > "$control/paused"
      count=0
      while [ ! -f "$control/release" ]; do
        count=$((count + 1))
        [ "$count" -lt 1500 ] || exit 97
        sleep 0.01
      done
    fi
  fi
fi
exec {git} "$@"
"#,
                control = quote(root.path().to_str().unwrap()),
                git = quote(real_git.trim())
            );
            fs::write(bin.join("git"), wrapper).unwrap();
            fs::set_permissions(bin.join("git"), fs::Permissions::from_mode(0o755)).unwrap();
            let log_path = root.path().join("child.log");
            let log = fs::File::create(&log_path).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "concurrency::compare_race_child", "--nocapture"])
                .env("SHADOWCODE_COMPARE_RACE_ROOT", root.path())
                .env("SHADOWCODE_COMPARE_RACE_CASE", scenario)
                // Existing fixture location override also keeps all child
                // profile/worktree files under parent-owned timeout cleanup.
                .env("SHADOWCODE_COMPARE_CRASH_ROOT", root.path())
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        bin.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            if result.is_err() {
                let _ = fs::write(root.path().join("release"), b"release\n");
                let _ = child.kill();
            }
            child.wait().unwrap();
            let log = fs::read_to_string(log_path).unwrap();
            assert!(
                result.is_ok_and(|status| status.success()),
                "{scenario}: {log}"
            );
            eprintln!("{scenario}: {log}");
        }
    }

    async fn register(service: &Service, id: &str, name: &str, endpoint: &str) {
        call(
            service,
            "POST",
            "/api/models/register",
            json!({
                "id":id,"name":name,"provider":"local","endpoint":endpoint,"context_limit":16384
            }),
        )
        .await
        .unwrap();
    }

    // Bubblewrap gives the shell a namespace-local PID. Resolve the unique
    // host process in this lane before reading its kernel start time, as in
    // the existing project-inspection cancellation fixtures.
    fn host_pid(project: &Path, namespace_pid: u32) -> u32 {
        let project = project.canonicalize().unwrap();
        let matches: Vec<_> = fs::read_dir("/proc")
            .unwrap()
            .filter_map(|entry| {
                let path = entry.ok()?.path();
                let pid = path.file_name()?.to_str()?.parse::<u32>().ok()?;
                if fs::read_link(path.join("cwd")).ok()? != project {
                    return None;
                }
                let status = fs::read_to_string(path.join("status")).ok()?;
                let inner = status
                    .lines()
                    .find(|line| line.starts_with("NSpid:"))?
                    .split_whitespace()
                    .last()?
                    .parse::<u32>()
                    .ok()?;
                (inner == namespace_pid).then_some(pid)
            })
            .collect();
        assert_eq!(matches.len(), 1, "Expected exactly one live fixture shell");
        matches[0]
    }

    // PID plus kernel start time identifies only our fixture process. This
    // read-only check never signals a PID or confuses a recycled PID with it.
    fn process_identity(pid: u32) -> Option<String> {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(") ")?
            .1
            .split_whitespace()
            .nth(19)
            .map(str::to_owned)
    }

    #[tokio::test]
    async fn unrelated_project_status_and_cancel_finish_while_lane_is_stopping() {
        if std::env::var_os("SHADOWCODE_COMPARE_STOP_ROOT").is_none() {
            // Bubblewrap's namespace teardown can finish before the shell's
            // TERM trap holds. Exercise a genuinely slow owned fallback shell
            // by making only this child's availability probe fail. Never
            // change PATH in the shared test process or claim sandbox coverage.
            let root = tempfile::tempdir().unwrap();
            let bin = root.path().join("bin");
            fs::create_dir(&bin).unwrap();
            fs::write(bin.join("bwrap"), "#!/bin/sh\nexit 1\n").unwrap();
            fs::set_permissions(bin.join("bwrap"), fs::Permissions::from_mode(0o755)).unwrap();
            let log_path = root.path().join("child.log");
            let log = fs::File::create(&log_path).unwrap();
            let mut child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "concurrency::unrelated_project_status_and_cancel_finish_while_lane_is_stopping",
                    "--nocapture",
                ])
                .env("SHADOWCODE_COMPARE_STOP_ROOT", root.path())
                .env("SHADOWCODE_COMPARE_CRASH_ROOT", root.path())
                .env(
                    "PATH",
                    format!(
                        "{}:{}",
                        bin.display(),
                        std::env::var("PATH").unwrap_or_default()
                    ),
                )
                .stdout(Stdio::from(log.try_clone().unwrap()))
                .stderr(Stdio::from(log))
                .spawn()
                .unwrap();
            let result = tokio::time::timeout(Duration::from_secs(60), async {
                loop {
                    if let Some(status) = child.try_wait().unwrap() {
                        break status;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
            })
            .await;
            if result.is_err() {
                let _ = child.kill();
            }
            child.wait().unwrap();
            let log = fs::read_to_string(log_path).unwrap();
            assert!(result.is_ok_and(|status| status.success()), "{log}");
            eprintln!("{log}");
            return;
        }
        let f = fixture().await;
        let (a_head, a_index, _) = dirty_source(&f.project);
        let hold = support::server(|_, body| {
            let answer = if body["tools"].as_array().is_none_or(Vec::is_empty) {
                response("Compare lane", json!([]))
            } else {
                response("Waiting for cancellation", json!([tool("owned-shell", "exec", json!({"command":
                    "trap 'printf stopping > stop-seen; while [ ! -e stop-release ]; do sleep 0.02; done; exit 0' TERM; echo $$ > owned.pid; printf ready > running; while :; do sleep 0.02; done"
                }))]))
            };
            (answer, Duration::ZERO)
        }).await;
        register(&f.service, "lane-hold", "beta", &hold.endpoint).await;
        let a = call(
            &f.service,
            "POST",
            "/api/compare",
            json!({
                "workspace":f.project,"task":"Set the answer","models":["lane-alpha","lane-hold"]
            }),
        )
        .await
        .unwrap();
        let a_id = a["id"].as_str().unwrap().to_owned();
        let held_lane = lane(&a, "lane-hold");
        let held_root = PathBuf::from(held_lane["worktree"].as_str().unwrap());
        let held_job = held_lane["job_id"].as_str().unwrap().to_owned();
        wait_file(&held_root.join("running"), Duration::from_secs(10)).await;
        let namespace_pid: u32 = fs::read_to_string(held_root.join("owned.pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let pid = host_pid(&held_root, namespace_pid);
        let identity =
            process_identity(pid).expect("owned shell must be alive before cancellation");
        let alpha_job = lane(&a, "lane-alpha")["job_id"].as_str().unwrap();
        let alpha = tokio::time::timeout(Duration::from_secs(10), f.service.engine.wait(alpha_job))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alpha.status, "completed");

        // Same Store/Engine and independent repositories: separate services
        // would miss a process-global or shared-store serialization bug.
        let project_b = f._root.path().join("project-b");
        fs::create_dir(&project_b).unwrap();
        git(&project_b, &["init", "-q"]);
        fs::write(project_b.join("lib.txt"), "project B unchanged\n").unwrap();
        git(&project_b, &["add", "."]);
        git(&project_b, &["commit", "-qm", "B base"]);
        let project_b = project_b.canonicalize().unwrap();
        let b_head = git(&project_b, &["rev-parse", "HEAD"]);
        let b_index = git(&project_b, &["write-tree"]);
        let mut trusted = Config::load(&f.paths, None).unwrap().trusted_workspaces;
        trusted.push(project_b.to_string_lossy().into_owned());
        Config::patch(&f.paths, json!({"trusted_workspaces":trusted})).unwrap();
        // A different server is necessary: support::server intentionally
        // serializes responses, including a delayed response body.
        let waiting = support::server(|_, _| {
            (
                response("Late fixture answer", json!([])),
                Duration::from_secs(30),
            )
        })
        .await;
        register(&f.service, "b-alpha", "alpha", &waiting.endpoint).await;
        register(&f.service, "b-beta", "beta", &waiting.endpoint).await;
        let b = call(
            &f.service,
            "POST",
            "/api/compare",
            json!({
                "workspace":project_b,"task":"Wait for cancellation","models":["b-alpha","b-beta"]
            }),
        )
        .await
        .unwrap();
        let b_id = b["id"].as_str().unwrap().to_owned();
        let b_jobs: Vec<String> = b["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["job_id"].as_str().unwrap().to_owned())
            .collect();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if !waiting.requests.lock().unwrap().is_empty()
                    && b_jobs
                        .iter()
                        .all(|id| f.service.engine.job(id).unwrap().unwrap().status == "running")
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();

        let release = ReleaseOnDrop(held_root.join("stop-release"));
        let keep_service = f.service.clone();
        let keep_path = format!("/api/compare/{a_id}/keep");
        let keep = tokio::spawn(async move {
            call(
                &keep_service,
                "POST",
                &keep_path,
                json!({"model":"lane-alpha"}),
            )
            .await
        });
        wait_file(&held_root.join("stop-seen"), Duration::from_secs(10)).await;
        assert_eq!(
            f.service.engine.job(&held_job).unwrap().unwrap().status,
            "cancelling"
        );
        assert!(!keep.is_finished());
        assert_eq!(process_identity(pid).as_ref(), Some(&identity));

        let begin = Instant::now();
        let measured = tokio::time::timeout(Duration::from_secs(1), async {
            let status = call(
                &f.service,
                "GET",
                &format!("/api/compare/{b_id}"),
                Value::Null,
            )
            .await?;
            let status_ms = begin.elapsed().as_millis();
            assert_eq!(status["state"], "running");
            let job = call(
                &f.service,
                "GET",
                &format!("/api/jobs/{}", b_jobs[0]),
                Value::Null,
            )
            .await?;
            assert_eq!(job["status"], "running");
            call(
                &f.service,
                "POST",
                &format!("/api/compare/{b_id}/cancel"),
                Value::Null,
            )
            .await?;
            for id in &b_jobs {
                let cancelled = call(
                    &f.service,
                    "POST",
                    &format!("/api/jobs/{id}/cancel"),
                    Value::Null,
                )
                .await?;
                assert_eq!(cancelled["status"], "cancelled");
            }
            Ok::<_, anyhow::Error>(
                json!({"status_ms":status_ms,"cancelled_ms":begin.elapsed().as_millis()}),
            )
        })
        .await;
        let still_waiting =
            !keep.is_finished() && process_identity(pid).as_ref() == Some(&identity);
        // Always release before asserting latency/results, including timeout.
        fs::write(&release.0, b"release\n").unwrap();
        let kept = tokio::time::timeout(Duration::from_secs(8), keep).await;
        f.service.engine.shutdown().await.unwrap();
        let timing = measured
            .expect("B status/cancellation blocked behind A shutdown")
            .unwrap();
        assert!(
            still_waiting,
            "A exited before B responsiveness was established"
        );
        let kept = kept.unwrap().unwrap().unwrap();
        assert_eq!(kept["state"], "applied");
        assert_eq!(kept["winner"], "lane-alpha");
        assert_eq!(kept["cleanup_pending"], false);
        assert_ne!(
            process_identity(pid).as_ref(),
            Some(&identity),
            "owned child survived shutdown"
        );
        assert_user_state(&f.project, &a_head, &a_index);
        assert_eq!(
            fs::read_to_string(f.project.join("answer.txt")).unwrap(),
            "alpha\n"
        );
        assert_eq!(git(&project_b, &["rev-parse", "HEAD"]), b_head);
        assert_eq!(git(&project_b, &["write-tree"]), b_index);
        assert_eq!(git(&project_b, &["status", "--porcelain=v1"]), "");
        drop(f.service);
        let reopened = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
        let a_saved = call(
            &reopened,
            "GET",
            &format!("/api/compare/{a_id}"),
            Value::Null,
        )
        .await
        .unwrap();
        let b_saved = call(
            &reopened,
            "GET",
            &format!("/api/compare/{b_id}"),
            Value::Null,
        )
        .await
        .unwrap();
        assert_eq!(a_saved["winner"], "lane-alpha");
        assert_eq!(b_saved["state"], "done");
        assert!(b_saved["winner"].is_null());
        for row in b_saved["lanes"].as_array().unwrap() {
            assert_eq!(row["status"], "cancelled");
            assert!(Path::new(row["worktree"].as_str().unwrap()).is_dir());
        }
        let a_board = shadowcode_core::compare::scoreboard(&reopened.engine, &f.project)
            .await
            .unwrap();
        let b_board = shadowcode_core::compare::scoreboard(&reopened.engine, &project_b)
            .await
            .unwrap();
        assert_eq!(
            a_board["rows"],
            json!([
                {"model":"lane-alpha","name":"alpha","wins":1,"runs":1},
                {"model":"lane-hold","name":"beta","wins":0,"runs":1}
            ])
        );
        assert_eq!(
            b_board["rows"],
            json!([
                {"model":"b-alpha","name":"alpha","wins":0,"runs":1},
                {"model":"b-beta","name":"beta","wins":0,"runs":1}
            ])
        );
        call(
            &reopened,
            "POST",
            &format!("/api/compare/{b_id}/discard"),
            Value::Null,
        )
        .await
        .unwrap();
        reopened.engine.shutdown().await.unwrap();
        eprintln!(
            "{}",
            json!({"case":"CMP-11","service":"shared","process_scope":"owned fallback shell; isolated failed bubblewrap probe","timing":timing,"a_still_stopping_after_b":still_waiting})
        );
    }
}

// Linux mode-bit failure is checked with a real unlink before qualification.
// Privileged/DAC-bypassing runs must fail rather than claim an exercised fault.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn filesystem_deletion_failure_preserves_applied_winner_and_retries_after_restart() {
    use std::os::unix::fs::PermissionsExt;

    struct RestoreMode {
        paths: [PathBuf; 2],
        mode: u32,
    }
    impl RestoreMode {
        fn restore(&self) {
            for path in &self.paths {
                if path.exists() {
                    fs::set_permissions(path, fs::Permissions::from_mode(self.mode)).unwrap();
                }
            }
        }
    }
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            for path in &self.paths {
                if path.exists() {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(self.mode));
                }
            }
        }
    }
    async fn assert_score(service: &Service) {
        let board = call(service, "GET", "/api/compare/scoreboard", Value::Null)
            .await
            .unwrap();
        assert_eq!(
            board["rows"],
            json!([
                {"model":"lane-alpha","name":"alpha","wins":1,"runs":1},
                {"model":"lane-beta","name":"beta","wins":0,"runs":1},
            ])
        );
    }

    let f = fixture().await;
    let project = f.project.clone();
    fs::write(project.join("tracked.txt"), "staged user contents\n").unwrap();
    git(&project, &["add", "tracked.txt"]);
    fs::write(project.join("tracked.txt"), "unstaged user contents\n").unwrap();
    fs::write(project.join("notes.txt"), "untracked user contents\n").unwrap();
    fs::write(project.join("ignored.log"), "ignored user contents\n").unwrap();
    let head = git(&project, &["rev-parse", "HEAD"]);
    let index = git(&project, &["write-tree"]);
    let assert_source = |answer: &str| {
        assert_eq!(git(&project, &["rev-parse", "HEAD"]), head);
        assert_eq!(git(&project, &["write-tree"]), index);
        assert_eq!(
            git(&project, &["show", ":tracked.txt"]),
            "staged user contents"
        );
        for (path, expected) in [
            ("tracked.txt", "unstaged user contents\n"),
            ("notes.txt", "untracked user contents\n"),
            ("ignored.log", "ignored user contents\n"),
            ("lib.txt", "value = 2 (alpha)\n"),
            ("answer.txt", answer),
        ] {
            assert_eq!(fs::read_to_string(project.join(path)).unwrap(), expected);
        }
        assert!(!project.join("beta.txt").exists());
    };

    let started = start(&f).await;
    let id = started["id"].as_str().unwrap().to_owned();
    let ready = finished(&f, &id).await;
    assert!(ready["lanes"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["status"] == "completed"));
    let alpha = PathBuf::from(lane(&ready, "lane-alpha")["worktree"].as_str().unwrap());
    let beta = PathBuf::from(lane(&ready, "lane-beta")["worktree"].as_str().unwrap());
    let beta_id = lane(&ready, "lane-beta")["worktree_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let retained = beta
        .parent()
        .unwrap()
        .join(".cleanup")
        .join(&beta_id)
        .join("checkout");
    let protected = beta.join("blocked");
    fs::create_dir(&protected).unwrap();
    let probe = protected.join("recoverable.txt");
    fs::write(&probe, "recoverable losing lane data\n").unwrap();
    let restore = RestoreMode {
        paths: [protected.clone(), retained.join("blocked")],
        mode: fs::metadata(&protected).unwrap().permissions().mode(),
    };
    // A nested permission fault permits root relocation, then makes real Git
    // deletion fail after it has already removed some other lane files.
    fs::set_permissions(
        &protected,
        fs::Permissions::from_mode(restore.mode & !0o222),
    )
    .unwrap();
    let denied = fs::remove_file(&probe).expect_err(
        "The OS bypassed the mode-bit deletion fault; run this qualification without root/DAC override",
    );
    assert_eq!(denied.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(probe.is_file());

    let kept = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha"}),
    )
    .await
    .unwrap();
    assert_eq!(kept["state"], "applied");
    assert_eq!(kept["winner"], "lane-alpha");
    assert_eq!(kept["cleanup_pending"], true);
    let recovery = kept["recovery"].clone();
    assert_eq!(recovery["phase"], "applied");
    assert!(!recovery["operation_id"].as_str().unwrap().is_empty());
    let applied_files = kept["applied_files"].clone();
    assert_eq!(lane(&kept, "lane-alpha")["removed"], true);
    assert_eq!(lane(&kept, "lane-beta")["removed"], false);
    assert!(!alpha.exists());
    assert!(!beta.exists(), "{kept}");
    assert!(retained.is_dir(), "{kept}");
    assert_eq!(
        fs::read_to_string(retained.join("blocked/recoverable.txt")).unwrap(),
        "recoverable losing lane data\n"
    );
    let cleanup_journal: Value = serde_json::from_slice(
        &fs::read(
            f.paths
                .data
                .join("managed-worktrees/records/cleanup")
                .join(format!("{beta_id}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(cleanup_journal["relocated"], true);
    assert!(cleanup_journal["admin"]["entries"]["index"]["data"]
        .as_str()
        .is_some_and(|value| !value.is_empty()));
    assert!(
        kept["notes"].as_array().unwrap().iter().any(|note| {
            let note = note.as_str().unwrap();
            note.contains("Git worktree operation failed") && note.contains(beta.to_str().unwrap())
        }),
        "Expected actual Git deletion failure, not an ownership refusal: {kept}"
    );
    let records = worktrees::list(&f.paths, &project).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].id, beta_id);
    assert_eq!(records[0].state, "removing");
    assert_source("alpha\n");
    assert_score(&f.service).await;
    eprintln!(
        "{}",
        json!({"case":"CMP-04","phase":"filesystem_error","unlink_errno":denied.raw_os_error(),"notes":kept["notes"],"remaining_git_registration":git(&project, &["worktree", "list", "--porcelain"])})
    );

    f.service.engine.shutdown().await.unwrap();
    drop(f.service);
    let reopened = Service::open(f.paths.clone(), Some(project.clone())).unwrap();
    let restored = call(&reopened, "GET", &format!("/api/compare/{id}"), Value::Null)
        .await
        .unwrap();
    assert_eq!(restored["state"], "applied");
    assert_eq!(restored["winner"], "lane-alpha");
    assert_eq!(restored["cleanup_pending"], true);
    assert_eq!(restored["recovery"], recovery);
    assert_eq!(restored["applied_files"], applied_files);
    // Cleanup must never reapply the winner over later user edits.
    fs::write(project.join("answer.txt"), "user edited after Keep\n").unwrap();
    let pending = call(
        &reopened,
        "POST",
        &format!("/api/compare/{id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(pending["state"], "applied");
    assert_eq!(pending["winner"], "lane-alpha");
    assert_eq!(pending["cleanup_pending"], true);
    assert_eq!(pending["recovery"], recovery);
    assert_eq!(pending["applied_files"], applied_files);
    assert_source("user edited after Keep\n");
    assert_score(&reopened).await;

    restore.restore();
    for retry in 0..2 {
        let cleaned = call(
            &reopened,
            "POST",
            &format!("/api/compare/{id}/discard"),
            Value::Null,
        )
        .await
        .unwrap();
        // Preserve diagnostics even if this intended recovery assertion fails.
        eprintln!(
            "{}",
            json!({"case":"CMP-04","phase":"permission_restored","retry":retry,"cleanup_pending":cleaned["cleanup_pending"],"notes":cleaned["notes"]})
        );
        assert_eq!(cleaned["state"], "applied");
        assert_eq!(cleaned["winner"], "lane-alpha");
        assert_eq!(cleaned["recovery"], recovery);
        assert_eq!(cleaned["applied_files"], applied_files);
        assert_source("user edited after Keep\n");
        assert_score(&reopened).await;
        assert_eq!(
            cleaned["cleanup_pending"], false,
            "Owned deletion failure must be retryable after permission repair: {cleaned}"
        );
        assert!(cleaned["lanes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["removed"] == true));
    }
    assert!(!beta.exists());
    assert!(!retained.exists());
    assert!(worktrees::list(&f.paths, &project).unwrap().is_empty());
    assert_eq!(managed_branches(&project), "");
    assert_eq!(git(&project, &["worktree", "list"]).lines().count(), 1);
    reopened.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn keep_refuses_a_lane_switched_to_a_user_branch_before_staging() {
    let f = fixture().await;
    let started = start(&f).await;
    let id = started["id"].as_str().unwrap();
    let ready = finished(&f, id).await;
    let alpha = PathBuf::from(lane(&ready, "lane-alpha")["worktree"].as_str().unwrap());
    git(&alpha, &["switch", "-c", "user-work"]);
    let head = git(&alpha, &["rev-parse", "HEAD"]);
    let git_dir = PathBuf::from(git(&alpha, &["rev-parse", "--absolute-git-dir"]));
    let index = fs::read(git_dir.join("index")).unwrap();
    let result = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha","accept_unverified":true}),
    )
    .await;
    let branch_after = git(&f.project, &["rev-parse", "refs/heads/user-work"]);
    let index_after = fs::read(git_dir.join("index")).unwrap();
    let checkout_retained = alpha.is_dir();
    let source_unchanged = !f.project.join("answer.txt").exists()
        && fs::read_to_string(f.project.join("lib.txt")).unwrap() == "value = 1\n";
    f.service.engine.shutdown().await.unwrap();
    assert!(result.is_err(), "Keep accepted a user branch: {result:?}");
    assert_eq!(
        branch_after, head,
        "Keep must not commit onto a user branch"
    );
    assert_eq!(index_after, index, "Refusal must happen before staging");
    assert!(checkout_retained && source_unchanged);
}

#[tokio::test]
async fn keep_refuses_a_lane_redirected_to_a_foreign_git_directory_before_staging() {
    let f = fixture().await;
    let started = start(&f).await;
    let id = started["id"].as_str().unwrap();
    let ready = finished(&f, id).await;
    let alpha = PathBuf::from(lane(&ready, "lane-alpha")["worktree"].as_str().unwrap());
    let foreign = f._root.path().join("foreign");
    fs::create_dir(&foreign).unwrap();
    git(&foreign, &["init", "-q"]);
    fs::write(foreign.join("private.txt"), "user data\n").unwrap();
    git(&foreign, &["add", "."]);
    git(&foreign, &["commit", "-qm", "User data"]);
    let head = git(&foreign, &["rev-parse", "HEAD"]);
    let index = fs::read(foreign.join(".git/index")).unwrap();
    let git_file = fs::read(alpha.join(".git")).unwrap();
    fs::write(
        alpha.join(".git"),
        format!("gitdir: {}\n", foreign.join(".git").display()),
    )
    .unwrap();
    let result = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/keep"),
        json!({"model":"lane-alpha","accept_unverified":true}),
    )
    .await;
    let head_after = git(&foreign, &["rev-parse", "HEAD"]);
    let index_after = fs::read(foreign.join(".git/index")).unwrap();
    let source_unchanged = !f.project.join("answer.txt").exists();
    fs::write(alpha.join(".git"), git_file).unwrap();
    f.service.engine.shutdown().await.unwrap();
    assert!(
        result.is_err(),
        "Keep accepted a foreign Git directory: {result:?}"
    );
    assert_eq!(head_after, head, "Foreign branch must not receive a commit");
    assert_eq!(index_after, index, "Foreign index must not be staged");
    assert!(source_unchanged && alpha.is_dir());
    assert_eq!(
        fs::read_to_string(foreign.join("private.txt")).unwrap(),
        "user data\n"
    );
}

#[tokio::test]
async fn discard_preserves_a_lane_with_an_active_background_process_until_retry() {
    let f = fixture().await;
    let started = start(&f).await;
    let id = started["id"].as_str().unwrap();
    let ready = finished(&f, id).await;
    let beta = PathBuf::from(lane(&ready, "lane-beta")["worktree"].as_str().unwrap());
    let config = Config::load(&f.paths, Some(&beta)).unwrap();
    let task = f
        .service
        .engine
        .background()
        .start(&beta, &config, None, "held-fixture", "sleep 300")
        .unwrap();
    let discarded = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    let retained = beta.is_dir();
    // Stop and join the owned child before any assertion can panic.
    f.service.engine.background().stop(&task.id).await.unwrap();
    let retried = call(
        &f.service,
        "POST",
        &format!("/api/compare/{id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    f.service.engine.shutdown().await.unwrap();
    assert!(
        retained,
        "Discard removed a live background process's checkout"
    );
    assert_eq!(discarded["cleanup_pending"], true);
    assert_eq!(lane(&discarded, "lane-beta")["removed"], false);
    assert_eq!(retried["cleanup_pending"], false);
    assert!(!beta.exists());
}

/// LOC-05 core boundary: a real nonretryable provider response must not
/// invalidate the other lane's files or configured-check receipt.
#[tokio::test]
async fn failed_generation_preserves_checked_candidate_through_reopen_and_keep() {
    use anyhow::{ensure, Context};
    use futures_util::FutureExt;
    use std::sync::{Arc, Mutex};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    const CHECK: &str = "python3 check_candidate.py";
    let f = fixture().await;
    fs::write(f.project.join("check_candidate.py"), "from pathlib import Path\nassert Path('answer.txt').read_text() == 'alpha\\n'\nassert Path('lib.txt').read_text() == 'value = 2 (alpha)\\n'\nprint('candidate-check-passed')\n").unwrap();
    git(&f.project, &["add", "check_candidate.py"]);
    git(&f.project, &["commit", "-qm", "Candidate check"]);
    fs::write(f.project.join("tracked.txt"), "staged user bytes\n").unwrap();
    git(&f.project, &["add", "tracked.txt"]);
    fs::write(f.project.join("tracked.txt"), "unstaged user bytes\n").unwrap();
    fs::write(f.project.join("notes.txt"), "unrelated user bytes\n").unwrap();
    let head = git(&f.project, &["rev-parse", "HEAD"]);
    let index = fs::read(f.project.join(".git/index")).unwrap();
    Config::patch(&f.paths, json!({"verification":{"commands":[CHECK]}})).unwrap();
    let mut service = Some(f.service);

    let success = support::server(|_, body| {
        let messages = body["messages"].as_array().cloned().unwrap_or_default();
        let edited = messages.iter().any(|m| m["role"] == "tool");
        let checked = messages.iter().any(|m| m["name"] == "exec");
        let value = if edited && !checked {
            response(
                "Checking the candidate",
                json!([tool("check", "exec", json!({"command":CHECK}))]),
            )
        } else {
            reply(body)
        };
        (value, Duration::ZERO)
    })
    .await;
    // A separate listener lets alpha finish while beta's second request is
    // held. The ordinary support server handles one response at a time.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let (entered, entered_rx) = tokio::sync::oneshot::channel();
    let release = CancellationToken::new();
    let released = release.clone();
    let stop = CancellationToken::new();
    let stopped = stop.clone();
    let mut worker = tokio::spawn(async move {
        let exchange = async move {
            let mut entered = Some(entered);
            loop {
                let (mut socket, _) = listener.accept().await?;
                let body = tokio::time::timeout(Duration::from_secs(10), async {
                    let mut wire = Vec::new();
                    let mut buffer = [0; 8192];
                    loop {
                        let n = socket.read(&mut buffer).await?;
                        ensure!(n > 0, "beta request ended before body");
                        wire.extend_from_slice(&buffer[..n]);
                        ensure!(wire.len() < 1_000_000, "beta request byte limit");
                        if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                            let headers = String::from_utf8_lossy(&wire[..end]).to_lowercase();
                            let len: usize = headers
                                .lines()
                                .find_map(|line| {
                                    line.strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse().ok())
                                })
                                .context("beta needs Content-Length")?;
                            ensure!(len < 1_000_000, "beta declared body limit");
                            if wire.len() >= end + 4 + len {
                                return Ok::<Value, anyhow::Error>(serde_json::from_slice(
                                    &wire[end + 4..end + 4 + len],
                                )?);
                            }
                        }
                    }
                })
                .await
                .context("beta request timeout")??;
                let index = {
                    let mut requests = captured.lock().unwrap();
                    ensure!(requests.len() < 8, "beta request count limit");
                    let index = requests.len();
                    requests.push(body.clone());
                    index
                };
                ensure!(body["model"] == "beta", "wrong target reached beta fixture");
                let (status, value) = if index == 0 {
                    (
                        "200 OK",
                        response(
                            "Partial beta result",
                            json!([tool(
                                "partial",
                                "write_file",
                                json!({"path":"beta-partial.txt","content":"retained beta work\n","expected_hash":"missing"})
                            )]),
                        ),
                    )
                } else {
                    if index == 1 {
                        ensure!(
                            body["messages"].as_array().is_some_and(|messages| messages
                                .iter()
                                .any(|m| m["role"] == "tool" && m["name"] == "write_file")),
                            "beta did not receive its actual tool result"
                        );
                        let _ = entered.take().context("duplicate beta barrier")?.send(());
                        tokio::time::timeout(Duration::from_secs(30), released.cancelled())
                            .await
                            .context("beta failure barrier was not released")?;
                    }
                    // HTTP400 is deterministic/nonretryable, not a connection
                    // timeout or a manufactured persisted job status.
                    (
                        "400 Bad Request",
                        json!({"error":{"message":"comparison beta fixture rejected request"}}),
                    )
                };
                let text = value.to_string();
                tokio::time::timeout(Duration::from_secs(5), socket.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).as_bytes())).await.context("beta response timeout")??;
            }
        };
        tokio::select! {
            _ = stopped.cancelled() => Ok::<(), anyhow::Error>(()),
            result = exchange => result,
        }
    });

    let observed = std::panic::AssertUnwindSafe(async {
        for (name, endpoint) in [("alpha", success.endpoint.as_str()), ("beta", endpoint.as_str())] {
            call(service.as_ref().unwrap(), "POST", "/api/models/register", json!({"id":format!("failure-{name}"),"name":name,"provider":"local","endpoint":endpoint,"context_limit":16384})).await?;
        }
        let started = call(service.as_ref().unwrap(), "POST", "/api/compare", json!({"workspace":f.project,"task":"Set the answer and check it","models":["failure-alpha","failure-beta"]})).await?;
        let id = started["id"].as_str().context("comparison ID")?.to_owned();
        let alpha_id = lane(&started,"failure-alpha")["job_id"].as_str().context("alpha job")?.to_owned();
        let beta_id = lane(&started,"failure-beta")["job_id"].as_str().context("beta job")?.to_owned();
        let alpha_path = PathBuf::from(lane(&started,"failure-alpha")["worktree"].as_str().context("alpha worktree")?);
        let beta_path = PathBuf::from(lane(&started,"failure-beta")["worktree"].as_str().context("beta worktree")?);
        tokio::time::timeout(Duration::from_secs(15), entered_rx).await.context("beta never reached failure barrier")??;
        let alpha = tokio::time::timeout(Duration::from_secs(15), service.as_ref().unwrap().engine.wait(&alpha_id)).await.context("alpha did not finish while beta was held")??;
        let before = call(service.as_ref().unwrap(), "GET", &format!("/api/compare/{id}"), Value::Null).await?;
        ensure!(alpha.status == "completed" && before["state"] == "running", "held comparison state: {before}");
        let checks = lane(&before,"failure-alpha")["checks"].clone();
        ensure!(checks["passed"] == 1 && checks["failed"] == 0 && checks["incomplete"] == 0, "alpha check did not pass: {before}");
        ensure!(checks["commands"][0]["command"] == CHECK && checks["commands"][0]["exit_code"] == 0, "wrong configured check");
        let receipt = service.as_ref().unwrap().engine.store().last_task_event(&alpha.task_id,"verification.summary")?.context("missing alpha verification receipt")?;
        let commands = receipt["payload"]["commands"].as_array().context("missing receipt commands")?;
        ensure!(alpha.result.as_ref().context("missing alpha result")?["verification"]["commands"] == receipt["payload"]["commands"], "job result and durable receipt diverged");
        let check_receipts: Vec<_> = commands.iter().filter(|r| r["kind"] == "configured_check" && r["command"] == CHECK).collect();
        ensure!(check_receipts.len() == 1, "missing or duplicate configured receipt");
        let check_receipt = check_receipts[0];
        ensure!(check_receipt["task_id"] == alpha.task_id && check_receipt["success"] == true && check_receipt["exit_code"] == 0 && check_receipt["state"] == "passed", "invalid initial check receipt");
        let call_id = check_receipt["tool_call_id"].as_str().filter(|id| !id.is_empty() && *id != "[redacted secret]").context("missing owned tool identity")?;
        let completed_id: i64 = check_receipt["output_ref"].as_str().and_then(|r| r.strip_prefix("event:")).context("missing exact output_ref")?.parse()?;
        ensure!(completed_id > 0, "invalid output reference");
        let completion = service.as_ref().unwrap().engine.store().events_after(&alpha.session_id, completed_id - 1, Some(completed_id), 1)?.pop().context("referenced command completion missing")?;
        ensure!(completion["id"] == completed_id && completion["task_id"] == alpha.task_id && completion["session_id"] == alpha.session_id && completion["type"] == "tool.completed", "foreign/non-completion output reference");
        ensure!(completion["payload"]["tool"] == "exec" && completion["payload"]["call_id"] == call_id && completion["payload"]["success"] == true && completion["payload"]["output"]["exit_code"] == 0 && completion["payload"]["output"]["stdout"] == "candidate-check-passed\n", "receipt does not name the actual successful check output: {completion}");
        let events = service.as_ref().unwrap().engine.store().recent_events(&alpha.session_id, 100)?;
        let starts: Vec<_> = events.iter().filter(|e| e["task_id"] == alpha.task_id && e["type"] == "tool.started" && e["payload"]["tool"] == "exec" && e["payload"]["call_id"] == call_id).collect();
        ensure!(starts.len() == 1 && starts[0]["payload"]["arguments"]["command"] == CHECK, "command identity did not start exact check");
        ensure!(fs::read_to_string(alpha_path.join("answer.txt"))? == "alpha\n", "alpha output missing before failure");
        ensure!(fs::read_to_string(beta_path.join("beta-partial.txt"))? == "retained beta work\n", "beta's tool never ran");
        release.cancel();
        let beta = tokio::time::timeout(Duration::from_secs(15), service.as_ref().unwrap().engine.wait(&beta_id)).await.context("beta did not fail after release")??;
        let alpha_requests = success.requests.lock().unwrap().len();
        ensure!(beta.status == "failed" && beta.summary.contains("400"), "wrong beta failure: {beta:?}");
        for reopened in [false, true] {
            if reopened {
                service.as_ref().unwrap().engine.shutdown().await?;
                drop(service.take());
                service = Some(Service::open(f.paths.clone(), Some(f.project.clone()))?);
            }
            let current = service.as_ref().unwrap();
            let done = call(current,"GET",&format!("/api/compare/{id}"),Value::Null).await?;
            ensure!(done["state"] == "done" && done["winner"].is_null(), "comparison outcome lost: {done}");
            ensure!(lane(&done,"failure-alpha")["job_id"] == alpha_id && lane(&done,"failure-alpha")["status"] == "completed" && lane(&done,"failure-alpha")["summary"] == alpha.summary, "successful lane changed: {done}");
            ensure!(lane(&done,"failure-beta")["job_id"] == beta_id && lane(&done,"failure-beta")["status"] == "failed" && lane(&done,"failure-beta")["error"].as_str().is_some_and(|s| !s.is_empty()), "failed lane outcome missing: {done}");
            ensure!(lane(&done,"failure-alpha")["checks"] == checks && current.engine.store().last_task_event(&alpha.task_id,"verification.summary")? == Some(receipt.clone()), "successful receipt changed after beta failed/reopen: {done}");
            ensure!(current.engine.store().events_after(&alpha.session_id, completed_id - 1, Some(completed_id), 1)?.pop() == Some(completion.clone()), "durable check completion changed after failure/reopen");
            ensure!(alpha_path.is_dir() && beta_path.is_dir() && lane(&done,"failure-alpha")["removed"] == false && lane(&done,"failure-beta")["removed"] == false, "failure removed recovery material");
            ensure!(fs::read_to_string(alpha_path.join("answer.txt"))? == "alpha\n" && fs::read_to_string(alpha_path.join("lib.txt"))? == "value = 2 (alpha)\n", "alpha files changed");
            ensure!(fs::read_to_string(beta_path.join("beta-partial.txt"))? == "retained beta work\n", "failed lane partial work changed");
            ensure!(fs::read(f.project.join(".git/index"))? == index && git(&f.project,&["rev-parse","HEAD"]) == head, "source index/HEAD changed before Keep");
            ensure!(fs::read_to_string(f.project.join("tracked.txt"))? == "unstaged user bytes\n" && fs::read_to_string(f.project.join("notes.txt"))? == "unrelated user bytes\n", "user work changed before Keep");
            ensure!(!f.project.join("answer.txt").exists() && !f.project.join("beta-partial.txt").exists() && fs::read_to_string(f.project.join("lib.txt"))? == "value = 1\n", "failure changed source");
            for _ in 0..2 {
                let board = call(current,"GET","/api/compare/scoreboard",Value::Null).await?;
                ensure!(board["rows"].as_array().context("scoreboard rows")?.iter().all(|row| row["runs"] == 1 && row["wins"] == 0), "duplicate run count: {board}");
            }
        }
        let kept = call(service.as_ref().unwrap(),"POST",&format!("/api/compare/{id}/keep"),json!({"model":"failure-alpha"})).await?;
        ensure!(kept["state"] == "applied" && kept["winner"] == "failure-alpha" && lane(&kept,"failure-alpha")["checks"] == checks, "checked winner could not be kept: {kept}");
        service.as_ref().unwrap().engine.shutdown().await?;
        drop(service.take());
        service = Some(Service::open(f.paths.clone(),Some(f.project.clone()))?);
        for _ in 0..2 {
            let retried = call(service.as_ref().unwrap(),"POST",&format!("/api/compare/{id}/discard"),Value::Null).await?;
            ensure!(retried["state"] == "applied" && retried["winner"] == "failure-alpha" && retried["cleanup_pending"] == false && lane(&retried,"failure-alpha")["checks"] == checks, "applied result changed on cleanup retry: {retried}");
        }
        let board = call(service.as_ref().unwrap(),"GET","/api/compare/scoreboard",Value::Null).await?;
        let scores = board["rows"].as_array().context("scoreboard rows")?;
        ensure!(scores.len() == 2 && scores.iter().all(|row| row["runs"] == 1 && row["wins"] == if row["model"] == "failure-alpha" { 1 } else { 0 }), "winner rescored: {board}");
        ensure!(fs::read_to_string(f.project.join("answer.txt"))? == "alpha\n" && fs::read_to_string(f.project.join("lib.txt"))? == "value = 2 (alpha)\n" && !f.project.join("beta-partial.txt").exists(), "wrong files applied");
        ensure!(fs::read(f.project.join(".git/index"))? == index && git(&f.project,&["rev-parse","HEAD"]) == head, "source index/HEAD changed");
        ensure!(fs::read_to_string(f.project.join("tracked.txt"))? == "unstaged user bytes\n" && fs::read_to_string(f.project.join("notes.txt"))? == "unrelated user bytes\n", "unrelated user work changed");
        ensure!(service.as_ref().unwrap().engine.store().last_task_event(&alpha.task_id,"verification.summary")? == Some(receipt.clone()), "durable receipt changed after Keep");
        ensure!(service.as_ref().unwrap().engine.store().events_after(&alpha.session_id, completed_id - 1, Some(completed_id), 1)?.pop() == Some(completion.clone()), "durable check completion changed after Keep");
        ensure!(success.requests.lock().unwrap().len() == alpha_requests, "reopen/Keep replayed successful generation");
        ensure!(requests.lock().unwrap().len() == 2 && f.server.requests.lock().unwrap().is_empty(), "beta retried or old endpoint used");
        ensure!(service.as_ref().unwrap().engine.store().last_task_event(&beta.task_id,"model.retry")?.is_none(), "nonretryable failure retried");
        Ok(json!({"comparison":id,"alpha_job":alpha_id,"beta_job":beta_id,"checks":checks,"receipt":receipt,"completion":completion,"scoreboard":board,"beta_requests":requests.lock().unwrap().len()}))
    }).catch_unwind().await;
    // Release/join fixture work and stop engine children before outcome asserts.
    release.cancel();
    let shutdown = if let Some(service) = service.as_ref() {
        service.engine.shutdown().await
    } else {
        Ok(())
    };
    stop.cancel();
    let joined = tokio::time::timeout(Duration::from_secs(5), &mut worker).await;
    if joined.is_err() {
        worker.abort();
        let _ = worker.await;
    }
    eprintln!(
        "COMPARE_LANE_FAILURE panic={}; result={:?}; shutdown={shutdown:?}; listener={joined:?}",
        observed.is_err(),
        observed.as_ref().ok()
    );
    assert!(shutdown.is_ok(), "{shutdown:?}");
    assert!(matches!(joined, Ok(Ok(Ok(())))), "{joined:?}");
    match observed {
        Ok(result) => {
            result.expect("Compare failed-lane isolation regression");
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
