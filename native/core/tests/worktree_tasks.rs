//! Worktree tasks ("Run in new worktree"): a task in a fresh managed
//! worktree runs beside a task in the project's main checkout, and its result
//! is applied, kept as a branch or discarded. Loopback models on one fake
//! OpenAI-compatible server; no vendor CLI or network is used.
mod support;
use anyhow::Result;
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
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

fn git(root: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "user.name=Worktree Test",
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

/// "alpha" (the worktree task) edits lib.txt and adds answer.txt; "main"
/// (the main checkout's task) adds main.txt.
fn reply(body: &Value) -> Value {
    let messages = body["messages"].as_array().cloned().unwrap_or_default();
    if body["tools"].as_array().is_none_or(Vec::is_empty) {
        return response("Worktree task", json!([]));
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
            json!([tool(
                "m1",
                "write_file",
                json!({"path":"main.txt","content":"main\n","expected_hash":"missing"})
            )]),
        ),
    }
}

struct Fixture {
    _server: support::Server,
    _root: tempfile::TempDir,
    project: PathBuf,
    service: Service,
    delay_ms: Arc<AtomicU64>,
}

async fn fixture() -> Fixture {
    let delay_ms = Arc::new(AtomicU64::new(0));
    let delay = delay_ms.clone();
    let server = support::server(move |_, body| {
        (
            reply(body),
            Duration::from_millis(delay.load(Ordering::Acquire)),
        )
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    git(&project, &["init", "-q"]);
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
            "model": {"provider":"local","endpoint":server.endpoint,"name":"main","context_limit":16384},
            "agent": {"max_steps": 8},
            "cli_agents": {"enabled": false}
        }),
    )
    .unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    for name in ["alpha", "main"] {
        call(
            &service,
            "POST",
            "/api/models/register",
            json!({"id":format!("m-{name}"),"name":name,"provider":"local","endpoint":server.endpoint,"context_limit":16384}),
        )
        .await
        .unwrap();
    }
    Fixture {
        _server: server,
        _root: root,
        project,
        service,
        delay_ms,
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

async fn run_in_worktree(f: &Fixture) -> Value {
    call(
        &f.service,
        "POST",
        "/api/run",
        json!({"workspace":f.project,"task":"Set the answer","model":"m-alpha","worktree":true}),
    )
    .await
    .unwrap()
}

async fn finished(f: &Fixture, job: &str) -> Value {
    let started = Instant::now();
    loop {
        let job = call(&f.service, "GET", &format!("/api/jobs/{job}"), Value::Null)
            .await
            .unwrap();
        if !matches!(
            job["status"].as_str(),
            Some("queued" | "running" | "paused" | "cancelling")
        ) {
            return job;
        }
        assert!(started.elapsed() < Duration::from_secs(60), "{job:#}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn task(f: &Fixture, id: &str) -> Value {
    call(
        &f.service,
        "GET",
        &format!("/api/worktree-tasks/{id}"),
        Value::Null,
    )
    .await
    .unwrap()
}

fn managed_branches(project: &Path) -> String {
    git(project, &["branch", "--list", "shadowcode/*"])
}

#[tokio::test]
async fn a_worktree_task_runs_beside_the_main_checkout_and_applies() {
    let f = fixture().await;
    let project = &f.project;
    fs::write(project.join("notes.txt"), "uncommitted\n").unwrap();
    f.delay_ms.store(700, Ordering::Release);

    let main = call(
        &f.service,
        "POST",
        "/api/run",
        json!({"workspace":project,"task":"Add main","model":"m-main"}),
    )
    .await
    .unwrap();
    // The checkout's one-active-task rule still holds.
    let refused = call(
        &f.service,
        "POST",
        "/api/run",
        json!({"workspace":project,"task":"Another","model":"m-main"}),
    )
    .await
    .unwrap_err();
    assert!(
        format!("{refused:#}").contains("active task"),
        "{refused:#}"
    );

    let started = run_in_worktree(&f).await;
    let record = &started["worktree_task"];
    let id = record["id"].as_str().unwrap().to_owned();
    let worktree = PathBuf::from(record["worktree"].as_str().unwrap());
    assert_eq!(record["state"], "running");
    assert_eq!(record["workspace"], json!(project));
    assert_eq!(started["workspace"], json!(worktree));
    assert_ne!(started["session_id"], main["session_id"]);
    assert_eq!(
        fs::read_to_string(worktree.join("notes.txt")).unwrap(),
        "uncommitted\n"
    );
    // Both run at the same time.
    let both = Instant::now();
    loop {
        let a = call(
            &f.service,
            "GET",
            &format!("/api/jobs/{}", main["id"].as_str().unwrap()),
            Value::Null,
        )
        .await
        .unwrap();
        let b = call(
            &f.service,
            "GET",
            &format!("/api/jobs/{}", started["id"].as_str().unwrap()),
            Value::Null,
        )
        .await
        .unwrap();
        if a["status"] == "running" && b["status"] == "running" {
            break;
        }
        assert!(
            both.elapsed() < Duration::from_secs(20),
            "{} / {}",
            a["status"],
            b["status"]
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    f.delay_ms.store(0, Ordering::Release);
    assert_eq!(
        finished(&f, main["id"].as_str().unwrap()).await["status"],
        "completed"
    );
    assert_eq!(
        finished(&f, started["id"].as_str().unwrap()).await["status"],
        "completed"
    );
    assert!(project.join("main.txt").is_file());
    assert!(!project.join("answer.txt").exists());
    let models: Vec<Value> = f
        ._server
        .requests
        .lock()
        .unwrap()
        .iter()
        .map(|body| body["model"].clone())
        .collect();
    assert!(models.contains(&json!("alpha")) && models.contains(&json!("main")));

    let record = task(&f, &id).await;
    assert_eq!(record["state"], "done", "{record:#}");
    let changed: Vec<&str> = record["changed_files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|file| file["path"].as_str().unwrap())
        .collect();
    assert_eq!(changed, vec!["answer.txt", "lib.txt"]);

    // Listed under its project, and never a project of its own.
    let sid = started["session_id"].as_str().unwrap();
    let listed = call(
        &f.service,
        "GET",
        &format!("/api/sessions?workspace={}", project.display()),
        Value::Null,
    )
    .await
    .unwrap();
    let row = listed["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["id"] == sid)
        .cloned()
        .unwrap();
    assert_eq!(row["worktree_task"], id.as_str());
    assert_eq!(row["worktree_source"], json!(project));
    let projects = call(&f.service, "GET", "/api/projects", Value::Null)
        .await
        .unwrap();
    assert!(!projects.to_string().contains(&*worktree.to_string_lossy()));
    let session = call(
        &f.service,
        "GET",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(session["worktree"]["id"], id.as_str());
    // Its worktree would be orphaned: the conversation is not deleted.
    let error = call(
        &f.service,
        "DELETE",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("own worktree"), "{error:#}");

    let applied = call(
        &f.service,
        "POST",
        &format!("/api/worktree-tasks/{id}/apply"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(applied["state"], "applied", "{applied:#}");
    assert_eq!(applied["removed"], true);
    assert_eq!(
        fs::read_to_string(project.join("lib.txt")).unwrap(),
        "value = 2 (alpha)\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("answer.txt")).unwrap(),
        "alpha\n"
    );
    assert!(project.join("main.txt").is_file());
    assert!(!worktree.exists());
    assert_eq!(managed_branches(project), "");
    // Applied to the working tree only.
    assert_eq!(git(project, &["log", "--format=%s"]), "Base");
    let session = call(
        &f.service,
        "GET",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(session["workspace"], json!(project));
    assert!(session["worktree"].is_null());
    // A second apply is refused.
    let again = call(
        &f.service,
        "POST",
        &format!("/api/worktree-tasks/{id}/apply"),
        Value::Null,
    )
    .await
    .unwrap_err();
    assert!(format!("{again:#}").contains("already applied"));
}

#[tokio::test]
async fn conflicts_write_nothing_and_discard_removes_the_worktree() {
    let f = fixture().await;
    let project = &f.project;
    let started = run_in_worktree(&f).await;
    let id = started["worktree_task"]["id"].as_str().unwrap().to_owned();
    let worktree = PathBuf::from(started["worktree_task"]["worktree"].as_str().unwrap());
    finished(&f, started["id"].as_str().unwrap()).await;
    // The project changed the same line meanwhile.
    fs::write(project.join("lib.txt"), "value = 9\n").unwrap();
    let refused = call(
        &f.service,
        "POST",
        &format!("/api/worktree-tasks/{id}/apply"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(refused["state"], "done");
    assert_eq!(refused["conflicts"], json!(["lib.txt"]), "{refused:#}");
    assert_eq!(
        fs::read_to_string(project.join("lib.txt")).unwrap(),
        "value = 9\n"
    );
    assert!(!project.join("answer.txt").exists());
    assert!(worktree.is_dir());

    let discarded = call(
        &f.service,
        "POST",
        &format!("/api/worktree-tasks/{id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(discarded["state"], "discarded");
    assert!(!worktree.exists());
    assert_eq!(managed_branches(project), "");
    assert_eq!(
        fs::read_to_string(project.join("lib.txt")).unwrap(),
        "value = 9\n"
    );
    let listed = call(&f.service, "GET", "/api/worktree-tasks", Value::Null)
        .await
        .unwrap();
    assert_eq!(listed["tasks"][0]["state"], "discarded");
    // The conversation is an ordinary one of the project now.
    let sid = started["session_id"].as_str().unwrap();
    call(
        &f.service,
        "DELETE",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn keep_as_branch_commits_the_result_and_removes_the_checkout() {
    let f = fixture().await;
    let project = &f.project;
    let started = run_in_worktree(&f).await;
    let id = started["worktree_task"]["id"].as_str().unwrap().to_owned();
    let worktree = PathBuf::from(started["worktree_task"]["worktree"].as_str().unwrap());
    finished(&f, started["id"].as_str().unwrap()).await;
    let kept = call(
        &f.service,
        "POST",
        &format!("/api/worktree-tasks/{id}/keep-branch"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(kept["state"], "branch");
    let branch = kept["kept_branch"].as_str().unwrap();
    assert!(branch.starts_with("shadowcode/"));
    assert!(!worktree.exists());
    assert_eq!(managed_branches(project), branch);
    assert_eq!(
        git(project, &["show", &format!("{branch}:lib.txt")]),
        "value = 2 (alpha)"
    );
    assert_eq!(
        git(project, &["log", "-1", "--format=%s", branch]),
        "ShadowCode: Set the answer"
    );
    // The project itself is untouched.
    assert_eq!(
        fs::read_to_string(project.join("lib.txt")).unwrap(),
        "value = 1\n"
    );
}

#[tokio::test]
async fn a_new_worktree_gets_its_files_its_setup_its_own_port_and_a_teardown() {
    let f = fixture().await;
    fs::write(f.project.join(".env"), "TOKEN=local-only\n").unwrap();
    let suggested = call(&f.service, "GET", "/api/worktree-tasks/setup", Value::Null)
        .await
        .unwrap();
    assert_eq!(suggested["suggested"]["copy"], json!([".env"]));
    let marker = f.project.parent().unwrap().join("teardown-ran");
    call(
        &f.service,
        "POST",
        "/api/worktree-tasks/setup",
        json!({"setup":{
            "copy":[".env"],
            "setup":["printf \"$PORT\" > port.txt"],
            "teardown":[format!("touch {}", marker.display())],
            "port_start":41000,
            "port_end":41999
        }}),
    )
    .await
    .unwrap();
    let started = run_in_worktree(&f).await;
    let record = &started["worktree_task"];
    let port = record["port"].as_u64().expect("a port");
    assert!((41000..=41999).contains(&port), "{record}");
    // The open conversation now runs in the worktree; the setup shown and
    // saved there is still the project's, which new tasks start from.
    let open = call(&f.service, "GET", "/api/worktree-tasks/setup", Value::Null)
        .await
        .unwrap();
    assert_eq!(open["workspace"], json!(f.project), "{open}");
    assert_eq!(open["setup"]["port_start"], 41000);
    assert_eq!(open["suggested"]["copy"], json!([".env"]));
    let saved = call(
        &f.service,
        "POST",
        "/api/worktree-tasks/setup",
        json!({"setup": open["setup"]}),
    )
    .await
    .unwrap();
    assert_eq!(saved["workspace"], json!(f.project));
    assert_eq!(record["setup"]["ok"], true, "{record}");
    let worktree = PathBuf::from(record["worktree"].as_str().unwrap());
    assert_eq!(
        fs::read_to_string(worktree.join(".env")).unwrap(),
        "TOKEN=local-only\n"
    );
    assert_eq!(
        fs::read_to_string(worktree.join("port.txt")).unwrap(),
        port.to_string()
    );
    finished(&f, started["id"].as_str().unwrap()).await;
    // A second task gets another port.
    let second = run_in_worktree(&f).await;
    assert_ne!(second["worktree_task"]["port"].as_u64(), Some(port));
    finished(&f, second["id"].as_str().unwrap()).await;
    // Discarding runs the teardown in the worktree first.
    let id = record["id"].as_str().unwrap();
    call(
        &f.service,
        "POST",
        &format!("/api/worktree-tasks/{id}/discard"),
        json!({}),
    )
    .await
    .unwrap();
    assert!(marker.exists());
}

#[tokio::test]
async fn a_worktree_task_can_start_from_another_branch() {
    let f = fixture().await;
    git(&f.project, &["checkout", "-qb", "feature"]);
    fs::write(f.project.join("feature.txt"), "feature\n").unwrap();
    git(&f.project, &["add", "."]);
    git(&f.project, &["commit", "-qm", "Feature"]);
    git(&f.project, &["checkout", "-q", "-"]);
    let started = call(
        &f.service,
        "POST",
        "/api/run",
        json!({"workspace":f.project,"task":"Set the answer","model":"m-alpha","worktree":true,"base_branch":"feature"}),
    )
    .await
    .unwrap();
    let worktree = PathBuf::from(started["worktree_task"]["worktree"].as_str().unwrap());
    assert!(worktree.join("feature.txt").exists());
    assert_eq!(
        started["worktree_task"]["base"]["included_uncommitted"],
        false
    );
    finished(&f, started["id"].as_str().unwrap()).await;
    let missing = call(
        &f.service,
        "POST",
        "/api/run",
        json!({"workspace":f.project,"task":"x","model":"m-alpha","worktree":true,"base_branch":"no-such-branch"}),
    )
    .await
    .unwrap_err();
    assert!(format!("{missing:#}").contains("no-such-branch"));
    // Only local branches: never a tag, even one that shares a branch's name.
    let feature = git(&f.project, &["rev-parse", "feature"]);
    git(&f.project, &["tag", "v-feature", &feature]);
    git(&f.project, &["tag", "shared-name", &feature]);
    git(&f.project, &["branch", "shared-name", "HEAD"]);
    // The base branch picker offers the names the project's branch list
    // shows, where that branch is `heads/shared-name`.
    let overview = call(&f.service, "GET", "/api/git", Value::Null)
        .await
        .unwrap();
    let listed = overview["branches"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|branch| branch["name"].as_str())
        .find(|name| name.ends_with("shared-name"))
        .unwrap_or_else(|| panic!("{overview}"))
        .to_owned();
    for (base, refused) in [
        ("v-feature", true),
        ("HEAD~0", true),
        ("tags/shared-name", true),
        ("refs/tags/shared-name", true),
        ("shared-name", false),
        ("refs/heads/shared-name", false),
        (listed.as_str(), false),
    ] {
        let started = call(
            &f.service,
            "POST",
            "/api/run",
            json!({"workspace":f.project,"task":"Set the answer","model":"m-alpha","worktree":true,"base_branch":base}),
        )
        .await;
        if refused {
            let error = format!("{:#}", started.unwrap_err());
            assert!(error.contains("branch"), "{base}: {error}");
            continue;
        }
        let started = started.unwrap();
        let worktree = PathBuf::from(started["worktree_task"]["worktree"].as_str().unwrap());
        assert!(
            !worktree.join("feature.txt").exists(),
            "{base} started from the tag"
        );
        finished(&f, started["id"].as_str().unwrap()).await;
    }
}
