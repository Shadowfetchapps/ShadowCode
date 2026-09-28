mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::StartRequest,
    paths::AppPaths,
    service::{Request, Service},
    store::Store,
};
use std::{fs, path::Path, process::Command, time::Duration};

fn setup(trusted: bool) -> (tempfile::TempDir, Service) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"fixture","context_limit":16384},"trusted_workspaces":if trusted {vec![project.clone()]} else {vec![]}})).unwrap();
    let service = Service::open(paths, Some(project)).unwrap();
    (root, service)
}
async fn call(service: &Service, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}
fn git(project: &Path, args: &[&str]) -> String {
    let result = Command::new("git")
        .args([
            "-c",
            "user.name=Service Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgSign=false",
            "-c",
            "core.hooksPath=/dev/null",
        ])
        .args(args)
        .current_dir(project)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).unwrap()
}
fn init(project: &Path) {
    git(project, &["init", "-q"]);
    git(project, &["config", "user.name", "Service Test"]);
    git(project, &["config", "user.email", "test@example.invalid"]);
    fs::write(
        project.join("sample.txt"),
        (1..=30).map(|v| format!("line {v}\n")).collect::<String>(),
    )
    .unwrap();
    git(project, &["add", "."]);
    git(project, &["commit", "-qm", "Fixture"]);
}

#[test]
fn existing_profile_migrates_to_private_editor_draft_storage() {
    let root = tempfile::tempdir().unwrap();
    let database = root.path().join("state.sqlite");
    drop(Store::open(&database).unwrap());
    let db = rusqlite::Connection::open(&database).unwrap();
    db.execute_batch("DROP TABLE editor_drafts; PRAGMA user_version=26;")
        .unwrap();
    drop(db);
    let reopened = Store::open(&database).unwrap();
    let workspace = root.path().join("project");
    assert!(reopened.editor_drafts(&workspace).unwrap().is_empty());
    let version: i64 = rusqlite::Connection::open(&database)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, shadowcode_core::store::SCHEMA_VERSION);
}

#[tokio::test]
async fn editor_drafts_survive_restart_without_overwriting_workspace_files() {
    let (root, service) = setup(true);
    let project = service.workspace().unwrap();
    let original = "let answer = 1;\n";
    fs::write(project.join("editor.rs"), original).unwrap();
    let opened = call(
        &service,
        "GET",
        "/api/workspace/file?path=editor.rs&full=true",
        Value::Null,
    )
    .await
    .unwrap();
    let hash = opened["hash"].as_str().unwrap();
    let body = json!({"workspace":project,"base":original,"draft":"let answer = 2;\n","base_hash":hash,"expected_revision":"missing"});
    let first = call(
        &service,
        "PUT",
        "/api/workspace/editor-draft?path=editor.rs",
        body.clone(),
    )
    .await
    .unwrap();
    assert_eq!(first["draft"], "let answer = 2;\n");
    assert_eq!(
        fs::read_to_string(project.join("editor.rs")).unwrap(),
        original
    );
    assert!(
        call(
            &service,
            "PUT",
            "/api/workspace/editor-draft?path=editor.rs",
            body
        )
        .await
        .is_err(),
        "a second window cannot overwrite the saved draft with a stale revision"
    );
    assert!(call(
        &service,
        "PUT",
        "/api/workspace/editor-draft?path=editor.rs",
        json!({"workspace":root.path().join("wrong"),"base":original,"draft":"other","base_hash":hash,"expected_revision":first["revision"]})
    )
    .await
    .is_err(), "a queued draft write cannot follow a project selection change");
    drop(service);
    let reopened = Service::open(
        AppPaths::isolated(&root.path().join("profile")).unwrap(),
        Some(project.clone()),
    )
    .unwrap();
    let listed = call(
        &reopened,
        "GET",
        &format!(
            "/api/workspace/editor-drafts?workspace={}",
            project.display()
        ),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(listed["drafts"].as_array().unwrap().len(), 1);
    assert_eq!(listed["drafts"][0]["draft"], "let answer = 2;\n");
    let other = root.path().join("other");
    fs::create_dir(&other).unwrap();
    let second_project = reopened.fork_selection(other.clone(), None).unwrap();
    assert_eq!(
        call(
            &second_project,
            "GET",
            &format!("/api/workspace/editor-drafts?workspace={}", other.display()),
            Value::Null
        )
        .await
        .unwrap()["drafts"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert!(call(
        &reopened,
        "PUT",
        "/api/workspace/editor-draft?path=../outside",
        json!({"workspace":project,"base":original,"draft":"changed","base_hash":hash,"expected_revision":"missing"})
    )
    .await
    .is_err());
    let revision = first["revision"].as_str().unwrap();
    assert!(call(
        &reopened,
        "DELETE",
        "/api/workspace/editor-draft?path=editor.rs",
        json!({"workspace":project,"expected_revision":"missing"})
    )
    .await
    .is_err());
    call(
        &reopened,
        "DELETE",
        "/api/workspace/editor-draft?path=editor.rs",
        json!({"workspace":project,"expected_revision":revision}),
    )
    .await
    .unwrap();
    assert!(call(
        &reopened,
        "GET",
        &format!(
            "/api/workspace/editor-drafts?workspace={}",
            project.display()
        ),
        Value::Null
    )
    .await
    .unwrap()["drafts"]
        .as_array()
        .unwrap()
        .is_empty());
    assert_eq!(
        fs::read_to_string(project.join("editor.rs")).unwrap(),
        original
    );
}

#[tokio::test]
async fn workspace_mutations_respect_cross_process_compare_lock() {
    use fs2::FileExt;
    use std::fs::OpenOptions;

    let (_root, service) = setup(true);
    let project = service.workspace().unwrap();
    init(&project);
    let source = fs::read_to_string(project.join("sample.txt")).unwrap();
    let base_hash = shadowcode_core::workspace::hash(source.as_bytes());
    let lock_file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(project.join(".git/shadowcode-compare.lock"))
        .unwrap();
    lock_file.try_lock_exclusive().unwrap();

    let draft = json!({"workspace":project,"base":source,"draft":"unsaved edit\n","base_hash":base_hash,"expected_revision":"missing"});
    let blocked = call(
        &service,
        "PUT",
        "/api/workspace/editor-draft?path=sample.txt",
        draft.clone(),
    )
    .await
    .unwrap_err();
    assert!(blocked.to_string().contains("Another ShadowCode operation"));
    let blocked_file = call(
        &service,
        "PUT",
        "/api/workspace/file?path=sample.txt",
        json!({"content":"must not race Compare\n","expected_hash":base_hash}),
    )
    .await
    .unwrap_err();
    assert!(blocked_file
        .to_string()
        .contains("Another ShadowCode operation"));
    let blocked_stage = call(
        &service,
        "POST",
        "/api/workspace/git/add",
        json!({"paths":["sample.txt"]}),
    )
    .await
    .unwrap_err();
    assert!(blocked_stage
        .to_string()
        .contains("Another ShadowCode operation"));
    assert_eq!(
        fs::read_to_string(project.join("sample.txt")).unwrap(),
        source
    );
    drop(lock_file);

    let saved_file = call(
        &service,
        "PUT",
        "/api/workspace/file?path=sample.txt",
        json!({"content":"file save after Compare\n","expected_hash":base_hash}),
    )
    .await
    .unwrap();
    assert!(saved_file["hash"].as_str().is_some());

    let saved = call(
        &service,
        "PUT",
        "/api/workspace/editor-draft?path=sample.txt",
        draft,
    )
    .await
    .unwrap();
    assert_eq!(saved["draft"], "unsaved edit\n");
}

#[tokio::test]
async fn manual_mutations_require_trust_and_respect_read_only_mode() {
    let (_root, service) = setup(false);
    let workspace = service.workspace().unwrap();
    let operations = [
        (
            "PUT",
            "/api/workspace/instructions",
            json!({"content":"Instructions"}),
        ),
        (
            "PUT",
            "/api/workspace/skills",
            json!({"name":"build","content":"Build it"}),
        ),
        (
            "POST",
            "/api/workspace/attach",
            json!({"filename":"note.txt","text":"Attached"}),
        ),
        (
            "POST",
            "/api/workspace/exec",
            json!({"command":"touch unexpected"}),
        ),
        ("POST", "/api/workspace/git/add", json!({"paths":["."]})),
    ];
    for (method, path, body) in &operations {
        assert!(call(&service, method, path, body.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("Trust"));
    }
    let opened = call(&service, "POST", "/api/projects", json!({"path":workspace}))
        .await
        .unwrap();
    assert_eq!(opened["needs_trust"], true);
    call(
        &service,
        "POST",
        "/api/projects/trust",
        json!({"path":workspace}),
    )
    .await
    .unwrap();
    call(
        &service,
        "PUT",
        "/api/workspace/instructions",
        json!({"content":"Instructions"}),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join(".shadow/instructions.md")).unwrap(),
        "Instructions"
    );
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"level":"read_only"}}),
    )
    .unwrap();
    for (method, path, body) in operations
        .iter()
        .filter(|(_, path, _)| *path != "/api/workspace/attach")
    {
        assert!(call(&service, method, path, body.clone())
            .await
            .unwrap_err()
            .to_string()
            .contains("read-only"));
    }
    assert!(!workspace.join("unexpected").exists());
    // Attaching is user input, not a project change: read-only projects accept
    // it, still under .shadow/attachments with the usual limits.
    let attached = call(
        &service,
        "POST",
        "/api/workspace/attach",
        json!({"filename":"note.txt","text":"Attached"}),
    )
    .await
    .unwrap();
    let stored = attached["path"].as_str().unwrap();
    assert!(stored.starts_with(".shadow/attachments/") && stored.ends_with("-note.txt"));
    assert_eq!(
        fs::read_to_string(workspace.join(stored)).unwrap(),
        "Attached"
    );
    let png = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .encode([0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0])
    };
    let image = call(
        &service,
        "POST",
        "/api/workspace/attach-image",
        json!({"filename":"shot.png","data_base64":png}),
    )
    .await
    .unwrap();
    assert!(image["path"]
        .as_str()
        .unwrap()
        .starts_with(".shadow/attachments/"));
    assert!(call(
        &service,
        "POST",
        "/api/workspace/attach-image",
        json!({"filename":"shot.png","data_base64":"bm90IGFuIGltYWdl"}),
    )
    .await
    .is_err());
    let file = call(
        &service,
        "GET",
        "/api/workspace/file?path=.shadow%2Finstructions.md",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(file["content"], "Instructions");
    assert!(call(
        &service,
        "GET",
        "/api/workspace/file?path=..%2Fprofile%2Fconfig.yaml",
        Value::Null
    )
    .await
    .is_err());
}

#[tokio::test]
async fn editor_save_preserves_external_changes_and_requires_project_write_access() {
    let (_root, service) = setup(false);
    let workspace = service.workspace().unwrap();
    init(&workspace);
    fs::write(workspace.join("staged.txt"), "keep staged\n").unwrap();
    git(&workspace, &["add", "staged.txt"]);
    let index_before = fs::read(workspace.join(".git/index")).unwrap();
    fs::write(workspace.join("source.rs"), "fn old() {}\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            workspace.join("source.rs"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
    }
    let opened = call(
        &service,
        "GET",
        "/api/workspace/file?path=source.rs&full=true",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(opened["content"], "fn old() {}\n");
    assert_eq!(opened["truncated"], false);
    let head = call(
        &service,
        "GET",
        "/api/workspace/file?path=source.rs&head=true",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(head["hash"], opened["hash"]);
    assert!(head.get("content").is_none());
    let original_hash = opened["hash"].as_str().unwrap();
    let save = || {
        call(
            &service,
            "PUT",
            "/api/workspace/file?path=source.rs",
            json!({"content":"fn draft() {}\n","expected_hash":original_hash}),
        )
    };
    assert!(save().await.unwrap_err().to_string().contains("Trust"));
    call(
        &service,
        "POST",
        "/api/projects/trust",
        json!({"path":workspace}),
    )
    .await
    .unwrap();
    fs::write(workspace.join("source.rs"), "fn agent() {}\n").unwrap();
    assert!(save()
        .await
        .unwrap_err()
        .to_string()
        .contains("File changed"));
    assert_eq!(
        fs::read_to_string(workspace.join("source.rs")).unwrap(),
        "fn agent() {}\n"
    );
    let current = call(
        &service,
        "GET",
        "/api/workspace/file?path=source.rs&full=true",
        Value::Null,
    )
    .await
    .unwrap();
    let result = call(
        &service,
        "PUT",
        "/api/workspace/file?path=source.rs",
        json!({"content":"fn merged() {}\n","expected_hash":current["hash"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("source.rs")).unwrap(),
        "fn merged() {}\n"
    );
    assert_ne!(result["hash"], current["hash"]);
    assert_eq!(
        fs::read(workspace.join(".git/index")).unwrap(),
        index_before
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(workspace.join("source.rs"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
    }
    assert!(call(
        &service,
        "PUT",
        "/api/workspace/file?path=source.rs",
        json!({"content":"invalid revision","expected_hash":""}),
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("revision"));
    fs::remove_file(workspace.join("source.rs")).unwrap();
    assert!(call(
        &service,
        "GET",
        "/api/workspace/file?path=source.rs&head=true",
        Value::Null,
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("File not found"));
    assert!(call(
        &service,
        "PUT",
        "/api/workspace/file?path=source.rs",
        json!({"content":"stale","expected_hash":result["hash"]}),
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("File changed"));
    let recreated = call(
        &service,
        "PUT",
        "/api/workspace/file?path=source.rs",
        json!({"content":"fn recreated() {}\n","expected_hash":"missing"}),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("source.rs")).unwrap(),
        "fn recreated() {}\n"
    );
    #[cfg(unix)]
    {
        let outside = workspace.parent().unwrap().join("outside.txt");
        fs::write(&outside, "Outside\n").unwrap();
        std::os::unix::fs::symlink(&outside, workspace.join("linked.txt")).unwrap();
        assert!(call(
            &service,
            "GET",
            "/api/workspace/file?path=linked.txt&head=true",
            Value::Null,
        )
        .await
        .is_err());
        assert!(call(
            &service,
            "GET",
            "/api/workspace/file?path=linked.txt&full=true",
            Value::Null,
        )
        .await
        .is_err());
    }
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"level":"read_only"}}),
    )
    .unwrap();
    assert!(call(
        &service,
        "PUT",
        "/api/workspace/file?path=source.rs",
        json!({"content":"forbidden","expected_hash":recreated["hash"]}),
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("read-only"));
}

#[tokio::test]
async fn job_gate_blocks_untrusted_projects_until_trust_reloads() {
    let (_root, service) = setup(false);
    let workspace = service.workspace().unwrap();
    let health = call(&service, "GET", "/api/health", Value::Null)
        .await
        .unwrap();
    assert_eq!(health["trusted"], false);
    let error = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Explain the project","workspace":workspace}),
    )
    .await
    .unwrap_err()
    .to_string();
    assert!(
        error.contains("Trust this project before starting an agent task"),
        "{error}"
    );
    let opened = call(&service, "POST", "/api/projects", json!({"path":workspace}))
        .await
        .unwrap();
    assert_eq!(opened["needs_trust"], true);
    call(
        &service,
        "POST",
        "/api/projects/trust",
        json!({"path":format!("{}/", workspace.display())}),
    )
    .await
    .unwrap();
    let reloaded = Config::load(service.engine.paths(), Some(&workspace)).unwrap();
    assert!(
        reloaded.is_trusted(&workspace),
        "trust must persist across a config reload"
    );
    let status = call(&service, "GET", "/api/workspace/status", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["trusted"], true);
    let started = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Explain the project","workspace":format!("{}/", workspace.display())}),
    )
    .await;
    match started {
        Ok(job) => {
            if let Some(id) = job["id"].as_str() {
                let _ = call(
                    &service,
                    "POST",
                    &format!("/api/jobs/{id}/cancel"),
                    json!({}),
                )
                .await;
            }
        }
        Err(error) => assert!(
            !error
                .to_string()
                .contains("Trust this project before starting an agent task"),
            "{error}"
        ),
    }
}

#[tokio::test]
async fn onboarding_preserves_custom_credential_name_and_selects_a_real_session() {
    let (_root, service) = setup(false);
    let workspace = service.workspace().unwrap();
    let opened = call(
        &service,
        "POST",
        "/api/onboarding",
        json!({"workspace":workspace,"provider":"ollama","model":"local-code","theme":"dark"}),
    )
    .await
    .unwrap();
    let cfg = Config::load(service.engine.paths(), None).unwrap();
    assert!(cfg.is_trusted(&workspace));
    assert_eq!(cfg.model.context_limit, 16384);
    assert_eq!(cfg.model.name, "local-code");
    assert_eq!(cfg.ui["theme"], "dark");
    let sid = opened["session_id"].as_str().unwrap();
    let session = call(
        &service,
        "GET",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(session["workspace"], json!(workspace));
    Config::patch(
        service.engine.paths(),
        json!({"model":{"api_key_env":"CUSTOM_LOCAL_TOKEN"}}),
    )
    .unwrap();
    call(
        &service,
        "POST",
        "/api/models/register",
        json!({"provider":"ollama","name":"another-code-model"}),
    )
    .await
    .unwrap();
    let registered = service
        .engine
        .store()
        .models()
        .unwrap()
        .into_iter()
        .find(|m| m["name"] == "another-code-model")
        .unwrap();
    assert_eq!(registered["metadata"]["api_key_env"], "CUSTOM_LOCAL_TOKEN");
    assert_eq!(
        call(&service, "GET", "/api/workspace/instructions", Value::Null)
            .await
            .unwrap()["content"],
        ""
    );
    assert!(call(
        &service,
        "POST",
        "/api/onboarding",
        json!({"workspace":workspace,"provider":"ollama","model":"invalid","context_limit":1})
    )
    .await
    .is_err());
    assert_eq!(
        Config::load(service.engine.paths(), None)
            .unwrap()
            .model
            .name,
        "local-code"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_manual_terminal_and_audits_its_original_session() {
    let (root, service) = setup(true);
    let workspace = service.workspace().unwrap();
    let session = call(
        &service,
        "POST",
        "/api/sessions",
        json!({"title":"Original"}),
    )
    .await
    .unwrap();
    let sid = session["id"].as_str().unwrap();
    let worker = tokio::spawn({
        let service = service.clone();
        async move {
            call(
                &service,
                "POST",
                "/api/workspace/exec",
                json!({"command":"sleep 30 & echo $! > child.pid; wait","timeout":60}),
            )
            .await
            .unwrap()
        }
    });
    let pid = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            // Shell redirection creates the file before echo writes the PID.
            if let Some(pid) = fs::read_to_string(workspace.join("child.pid"))
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
            {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(call(
        &service,
        "PUT",
        "/api/workspace/instructions",
        json!({"content":"Should not write"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("manual operation"));
    assert!(call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Explain the project"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("manual operation"));
    let other = root.path().join("other");
    fs::create_dir(&other).unwrap();
    call(
        &service,
        "POST",
        "/api/sessions",
        json!({"workspace":other}),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(4), service.engine.shutdown())
        .await
        .unwrap()
        .unwrap();
    let result = worker.await.unwrap();
    assert_eq!(result["cancelled"], true);
    assert_eq!(result["ok"], false);
    let events = service.engine.store().recent_events(sid, 100).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "terminal.completed")
            .count(),
        1
    );
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
            if stat.is_empty()
                || stat
                    .rsplit_once(") ")
                    .is_some_and(|(_, tail)| tail.starts_with(['Z', 'X']))
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(service
        .engine
        .reserve_workspace(&workspace)
        .err()
        .unwrap()
        .to_string()
        .contains("shutting down"));
}

#[tokio::test]
async fn workspace_reservation_excludes_tasks_and_is_released_on_drop() {
    let (_root, service) = setup(true);
    let workspace = service.workspace().unwrap();
    let reservation = service.engine.reserve_workspace(&workspace).unwrap();
    assert!(service.engine.reserve_workspace(&workspace).is_err());
    let start = || StartRequest {
        workspace: workspace.clone(),
        task: "Describe the project".into(),
        session_id: None,
        model: None,
        mode: "code".into(),
        queue: true,
        images: Vec::new(),
        web: false,
    };
    assert!(service
        .engine
        .start(start())
        .await
        .unwrap_err()
        .to_string()
        .contains("manual operation"));
    drop(reservation);
    let job = service.engine.start(start()).await.unwrap();
    assert!(service.engine.reserve_workspace(&workspace).is_err());
    service.engine.cancel(&job.id).await.unwrap();
    drop(service.engine.reserve_workspace(&workspace).unwrap());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn review_stages_one_hunk_rejects_another_and_refuses_stale_hunks() {
    let (_root, service) = setup(true);
    let workspace = service.workspace().unwrap();
    init(&workspace);
    let original = fs::read_to_string(workspace.join("sample.txt")).unwrap();
    fs::write(
        workspace.join("sample.txt"),
        original
            .replace("line 2\n", "first change\n")
            .replace("line 28\n", "last change\n"),
    )
    .unwrap();
    let diff = call(
        &service,
        "GET",
        "/api/workspace/diff?path=sample.txt",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(diff["hunks"].as_array().unwrap().len(), 2);
    assert!(diff["hunks"][0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == &json!({"kind":"add","text":"first change"})));
    let first = json!({"path":"sample.txt","action":"accept","hunk":diff["hunks"][0]});
    call(&service, "POST", "/api/workspace/diff/hunk", first.clone())
        .await
        .unwrap();
    let staged = git(&workspace, &["show", ":sample.txt"]);
    assert!(staged.contains("first change"));
    assert!(!staged.contains("last change"));
    assert!(call(&service, "POST", "/api/workspace/diff/hunk", first)
        .await
        .unwrap_err()
        .to_string()
        .contains("Refresh"));
    let remaining = call(
        &service,
        "GET",
        "/api/workspace/diff?path=sample.txt",
        Value::Null,
    )
    .await
    .unwrap();
    call(
        &service,
        "POST",
        "/api/workspace/diff/hunk",
        json!({"path":"sample.txt","action":"reject","hunk":remaining["hunks"][0]}),
    )
    .await
    .unwrap();
    assert_eq!(
        fs::read_to_string(workspace.join("sample.txt")).unwrap(),
        staged
    );
    call(
        &service,
        "POST",
        "/api/workspace/git/commit",
        json!({"message":"Stage one change"}),
    )
    .await
    .unwrap();
    assert!(git(&workspace, &["status", "--porcelain"]).is_empty());
}

#[tokio::test]
async fn review_supports_new_binary_literal_and_no_final_newline_files() {
    let (_root, service) = setup(true);
    let workspace = service.workspace().unwrap();
    init(&workspace);
    fs::write(workspace.join("new [1].txt"), "new text\n").unwrap();
    fs::write(workspace.join("binary.bin"), b"\0\x01\x02").unwrap();
    let preview = call(
        &service,
        "GET",
        "/api/workspace/diff?path=new%20%5B1%5D.txt",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(preview["untracked"], true);
    assert!(!preview["hunks"].as_array().unwrap().is_empty());
    assert!(call(
        &service,
        "POST",
        "/api/workspace/diff/hunk",
        json!({"path":"new [1].txt","action":"accept","hunk":preview["hunks"][0]})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("whole file"));
    let binary = call(
        &service,
        "GET",
        "/api/workspace/diff?path=binary.bin",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(binary["binary"], true);
    assert_eq!(binary["untracked"], true);
    call(
        &service,
        "POST",
        "/api/workspace/git/add",
        json!({"paths":["new [1].txt","binary.bin"]}),
    )
    .await
    .unwrap();
    let binary = call(
        &service,
        "GET",
        "/api/workspace/diff?path=binary.bin",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(binary["binary"], true);
    assert_eq!(binary["untracked"], false);
    assert_eq!(git(&workspace, &["show", ":new [1].txt"]), "new text\n");
    fs::write(workspace.join("sample.txt"), "no final newline").unwrap();
    let diff = call(
        &service,
        "GET",
        "/api/workspace/diff?path=sample.txt",
        Value::Null,
    )
    .await
    .unwrap();
    assert!(diff["hunks"][0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["kind"] == "meta"));
    call(
        &service,
        "POST",
        "/api/workspace/diff/hunk",
        json!({"path":"sample.txt","action":"accept","hunk":diff["hunks"][0]}),
    )
    .await
    .unwrap();
    assert_eq!(
        git(&workspace, &["show", ":sample.txt"]),
        "no final newline"
    );
}

#[tokio::test]
async fn sessions_replay_beyond_page_limits_export_and_delete_without_stale_selection() {
    let (_root, service) = setup(true);
    let session = call(
        &service,
        "POST",
        "/api/sessions",
        json!({"title":"Long conversation"}),
    )
    .await
    .unwrap();
    let sid = session["id"].as_str().unwrap();
    let store = service.engine.store();
    for index in 0..10005 {
        store
            .add_event(
                "model.delta",
                &json!({"text":format!("Message {index}")}),
                Some(sid),
                None,
            )
            .unwrap();
    }
    let detail = call(
        &service,
        "GET",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(detail["events"].as_array().unwrap().len(), 10000);
    assert_eq!(
        detail["events"].as_array().unwrap().last().unwrap()["id"],
        detail["event_cursor"]
    );
    let exported = call(
        &service,
        "GET",
        &format!("/api/sessions/{sid}/export?format=json"),
        Value::Null,
    )
    .await
    .unwrap();
    let content: Value = serde_json::from_str(exported["content"].as_str().unwrap()).unwrap();
    assert_eq!(content["events"].as_array().unwrap().len(), 10005);
    assert_eq!(content["events"][0]["payload"]["text"], "Message 0");
    let boundary = store.event_cursor(sid).unwrap();
    store
        .add_event(
            "model.delta",
            &json!({"text":"After snapshot"}),
            Some(sid),
            None,
        )
        .unwrap();
    assert_eq!(
        store.recent_events_through(sid, boundary, 1).unwrap()[0]["id"],
        boundary
    );
    let mut cursor = 0;
    let mut count = 0;
    loop {
        let page = call(
            &service,
            "GET",
            &format!("/api/sessions/{sid}/events?after={cursor}&limit=1300"),
            Value::Null,
        )
        .await
        .unwrap();
        let events = page["events"].as_array().unwrap();
        if events.is_empty() {
            break;
        }
        for event in events {
            let next = event["id"].as_i64().unwrap();
            assert!(next > cursor);
            cursor = next;
            count += 1;
        }
    }
    assert_eq!(count, 10006);
    call(
        &service,
        "DELETE",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert!(call(&service, "GET", "/api/events", Value::Null)
        .await
        .unwrap()["events"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn active_jobs_prevent_session_deletion_and_manual_edits_but_allow_replay() {
    let server=support::server(|_,_|(json!({"choices":[{"message":{"role":"assistant","content":"Done"},"finish_reason":"stop"}]}),Duration::from_secs(30))).await;
    let (_root, service) = setup(true);
    Config::patch(
        service.engine.paths(),
        json!({"model":{"endpoint":server.endpoint}}),
    )
    .unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Explain the concept of a project"}),
    )
    .await
    .unwrap();
    let sid = job["session_id"].as_str().unwrap();
    let jid = job["id"].as_str().unwrap();
    assert!(call(
        &service,
        "DELETE",
        &format!("/api/sessions/{sid}"),
        Value::Null
    )
    .await
    .is_err());
    assert!(call(
        &service,
        "PUT",
        "/api/workspace/instructions",
        json!({"content":"Collision"})
    )
    .await
    .is_err());
    let replay = call(
        &service,
        "GET",
        &format!("/api/jobs/{jid}/events"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(replay["events"][0]["type"], "user.message");
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    call(
        &service,
        "POST",
        &format!("/api/jobs/{jid}/cancel"),
        Value::Null,
    )
    .await
    .unwrap();
    call(
        &service,
        "DELETE",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_new_task_selects_its_session_so_approval_buttons_work() {
    let server = support::server(|index, _| {
        let message = if index == 0 {
            json!({"role":"assistant","content":"Checking the terminal","tool_calls":[{"id":"approved-command","type":"function","function":{"name":"exec","arguments":"{\"command\":\"printf approved\"}"}}]})
        } else { json!({"role":"assistant","content":"The approved command completed."}) };
        (json!({"choices":[{"message":message,"finish_reason":if index==0 {"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),Duration::ZERO)
    }).await;
    let (_root, service) = setup(true);
    Config::patch(
        service.engine.paths(),
        json!({"model":{"endpoint":server.endpoint}}),
    )
    .unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Run printf approved in the terminal"}),
    )
    .await
    .unwrap();
    let sid = job["session_id"].as_str().unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let pending = call(&service, "GET", "/api/approvals", Value::Null)
                .await
                .unwrap();
            if let Some(approval) = pending["approvals"].as_array().unwrap().first() {
                break approval.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(approval["session_id"], sid);
    let aid = approval["id"].as_str().unwrap();
    assert!(call(
        &service,
        "POST",
        &format!("/api/approvals/{aid}"),
        json!({"decision":"approve","session_id":"another-session"})
    )
    .await
    .is_err());
    call(
        &service,
        "POST",
        &format!("/api/approvals/{aid}"),
        json!({"decision":"approve"}),
    )
    .await
    .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(4),
        service.engine.wait(job["id"].as_str().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.status, "completed", "{}", result.summary);
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn deleting_a_session_does_not_require_its_project_directory_to_exist() {
    let (_root, service) = setup(true);
    let session = call(
        &service,
        "POST",
        "/api/sessions",
        json!({"title":"Removed project"}),
    )
    .await
    .unwrap();
    fs::remove_dir(service.workspace().unwrap()).unwrap();
    let sid = session["id"].as_str().unwrap();
    call(
        &service,
        "DELETE",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert!(service.engine.store().session(sid).unwrap().is_none());
}

#[tokio::test]
async fn session_listing_and_literal_search_work_after_tasks_are_saved() {
    let (_root, service) = setup(true);
    let special = call(
        &service,
        "POST",
        "/api/sessions",
        json!({"title":"Literal 100%_!\\ title"}),
    )
    .await
    .unwrap();
    let plain = call(
        &service,
        "POST",
        "/api/sessions",
        json!({"title":"Other session"}),
    )
    .await
    .unwrap();
    let all = call(&service, "GET", "/api/sessions", Value::Null)
        .await
        .unwrap();
    assert_eq!(all["sessions"].as_array().unwrap().len(), 2);
    for query in ["100%25", "%25_", "%21", "%5C", "Literal"] {
        let found = call(
            &service,
            "GET",
            &format!("/api/sessions?q={query}"),
            Value::Null,
        )
        .await
        .unwrap();
        assert_eq!(found["sessions"].as_array().unwrap().len(), 1, "{query}");
        assert_eq!(found["sessions"][0]["id"], special["id"]);
    }
    service
        .engine
        .store()
        .create_task(plain["id"].as_str().unwrap(), "Find the archived needle")
        .unwrap();
    let found = call(
        &service,
        "GET",
        "/api/sessions?q=archived%20needle",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(found["sessions"].as_array().unwrap().len(), 1);
    assert_eq!(found["sessions"][0]["id"], plain["id"]);
}

#[tokio::test]
async fn desktop_history_pages_are_bounded_complete_and_preserve_original_exports() {
    let (_root, service) = setup(true);
    let store = service.engine.store();
    let session = store
        .create_session(&service.workspace().unwrap(), "fixture", "Long history")
        .unwrap();
    let sid = session["id"].as_str().unwrap();
    let other = store
        .create_session(&service.workspace().unwrap(), "fixture", "Other history")
        .unwrap();
    let mut expected = vec![];
    for number in 0..300 {
        let id = store.add_event("model.delta", &json!({"text":format!("Saved message {number}: {}", "雪".repeat(7000)),"message_id":format!("message-{number}")}), Some(sid), None).unwrap();
        expected.push(id["id"].as_i64().unwrap());
        store
            .add_event(
                "user.message",
                &json!({"text":"Other session must stay out"}),
                other["id"].as_str(),
                None,
            )
            .unwrap();
    }
    let huge = "oversized-original".repeat(20000);
    expected.push(
        store
            .add_event("model.delta", &json!({"text":huge}), Some(sid), None)
            .unwrap()["id"]
            .as_i64()
            .unwrap(),
    );
    let initial = call(
        &service,
        "POST",
        &format!("/api/sessions/{sid}/activate?view=window"),
        json!({}),
    )
    .await
    .unwrap();
    assert!(initial["tasks"].as_array().unwrap().is_empty());
    assert!(initial.to_string().len() < 2_200_000);
    assert!(initial["history_page"]["has_older"].as_bool().unwrap());
    assert_eq!(
        initial["events"].as_array().unwrap().last().unwrap()["type"],
        "history.omitted"
    );
    let mut seen: Vec<i64> = initial["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|event| event["id"].as_i64().unwrap())
        .collect();
    let mut before = initial["history_page"]["first_cursor"].as_i64().unwrap();
    // A new live event must not change a page's exclusive upper boundary.
    let later = store
        .add_event(
            "user.message",
            &json!({"text":"New live message"}),
            Some(sid),
            None,
        )
        .unwrap();
    loop {
        let page = call(
            &service,
            "GET",
            &format!("/api/sessions/{sid}/events?view=window&before={before}"),
            Value::Null,
        )
        .await
        .unwrap();
        assert!(page.to_string().len() < 2_200_000);
        let rows = page["events"].as_array().unwrap();
        assert!(rows.len() <= 128);
        assert!(rows
            .iter()
            .all(|event| event["session_id"] == sid && event["id"].as_i64().unwrap() < before));
        seen.extend(rows.iter().map(|event| event["id"].as_i64().unwrap()));
        if !page["has_older"].as_bool().unwrap() {
            break;
        }
        let next = page["first_cursor"].as_i64().unwrap();
        assert!(next > 0 && next < before);
        before = next;
    }
    seen.sort_unstable();
    assert_eq!(seen, expected);
    let full = call(
        &service,
        "GET",
        &format!("/api/sessions/{sid}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(full["event_cursor"], later["id"]);
    assert!(full["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["payload"]["text"] == huge));
    let export = call(
        &service,
        "GET",
        &format!("/api/sessions/{sid}/export?format=json"),
        Value::Null,
    )
    .await
    .unwrap();
    assert!(export
        .to_string()
        .contains("oversized-originaloversized-original"));
    assert!(call(
        &service,
        "GET",
        &format!("/api/sessions/{sid}/events?view=window&before=0"),
        Value::Null
    )
    .await
    .is_err());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn historical_verification_rechecks_external_edits_and_restart_without_rewriting_receipts() {
    let (root, service) = setup(true);
    let project = root.path().join("project");
    fs::write(project.join("source.txt"), "original").unwrap();
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"approve_shell":false}}),
    )
    .unwrap();
    let started = call(
        &service,
        "POST",
        "/api/jobs/test",
        json!({"command":"test -f source.txt"}),
    )
    .await
    .unwrap();
    let id = started["id"].as_str().unwrap();
    let approval = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let pending = call(&service, "GET", "/api/approvals", Value::Null)
                .await
                .unwrap();
            if let Some(approval) = pending["approvals"].as_array().unwrap().first() {
                break approval.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    call(
        &service,
        "POST",
        &format!("/api/approvals/{}", approval["id"].as_str().unwrap()),
        json!({"decision":"approve"}),
    )
    .await
    .unwrap();
    let job = tokio::time::timeout(Duration::from_secs(8), service.engine.wait(id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.status, "completed", "{}", job.summary);
    let timing = job.timings.as_ref().unwrap();
    assert!(timing.complete);
    assert_eq!(timing.model_requests, 0);
    assert!(timing.model_requests_seconds.is_none());
    assert!(timing.final_checks_seconds.unwrap() >= timing.check_process_seconds.unwrap());
    assert_eq!(
        job.result.as_ref().unwrap()["verification"]["verified"],
        true
    );
    let route = format!("/api/jobs/{id}/verification");
    let fresh = call(&service, "GET", &route, Value::Null).await.unwrap();
    assert_eq!(fresh["status"], "passed");
    assert!(fresh["assessed_at"].is_number());
    fs::write(project.join("source.txt"), "externally edited").unwrap();
    let stale = call(&service, "GET", &route, Value::Null).await.unwrap();
    assert_eq!(stale["status"], "stale");
    assert_eq!(stale["verified"], false);
    assert_eq!(
        service.engine.job(id).unwrap().unwrap().result.unwrap()["verification"]["verified"],
        true,
        "Historical receipt must not be rewritten"
    );
    let paths = service.engine.paths().clone();
    service.engine.shutdown().await.unwrap();
    drop(service);
    let reopened = Service::open(paths, Some(project.clone())).unwrap();
    assert_eq!(
        call(&reopened, "GET", &route, Value::Null).await.unwrap()["status"],
        "stale"
    );
    fs::remove_file(project.join("source.txt")).unwrap();
    assert_eq!(
        call(&reopened, "GET", &route, Value::Null).await.unwrap()["verified"],
        false
    );
    reopened.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn stored_command_output_is_redacted_in_events_and_exports() {
    let (root, service) = setup(true);
    let workspace = service.workspace().unwrap();
    init(&workspace);
    let session = call(
        &service,
        "POST",
        "/api/sessions",
        json!({"title": "Redact"}),
    )
    .await
    .unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    // A fake GitHub token printed by a terminal command must not reach the
    // stored transcript or the conversation export.
    let token = format!("{}{}", "ghp_", "A".repeat(36));
    call(
        &service,
        "POST",
        "/api/workspace/exec",
        json!({"command": format!("echo {token}"), "session_id": sid}),
    )
    .await
    .unwrap();
    let events = service.engine.store().recent_events(&sid, 100).unwrap();
    let stored = serde_json::to_string(&events).unwrap();
    assert!(!stored.contains(&token), "token leaked into stored events");
    assert!(stored.contains("[redacted secret]"), "{stored}");
    let export = call(
        &service,
        "GET",
        &format!("/api/sessions/{sid}/export?format=markdown"),
        Value::Null,
    )
    .await
    .unwrap_or(Value::Null);
    assert!(
        !serde_json::to_string(&export).unwrap().contains(&token),
        "token leaked into export"
    );
    drop(root);
}
