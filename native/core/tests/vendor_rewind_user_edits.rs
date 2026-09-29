//! Rewinding a subscription turn must not undo files the user saved in
//! ShadowCode's editor while that turn ran. Changes the agent did not report
//! are flagged in the rewind preview. The vendor is a fake Codex app-server
//! that edits files, then waits while the test saves in the editor.
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
    workspace::hash,
};
use std::{fs, path::Path, time::Duration};
use vendor_support::{cli_agents, eventually, FakeCodex};

fn git(dir: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args([
            "-c",
            "core.hooksPath=/dev/null",
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
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
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

async fn save(
    service: &Service,
    project: &Path,
    path: &str,
    content: &str,
) -> anyhow::Result<Value> {
    let expected = hash(&fs::read(project.join(path)).unwrap());
    call(
        service,
        "PUT",
        &format!("/api/workspace/file?path={path}"),
        json!({"content":content,"expected_hash":expected}),
    )
    .await
}

fn read(project: &Path, path: &str) -> String {
    fs::read_to_string(project.join(path)).unwrap()
}

fn sorted(value: &Value) -> Vec<String> {
    let mut list: Vec<String> = value
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    list.sort();
    list
}

#[tokio::test]
async fn rewind_of_a_vendor_turn_keeps_files_you_saved_during_it() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    for name in ["agent.txt", "mine.txt", "both.txt", "shell.txt"] {
        fs::write(project.join(name), format!("{name} original\n")).unwrap();
    }
    git(&project, &["init", "-q"]);
    git(&project, &["add", "-A"]);
    git(&project, &["commit", "-qm", "base"]);
    let fake = FakeCodex::new(
        root.path(),
        json!({"auth":"chatgpt","turn":"edit_wait",
            // shell.txt stands for a file the agent changed with a command
            // (or another program did): it is not reported as an edit.
            "edits":{"agent.txt":"agent edit\n","both.txt":"agent first\n","shell.txt":"shell edit\n"},
            "reported":["agent.txt","both.txt"],
            // The agent edits both.txt again after the user's save.
            "late_edits":{"both.txt":"agent after you\n"}}),
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
        json!({"task":"edit the files","model":"cli:codex"}),
    )
    .await
    .unwrap();
    let id = job["id"].as_str().unwrap().to_owned();
    let task = job["task_id"].as_str().unwrap().to_owned();
    eventually(|| fake.marker("turn_editing"), "the vendor's edits").await;

    // While the turn runs, the user saves two files in the editor.
    let saved = save(&service, &project, "mine.txt", "my edit\n")
        .await
        .expect("an editor save is allowed while a subscription turn runs");
    assert_eq!(saved["during_turn"], true, "{saved}");
    save(&service, &project, "both.txt", "my edit of both\n")
        .await
        .unwrap();
    // Other manual changes still wait for the turn.
    let refused = call(
        &service,
        "POST",
        "/api/workspace/git/add",
        json!({"paths":["mine.txt"]}),
    )
    .await
    .expect_err("Git changes still wait for the turn");
    assert!(
        format!("{refused:#}").contains("Stop the running task"),
        "{refused:#}"
    );

    fs::write(fake.dir.join("release_turn"), "").unwrap();
    let done = tokio::time::timeout(Duration::from_secs(40), service.engine.wait(&id))
        .await
        .expect("turn finished")
        .unwrap();
    assert_eq!(done.status, "completed", "{done:?}");
    assert_eq!(read(&project, "mine.txt"), "my edit\n");
    assert_eq!(read(&project, "both.txt"), "agent after you\n");
    let events = service
        .engine
        .store()
        .events_after(&done.session_id, 0, None, 10_000)
        .unwrap();
    assert!(
        events.iter().any(|e| e["type"] == "agent.warning"
            && e["payload"]["text"]
                == "You saved mine.txt in the editor during this turn. Rewind keeps your version."),
        "the conversation says the user's save is kept"
    );

    // The rewind preview: what changes, what is kept and why, what the
    // agent did not report.
    let preview = call(
        &service,
        "GET",
        &format!("/api/checkpoints/tasks/{task}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(preview["rewindable"], true, "{preview}");
    let checkpoint = &preview["checkpoint"];
    assert_eq!(
        sorted(&checkpoint["rewind_paths"]),
        ["agent.txt", "shell.txt"],
        "{preview}"
    );
    assert_eq!(
        checkpoint["kept"],
        json!([
            {"path":"both.txt","reason":"edited_by_you_and_agent"},
            {"path":"mine.txt","reason":"saved_by_you"}
        ]),
        "{preview}"
    );
    assert_eq!(checkpoint["unreported"], json!(["shell.txt"]), "{preview}");

    // A default rewind restores the agent's files and keeps yours.
    let rewound = call(
        &service,
        "POST",
        &format!("/api/checkpoints/tasks/{task}/restore"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        sorted(&rewound["restored"]),
        ["agent.txt", "shell.txt"],
        "{rewound}"
    );
    assert_eq!(read(&project, "agent.txt"), "agent.txt original\n");
    assert_eq!(read(&project, "shell.txt"), "shell.txt original\n");
    assert_eq!(read(&project, "mine.txt"), "my edit\n", "your save is kept");
    assert_eq!(
        read(&project, "both.txt"),
        "agent after you\n",
        "kept by default"
    );

    // Undo puts back only what the rewind changed.
    let undo = rewound["undo_id"].as_str().unwrap();
    call(
        &service,
        "POST",
        &format!("/api/checkpoints/rewinds/{undo}/undo"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(read(&project, "agent.txt"), "agent edit\n");
    assert_eq!(read(&project, "mine.txt"), "my edit\n");
    call(
        &service,
        "POST",
        &format!("/api/checkpoints/tasks/{task}/restore"),
        json!({}),
    )
    .await
    .unwrap();

    // The file both of you edited can still be rewound on request; the file
    // only you changed never is.
    let preview = call(
        &service,
        "GET",
        &format!("/api/checkpoints/tasks/{task}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(preview["rewindable"], true, "{preview}");
    assert_eq!(
        preview["checkpoint"]["rewind_paths"],
        json!([]),
        "{preview}"
    );
    let all = call(
        &service,
        "POST",
        &format!("/api/checkpoints/tasks/{task}/restore"),
        json!({"include_user_edits":true}),
    )
    .await
    .unwrap();
    assert_eq!(all["restored"], json!(["both.txt"]), "{all}");
    assert_eq!(read(&project, "both.txt"), "both.txt original\n");
    assert_eq!(read(&project, "mine.txt"), "my edit\n");
    let preview = call(
        &service,
        "GET",
        &format!("/api/checkpoints/tasks/{task}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(preview["rewindable"], false, "{preview}");
    assert_eq!(
        preview["checkpoint"]["kept"],
        json!([{"path":"mine.txt","reason":"saved_by_you"}]),
        "{preview}"
    );
}

#[tokio::test]
async fn editor_saves_during_a_turn_are_refused_without_a_checkpoint_and_for_native_tasks() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("mine.txt"), "mine original\n").unwrap();
    let fake = FakeCodex::new(
        root.path(),
        json!({"auth":"chatgpt","turn":"edit_wait","edits":{"agent.txt":"agent\n"}}),
    );
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    // Vendor checkpoints off: Rewind does not cover the turn, so there is
    // nothing to keep the save apart from; the editor waits as before.
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"fixture","context_limit":16384},
            "trusted_workspaces":[project],
            "cli_agents": cli_agents(&fake),
            "checkpoints":{"vendor":false},
        }),
    )
    .unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"edit","model":"cli:codex"}),
    )
    .await
    .unwrap();
    eventually(|| fake.marker("turn_editing"), "the vendor's edits").await;
    let refused = save(&service, &project, "mine.txt", "my edit\n")
        .await
        .expect_err("no checkpoint window");
    assert!(
        format!("{refused:#}").contains("Stop the running task"),
        "{refused:#}"
    );
    fs::write(fake.dir.join("release_turn"), "").unwrap();
    let id = job["id"].as_str().unwrap();
    let done = tokio::time::timeout(Duration::from_secs(40), service.engine.wait(id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "completed");
    assert_eq!(read(&project, "mine.txt"), "mine original\n");
    // Once the turn ended, saving works again.
    save(&service, &project, "mine.txt", "my edit\n")
        .await
        .unwrap();
    assert_eq!(read(&project, "mine.txt"), "my edit\n");
}
