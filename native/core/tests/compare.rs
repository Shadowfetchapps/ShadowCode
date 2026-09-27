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
