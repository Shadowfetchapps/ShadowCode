//! Full undo: project checkpoints around native shell commands and vendor CLI
//! turns, restored by the existing rewind.
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    approvals::ApprovalHub,
    checkpoint,
    config::{Config, PermissionMode},
    events::TaskEvents,
    models::ToolCall,
    paths::AppPaths,
    service::{Request, Service},
    store::Store,
    tools::{ToolExecutor, ToolResult},
    workspace::Workspace,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use vendor_support::{cli_agents, FakeCodex};

fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn shell_config() -> Config {
    let mut config = Config::default();
    config.permissions.mode = PermissionMode::AllowEdits;
    config.permissions.approve_shell = false;
    config.permissions.require_approval_for_dangerous = false;
    config
}

struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    store: Arc<Store>,
    task: String,
    tools: ToolExecutor,
}

fn fixture(config: Config, prepare: impl FnOnce(&Path)) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    prepare(&project);
    let store = Arc::new(Store::open(&root.path().join("db")).unwrap());
    let session_id = store.create_session(&project, "mock", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = store.create_task(&session_id, "test").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(100);
    let events = TaskEvents {
        store: store.clone(),
        session_id,
        task_id: task.clone(),
        sender,
    };
    let tools = ToolExecutor::new(
        Arc::new(Workspace::open(&project).unwrap()),
        config,
        ApprovalHub::default(),
        events,
        CancellationToken::new(),
    )
    .unwrap();
    Fixture {
        _root: root,
        project: project.canonicalize().unwrap(),
        store,
        task,
        tools,
    }
}

async fn exec(tools: &ToolExecutor, command: &str) -> ToolResult {
    tools
        .execute(ToolCall {
            id: shadowcode_core::id(),
            name: "exec".into(),
            arguments: json!({"command":command}),
        })
        .await
        .unwrap()
}

fn sorted(value: &Value) -> Vec<String> {
    let mut paths: Vec<String> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p.as_str().unwrap().to_owned())
        .collect();
    paths.sort();
    paths
}

fn repo(project: &Path) {
    git(project, &["init", "-q"]);
    fs::write(project.join(".gitignore"), "*.log\n").unwrap();
    fs::write(project.join("tracked.txt"), "one").unwrap();
    fs::write(project.join("gone.txt"), "bye").unwrap();
    fs::create_dir(project.join("src")).unwrap();
    fs::write(project.join("src/lib.rs"), "fn a() {}\n").unwrap();
    git(project, &["add", "-A"]);
    git(project, &["commit", "-qm", "base"]);
    fs::write(project.join("untracked.txt"), "u").unwrap();
    fs::write(project.join("staged.txt"), "staged").unwrap();
    git(project, &["add", "staged.txt"]);
}

const EDIT: &str = "printf two > tracked.txt && rm gone.txt && printf new > added.txt && printf x > debug.log && printf u2 > untracked.txt && printf 'fn b() {}' > src/lib.rs";

#[tokio::test]
async fn shell_edits_in_a_git_repository_are_checkpointed_and_rewound() {
    let f = fixture(shell_config(), repo);
    let staged_before = git(&f.project, &["diff", "--cached", "--name-only"]);
    let result = exec(&f.tools, EDIT).await;
    assert!(result.success, "{} {}", result.error, result.output);
    let point = &result.output["checkpoint"];
    assert_eq!(point["method"], "git", "{point}");
    assert_eq!(
        sorted(&point["paths"]),
        [
            "added.txt",
            "gone.txt",
            "src/lib.rs",
            "tracked.txt",
            "untracked.txt"
        ]
    );
    let reference = point["ref"].as_str().unwrap();
    assert!(reference.starts_with("refs/shadowcode/checkpoints/"));
    // The checkpoint lives in its own ref; the user's index is untouched.
    let commit = git(&f.project, &["rev-parse", reference]);
    assert_eq!(
        git(&f.project, &["show", &format!("{commit}:untracked.txt")]),
        "u"
    );
    assert_eq!(
        git(&f.project, &["diff", "--cached", "--name-only"]),
        staged_before
    );
    let ws = Workspace::open(&f.project).unwrap();
    let summary = checkpoint::summary(&f.store, &ws, &f.task).unwrap();
    assert_eq!(summary["changes"], 5);

    let restored = checkpoint::restore(&f.store, &ws, &f.task).unwrap();
    assert_eq!(restored.len(), 5);
    let read = |p: &str| fs::read_to_string(f.project.join(p)).ok();
    assert_eq!(read("tracked.txt").as_deref(), Some("one"));
    assert_eq!(read("gone.txt").as_deref(), Some("bye"));
    assert_eq!(read("untracked.txt").as_deref(), Some("u"));
    assert_eq!(read("src/lib.rs").as_deref(), Some("fn a() {}\n"));
    assert_eq!(read("added.txt"), None);
    // Git-ignored files are not covered.
    assert_eq!(read("debug.log").as_deref(), Some("x"));
    assert_eq!(read("staged.txt").as_deref(), Some("staged"));
}

#[tokio::test]
async fn rewind_refuses_after_a_later_change_and_readers_take_no_checkpoint() {
    let f = fixture(shell_config(), repo);
    let reader = exec(&f.tools, "cat tracked.txt").await;
    assert!(reader.success);
    assert_eq!(reader.output["checkpoint"]["method"], "none");
    assert!(git(&f.project, &["for-each-ref", "refs/shadowcode/"]).is_empty());
    // A command that changes nothing leaves no checkpoint ref behind.
    let quiet = exec(&f.tools, "true && true").await;
    assert_eq!(quiet.output["checkpoint"]["method"], "git");
    assert!(git(&f.project, &["for-each-ref", "refs/shadowcode/"]).is_empty());

    let result = exec(&f.tools, "printf two > tracked.txt").await;
    assert!(result.success);
    fs::write(f.project.join("tracked.txt"), "three, by hand").unwrap();
    let ws = Workspace::open(&f.project).unwrap();
    let error = checkpoint::restore(&f.store, &ws, &f.task).unwrap_err();
    assert!(
        error.to_string().contains("changed after this task"),
        "{error}"
    );
    assert_eq!(
        fs::read_to_string(f.project.join("tracked.txt")).unwrap(),
        "three, by hand"
    );
}

#[tokio::test]
async fn failed_commands_are_still_checkpointed() {
    let f = fixture(shell_config(), repo);
    let result = exec(&f.tools, "printf two > tracked.txt; exit 3").await;
    assert!(!result.success);
    assert_eq!(
        sorted(&result.output["checkpoint"]["paths"]),
        ["tracked.txt"]
    );
    let ws = Workspace::open(&f.project).unwrap();
    checkpoint::restore(&f.store, &ws, &f.task).unwrap();
    assert_eq!(
        fs::read_to_string(f.project.join("tracked.txt")).unwrap(),
        "one"
    );
}

#[tokio::test]
async fn old_checkpoint_refs_are_pruned() {
    let mut config = shell_config();
    config.checkpoints.keep = 2;
    let f = fixture(config, repo);
    for n in 0..4 {
        let result = exec(&f.tools, &format!("printf {n} > tracked.txt")).await;
        assert!(result.success, "{}", result.error);
    }
    let refs = git(
        &f.project,
        &["for-each-ref", "--format=%(refname)", "refs/shadowcode/"],
    );
    assert_eq!(refs.lines().count(), 2, "{refs}");
}

#[tokio::test]
async fn folders_without_git_use_a_bounded_copy() {
    let f = fixture(shell_config(), |project| {
        fs::write(project.join("tracked.txt"), "one").unwrap();
        fs::write(project.join("gone.txt"), "bye").unwrap();
        fs::create_dir(project.join("node_modules")).unwrap();
        fs::write(project.join("node_modules/dep.js"), "dep").unwrap();
    });
    let result = exec(
        &f.tools,
        "printf two > tracked.txt && rm gone.txt && printf new > added.txt && printf changed > node_modules/dep.js",
    )
    .await;
    assert!(result.success, "{}", result.error);
    let point = &result.output["checkpoint"];
    assert_eq!(point["method"], "copy", "{point}");
    assert_eq!(
        sorted(&point["paths"]),
        ["added.txt", "gone.txt", "tracked.txt"]
    );
    let ws = Workspace::open(&f.project).unwrap();
    checkpoint::restore(&f.store, &ws, &f.task).unwrap();
    assert_eq!(
        fs::read_to_string(f.project.join("tracked.txt")).unwrap(),
        "one"
    );
    assert_eq!(
        fs::read_to_string(f.project.join("gone.txt")).unwrap(),
        "bye"
    );
    assert!(!f.project.join("added.txt").exists());

    // Over the copy budget: reported, never silently claimed.
    let mut config = shell_config();
    config.checkpoints.max_copy_files = 2;
    let big = fixture(config, |project| {
        for n in 0..5 {
            fs::write(project.join(format!("f{n}.txt")), "x").unwrap();
        }
    });
    let result = exec(&big.tools, "printf y > f0.txt").await;
    assert!(result.success);
    let point = &result.output["checkpoint"];
    assert_eq!(point["method"], "none");
    assert!(point["unavailable"].as_str().unwrap().contains("Git"));
    assert!(point["warning"]
        .as_str()
        .unwrap()
        .contains("Rewind does not cover"));
}

/// A step that stops ignoring files (it rewrites `.gitignore`) must not make
/// rewind delete files that existed, ignored, before the step.
#[tokio::test]
async fn rewind_never_deletes_files_that_were_ignored_before_the_step() {
    let f = fixture(shell_config(), |project| {
        repo(project);
        fs::write(project.join(".gitignore"), "*.log\nbuild/\n").unwrap();
        git(project, &["commit", "-qam", "ignore build"]);
        fs::write(project.join("secret.log"), "keep me").unwrap();
        fs::create_dir(project.join("build")).unwrap();
        fs::write(project.join("build/out.bin"), "artifact").unwrap();
    });
    let result = exec(
        &f.tools,
        "printf 'other\\n' > .gitignore && printf new > fresh.txt",
    )
    .await;
    assert!(result.success, "{} {}", result.error, result.output);
    let point = &result.output["checkpoint"];
    assert_eq!(point["method"], "git", "{point}");
    assert_eq!(sorted(&point["paths"]), [".gitignore", "fresh.txt"]);
    let skipped: Vec<&str> = point["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["path"].as_str().unwrap())
        .collect();
    assert!(skipped.contains(&"secret.log"), "{point}");
    assert!(skipped.contains(&"build/out.bin"), "{point}");
    let ws = Workspace::open(&f.project).unwrap();
    checkpoint::restore(&f.store, &ws, &f.task).unwrap();
    let read = |p: &str| fs::read_to_string(f.project.join(p)).ok();
    assert_eq!(read("secret.log").as_deref(), Some("keep me"));
    assert_eq!(read("build/out.bin").as_deref(), Some("artifact"));
    assert_eq!(read(".gitignore").as_deref(), Some("*.log\nbuild/\n"));
    assert_eq!(read("fresh.txt"), None);

    // The same in a folder without Git (its `.gitignore` still applies).
    let f = fixture(shell_config(), |project| {
        fs::write(project.join(".gitignore"), "*.log\n").unwrap();
        fs::write(project.join("secret.log"), "keep me").unwrap();
    });
    let result = exec(
        &f.tools,
        "printf 'other\\n' > .gitignore && printf new > fresh.txt",
    )
    .await;
    assert!(result.success, "{} {}", result.error, result.output);
    let point = &result.output["checkpoint"];
    assert_eq!(point["method"], "copy", "{point}");
    assert_eq!(sorted(&point["paths"]), [".gitignore", "fresh.txt"]);
    let ws = Workspace::open(&f.project).unwrap();
    checkpoint::restore(&f.store, &ws, &f.task).unwrap();
    let read = |p: &str| fs::read_to_string(f.project.join(p)).ok();
    assert_eq!(read("secret.log").as_deref(), Some("keep me"));
    assert_eq!(read("fresh.txt"), None);
}

/// A permission-only change (`chmod`) is put back by rewind.
#[cfg(unix)]
#[tokio::test]
async fn rewind_restores_permissions_changed_by_a_step() {
    use std::os::unix::fs::PermissionsExt;
    let mode = |path: &Path| fs::metadata(path).unwrap().permissions().mode() & 0o777;
    let f = fixture(shell_config(), |project| {
        repo(project);
        fs::write(project.join("run.sh"), "echo hi\n").unwrap();
        fs::set_permissions(project.join("run.sh"), fs::Permissions::from_mode(0o644)).unwrap();
        fs::write(project.join("tool.sh"), "echo tool\n").unwrap();
        fs::set_permissions(project.join("tool.sh"), fs::Permissions::from_mode(0o755)).unwrap();
        git(project, &["add", "run.sh", "tool.sh"]);
        git(project, &["commit", "-qm", "scripts"]);
    });
    let result = exec(&f.tools, "chmod 755 run.sh && chmod 644 tool.sh").await;
    assert!(result.success, "{} {}", result.error, result.output);
    assert_eq!(
        sorted(&result.output["checkpoint"]["paths"]),
        ["run.sh", "tool.sh"]
    );
    let ws = Workspace::open(&f.project).unwrap();
    checkpoint::restore(&f.store, &ws, &f.task).unwrap();
    assert_eq!(mode(&f.project.join("run.sh")), 0o644);
    assert_eq!(mode(&f.project.join("tool.sh")), 0o755);
}

// ------------------------------------------------------------ vendor ----

async fn call(service: &Service, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}

#[tokio::test]
async fn vendor_cli_edits_are_checkpointed_and_rewound() {
    for with_git in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        fs::create_dir(&project).unwrap();
        fs::write(project.join("existing.txt"), "original").unwrap();
        fs::write(project.join("old.txt"), "old").unwrap();
        if with_git {
            git(&project, &["init", "-q"]);
            git(&project, &["add", "-A"]);
            git(&project, &["commit", "-qm", "base"]);
        }
        let fake = FakeCodex::new(
            root.path(),
            json!({"auth":"chatgpt","turn":"edit",
                "edits":{"existing.txt":"changed by vendor","notes.txt":"new from vendor"},
                "deletes":["old.txt"]}),
        );
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
        let job = call(
            &service,
            "POST",
            "/api/jobs",
            json!({"task":"edit the notes","model":"cli:codex"}),
        )
        .await
        .unwrap();
        let id = job["id"].as_str().unwrap().to_owned();
        let done = tokio::time::timeout(Duration::from_secs(40), service.engine.wait(&id))
            .await
            .expect("job finished")
            .unwrap();
        assert_eq!(json!(done)["status"], "completed", "{}", json!(done));
        assert_eq!(
            fs::read_to_string(project.join("existing.txt")).unwrap(),
            "changed by vendor"
        );
        let sid = json!(done)["session_id"].as_str().unwrap().to_owned();
        let events = service
            .engine
            .store()
            .events_after(&sid, 0, None, 10_000)
            .unwrap();
        let recorded = events
            .iter()
            .find(|e| e["type"] == "checkpoint.updated")
            .unwrap_or_else(|| panic!("no checkpoint event: {events:?}"));
        assert_eq!(recorded["payload"]["source"], "vendor", "{recorded}");
        assert_eq!(
            sorted(&recorded["payload"]["changed"]),
            ["existing.txt", "notes.txt", "old.txt"]
        );
        let rewound = call(
            &service,
            "POST",
            &format!("/api/jobs/{id}/rewind"),
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(
            rewound["restored"].as_array().unwrap().len(),
            3,
            "{rewound}"
        );
        assert_eq!(
            fs::read_to_string(project.join("existing.txt")).unwrap(),
            "original"
        );
        assert_eq!(fs::read_to_string(project.join("old.txt")).unwrap(), "old");
        assert!(!project.join("notes.txt").exists());
    }
}
