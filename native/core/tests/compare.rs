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
    assert_eq!(managed_branches(project), "");
    assert_eq!(git(project, &["worktree", "list"]).lines().count(), 1);
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
    assert!(local.contains("one local model"), "{local}");
    assert!(local.contains("GPU memory"), "{local}");
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
            _ => assert!(assessed.is_err()),
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
            assert!(Path::new(lane["worktree"].as_str().unwrap()).exists());
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
