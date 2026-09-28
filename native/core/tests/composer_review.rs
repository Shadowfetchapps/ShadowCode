//! Composer, approvals and per-task review through the service: "Allow for
//! this task", deny notes the model reads, approval previews, @-mentions
//! read by a native model, the reasoning-effort mapping for vendor CLIs,
//! per-hunk undo, rewinds that can be undone and forking before a message.
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    cli_agent::{adapter_for, LaunchOptions, Update, Vendor, VendorAnswer},
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{fs, path::Path, time::Duration};

fn setup(endpoint: &str) -> (tempfile::TempDir, Service) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","context_limit":16384},"trusted_workspaces":[project],"agent":{"max_steps":12}})).unwrap();
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
fn reply(text: &str, calls: Value) -> (Value, Duration) {
    let reason = if calls.as_array().is_some_and(|c| !c.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    (
        json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
        Duration::ZERO,
    )
}
fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn last_tool_message(body: &Value) -> String {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or("").to_owned())
        .unwrap_or_default()
}
async fn next_approval(service: &Service) -> Value {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let pending = call(service, "GET", "/api/approvals", Value::Null)
                .await
                .unwrap();
            if let Some(first) = pending["approvals"].as_array().unwrap().first() {
                break first.clone();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("an approval prompt")
}
async fn finish(service: &Service, job: &Value) -> shadowcode_core::engine::Job {
    tokio::time::timeout(
        Duration::from_secs(30),
        service.engine.wait(job["id"].as_str().unwrap()),
    )
    .await
    .unwrap()
    .unwrap()
}
fn events(service: &Service, session: &str) -> Vec<Value> {
    service.engine.store().recent_events(session, 500).unwrap()
}

#[tokio::test]
async fn allow_for_task_skips_matching_prompts_and_a_deny_note_reaches_the_model() {
    let server = support::server(|index, body| match index {
        0 => reply("", json!([tool("c1", "exec", json!({"command":"ls -a"}))])),
        // Same program: allowed by the earlier "Allow for this task".
        1 => reply("", json!([tool("c2", "exec", json!({"command":"ls -l"}))])),
        2 => {
            assert!(last_tool_message(body).contains("\"success\":true"));
            reply(
                "",
                json!([tool(
                    "c3",
                    "write_file",
                    json!({"path":"notes.txt","content":"one\ntwo\n"})
                )]),
            )
        }
        _ => {
            let result = last_tool_message(body);
            assert!(
                result.contains("The user denied this action and said: keep notes out of the repo"),
                "{result}"
            );
            reply("Understood, no notes file.", json!([]))
        }
    })
    .await;
    let (_root, service) = setup(&server.endpoint);
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"mode":"ask","approve_shell":true}}),
    )
    .unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"List files, then write notes"}),
    )
    .await
    .unwrap();
    let session = job["session_id"].as_str().unwrap().to_owned();
    let first = next_approval(&service).await;
    assert_eq!(first["grant"], "`ls` commands");
    assert_eq!(first["preview"]["kind"], "command");
    assert_eq!(first["preview"]["command"], "ls -a");
    assert_eq!(
        first["preview"]["cwd"].as_str().unwrap(),
        service.workspace().unwrap().to_str().unwrap()
    );
    // A scope the prompt does not offer is refused.
    assert!(call(
        &service,
        "POST",
        &format!("/api/approvals/{}", first["id"].as_str().unwrap()),
        json!({"decision":"approve","scope":"forever"}),
    )
    .await
    .is_err());
    call(
        &service,
        "POST",
        &format!("/api/approvals/{}", first["id"].as_str().unwrap()),
        json!({"decision":"approve","scope":"task"}),
    )
    .await
    .unwrap();
    let write = next_approval(&service).await;
    assert_eq!(write["tool"], "write_file", "ls -l was not asked again");
    assert_eq!(write["grant"], "file edits");
    assert_eq!(write["note"], true);
    let file = &write["preview"]["files"][0];
    assert_eq!(file["path"], "notes.txt");
    assert_eq!(file["status"], "added");
    assert_eq!(file["diff"], "@@ -0,0 +1,2 @@\n+one\n+two\n");
    call(
        &service,
        "POST",
        &format!("/api/approvals/{}", write["id"].as_str().unwrap()),
        json!({"decision":"deny","note":"keep notes out of the repo"}),
    )
    .await
    .unwrap();
    let done = finish(&service, &job).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert!(!service.workspace().unwrap().join("notes.txt").exists());
    let log = events(&service, &session);
    let kinds: Vec<&str> = log.iter().filter_map(|e| e["type"].as_str()).collect();
    assert_eq!(
        kinds.iter().filter(|k| **k == "approval.requested").count(),
        2
    );
    assert!(log
        .iter()
        .any(|e| e["type"] == "approval.granted" && e["payload"]["grant"] == "`ls` commands"));
    assert!(log.iter().any(|e| e["type"] == "approval.resolved"
        && e["payload"]["scope"] == "task"
        && e["payload"]["approved"] == true));
    assert!(log.iter().any(|e| e["type"] == "approval.resolved"
        && e["payload"]["note"] == "keep notes out of the repo"));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn mentions_are_searched_and_read_by_a_native_model() {
    let server = support::server(|_, body| {
        let user = body["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rev()
            .find(|m| m["role"] == "user")
            .unwrap()["content"]
            .to_string();
        assert!(user.contains("Explain @src/lib.rs"), "{user}");
        assert!(user.contains("<file path=\\\"src/lib.rs\\\">"), "{user}");
        assert!(user.contains("pub fn answer() -> u32 { 42 }"), "{user}");
        assert!(user.contains("<folder path=\\\"docs\\\">"), "{user}");
        reply("It returns 42.", json!([]))
    })
    .await;
    let (_root, service) = setup(&server.endpoint);
    let project = service.workspace().unwrap();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::create_dir_all(project.join("docs")).unwrap();
    fs::write(
        project.join("src/lib.rs"),
        "pub fn answer() -> u32 { 42 }\n",
    )
    .unwrap();
    fs::write(project.join("docs/guide.md"), "# Guide\n").unwrap();
    let found = call(
        &service,
        "GET",
        "/api/workspace/mentions?q=lib",
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(
        found["items"][0],
        json!({"path":"src/lib.rs","kind":"file"})
    );
    // Effort and mentions are checked before anything starts.
    assert!(call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"x","effort":"maximum"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("reasoning effort"));
    assert!(call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"x","mentions":[{"path":"../secret","kind":"file"}]})
    )
    .await
    .is_err());
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Explain @src/lib.rs","effort":"low","mentions":[{"path":"src/lib.rs","kind":"file"},{"path":"docs","kind":"dir"}]}),
    )
    .await
    .unwrap();
    let done = finish(&service, &job).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    // The conversation shows the prompt, not the attached contents.
    let log = events(&service, job["session_id"].as_str().unwrap());
    assert!(!log
        .iter()
        .any(|e| e.to_string().contains("pub fn answer() -> u32")));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn review_undoes_one_hunk_and_a_rewind_can_be_undone() {
    let original: String = (1..=20).map(|i| format!("line {i}\n")).collect();
    let server = support::server(|index, _| match index {
        0 => reply(
            "",
            json!([tool(
                "e1",
                "edit_file",
                json!({"path":"sample.txt","old_string":"line 2\n","new_string":"line two\n"})
            ),]),
        ),
        1 => reply(
            "",
            json!([tool(
                "e2",
                "edit_file",
                json!({"path":"sample.txt","old_string":"line 18\n","new_string":"line eighteen\n"})
            ),]),
        ),
        2 => reply(
            "",
            json!([tool(
                "w1",
                "write_file",
                json!({"path":"new.txt","content":"hello\n"})
            )]),
        ),
        _ => reply("Edited sample.txt and added new.txt.", json!([])),
    })
    .await;
    let (_root, service) = setup(&server.endpoint);
    Config::patch(
        service.engine.paths(),
        json!({"permissions":{"mode":"allow_edits"}}),
    )
    .unwrap();
    let project = service.workspace().unwrap();
    fs::write(project.join("sample.txt"), &original).unwrap();
    fs::write(project.join("untouched.txt"), "same\n").unwrap();
    let job = call(&service, "POST", "/api/jobs", json!({"task":"Edit files"}))
        .await
        .unwrap();
    let done = finish(&service, &job).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let task = job["task_id"].as_str().unwrap();

    let review = call(
        &service,
        "GET",
        &format!("/api/review/tasks/{task}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(review["busy"], false);
    let files = review["files"].as_array().unwrap();
    assert_eq!(files.len(), 2, "only this task's files: {files:?}");
    assert_eq!(files[0]["path"], "sample.txt");
    assert_eq!(files[0]["source"], "checkpoint");
    assert_eq!(files[1]["status"], "added");
    let detail = call(
        &service,
        "GET",
        &format!("/api/review/tasks/{task}/file?path=sample.txt"),
        Value::Null,
    )
    .await
    .unwrap();
    let hunks = detail["hunks"].as_array().unwrap();
    assert_eq!(hunks.len(), 2);
    let after = call(
        &service,
        "POST",
        &format!("/api/review/tasks/{task}/undo"),
        json!({"path":"sample.txt","hunk":hunks[0]["id"]}),
    )
    .await
    .unwrap();
    assert_eq!(after["hunks"].as_array().unwrap().len(), 1);
    let text = fs::read_to_string(project.join("sample.txt")).unwrap();
    assert!(text.contains("line 2\n") && text.contains("line eighteen\n"));
    // A stale hunk id is refused and nothing changes.
    assert!(call(
        &service,
        "POST",
        &format!("/api/review/tasks/{task}/undo"),
        json!({"path":"sample.txt","hunk":hunks[0]["id"]}),
    )
    .await
    .is_err());
    // Files the task did not change are not part of its review.
    assert!(call(
        &service,
        "POST",
        &format!("/api/review/tasks/{task}/undo"),
        json!({"path":"untouched.txt"}),
    )
    .await
    .is_err());

    // Rewind keeps what the files were, and Undo puts them back.
    let rewound = call(
        &service,
        "POST",
        &format!("/api/checkpoints/tasks/{task}/restore"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(rewound["restored"].as_array().unwrap().len(), 2);
    let undo_id = rewound["undo_id"].as_str().unwrap().to_owned();
    assert_eq!(
        fs::read_to_string(project.join("sample.txt")).unwrap(),
        original
    );
    assert!(!project.join("new.txt").exists());
    let undone = call(
        &service,
        "POST",
        &format!("/api/checkpoints/rewinds/{undo_id}/undo"),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(undone["restored"].as_array().unwrap().len(), 2);
    assert_eq!(
        fs::read_to_string(project.join("sample.txt")).unwrap(),
        text
    );
    assert_eq!(
        fs::read_to_string(project.join("new.txt")).unwrap(),
        "hello\n"
    );
    assert!(call(
        &service,
        "POST",
        &format!("/api/checkpoints/rewinds/{undo_id}/undo"),
        json!({}),
    )
    .await
    .is_err());
    let log = events(&service, job["session_id"].as_str().unwrap());
    assert!(log.iter().any(|e| e["type"] == "review.undone"
        && e["payload"]["path"] == "sample.txt"
        && e["payload"]["whole"] == false));
    assert!(
        log.iter()
            .any(|e| e["type"] == "checkpoint.restored"
                && e["payload"]["undo_id"] == undo_id.as_str())
    );
    assert!(log.iter().any(|e| e["type"] == "checkpoint.rewind_undone"));
    // The task can be rewound again after the undo.
    let checkpoint = call(
        &service,
        "GET",
        &format!("/api/checkpoints/tasks/{task}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(checkpoint["rewindable"], true);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn edit_and_resend_forks_just_before_the_message() {
    let server = support::server(|_, _| reply("Done.", json!([]))).await;
    let (_root, service) = setup(&server.endpoint);
    let first = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"First request"}),
    )
    .await
    .unwrap();
    finish(&service, &first).await;
    let session = first["session_id"].as_str().unwrap().to_owned();
    let second = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Second request","session_id":session}),
    )
    .await
    .unwrap();
    finish(&service, &second).await;
    let log = events(&service, &session);
    let started = |task: &str| {
        log.iter()
            .find(|e| {
                e["type"] == "agent.started" && e["task_id"] == task && e["id"].as_i64().is_some()
            })
            .unwrap()["id"]
            .as_i64()
            .unwrap()
    };
    let cut = started(second["task_id"].as_str().unwrap());
    let fork = call(
        &service,
        "POST",
        &format!("/api/sessions/{session}/fork"),
        json!({"event_id":cut,"before":true}),
    )
    .await
    .unwrap();
    let branch = fork["fork"]["id"].as_str().unwrap();
    let copied = events(&service, branch);
    assert!(copied
        .iter()
        .any(|e| e.to_string().contains("First request")));
    assert!(
        !copied
            .iter()
            .any(|e| e.to_string().contains("Second request")),
        "{:#?}",
        log.iter()
            .map(|e| (e["id"].clone(), e["type"].clone(), e["task_id"].clone()))
            .collect::<Vec<_>>()
    );
    // Before the very first event there is nothing to copy: an empty branch.
    let earliest = log.iter().filter_map(|e| e["id"].as_i64()).min().unwrap();
    let empty = call(
        &service,
        "POST",
        &format!("/api/sessions/{session}/fork"),
        json!({"event_id":earliest,"before":true}),
    )
    .await
    .unwrap();
    assert!(events(&service, empty["fork"]["id"].as_str().unwrap()).is_empty());
    assert_eq!(empty["fork"]["parent_id"], session.as_str());
    service.engine.shutdown().await.unwrap();
}

fn launch(root: &Path, effort: Option<&str>) -> LaunchOptions {
    LaunchOptions {
        binary: "vendor".into(),
        workspace: root.to_path_buf(),
        model: "default".into(),
        read_only: false,
        resume: None,
        effort: effort.map(str::to_owned),
        ..Default::default()
    }
}

#[test]
fn reasoning_effort_reaches_each_vendors_own_control() {
    let root = tempfile::tempdir().unwrap();
    let (_, args) = adapter_for(Vendor::Codex, false).command(&launch(root.path(), Some("high")));
    assert_eq!(
        args,
        ["-c", "model_reasoning_effort=\"high\"", "app-server"]
    );
    let (_, args) = adapter_for(Vendor::Codex, true).command(&launch(root.path(), Some("low")));
    assert_eq!(&args[..3], ["-c", "model_reasoning_effort=\"low\"", "exec"]);
    let (_, args) = adapter_for(Vendor::Codex, false).command(&launch(root.path(), None));
    assert_eq!(args, ["app-server"]);
    // Claude Code takes `--effort`; current models think adaptively and
    // ignore a MAX_THINKING_TOKENS budget, so it is no longer sent.
    let claude = adapter_for(Vendor::Claude, false);
    let (_, args) = claude.command(&launch(root.path(), Some("medium")));
    let at = args.iter().position(|a| a == "--effort").expect("--effort");
    assert_eq!(args[at + 1], "medium");
    assert!(claude.env(&launch(root.path(), Some("medium"))).is_empty());
    let (_, args) = claude.command(&launch(root.path(), None));
    assert!(!args.contains(&"--effort".to_owned()));
    assert!(claude.env(&launch(root.path(), None)).is_empty());
    // A Claude Code without `--effort` keeps the thinking budget.
    let legacy = LaunchOptions {
        legacy_effort: true,
        ..launch(root.path(), Some("medium"))
    };
    assert!(!claude.command(&legacy).1.contains(&"--effort".to_owned()));
    assert_eq!(
        claude.env(&legacy),
        [("MAX_THINKING_TOKENS".to_owned(), "16000".to_owned())]
    );
    let cursor = adapter_for(Vendor::Cursor, false);
    assert!(cursor.env(&launch(root.path(), Some("high"))).is_empty());
    let (_, args) = cursor.command(&launch(root.path(), Some("high")));
    assert!(!args.iter().any(|a| a.contains("effort")));
}

fn rpc(id: u64, method: &str, params: Value) -> String {
    json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}).to_string()
}
fn approval_id(updates: &[Update]) -> String {
    updates
        .iter()
        .find_map(|u| match u {
            Update::Approval(p) => Some(p.request_id.clone()),
            _ => None,
        })
        .expect("approval prompt")
}

#[test]
fn allow_for_task_maps_to_the_vendors_session_choice_and_notes_to_claude() {
    let root = tempfile::tempdir().unwrap();
    let mut codex = adapter_for(Vendor::Codex, false);
    codex.on_start(&launch(root.path(), None));
    codex.prompt("approval mapping", &[]).unwrap();
    // Approval choices belong to this established thread and active turn.
    for (id, result) in [
        (1, json!({"userAgent":"fixture"})),
        (2, json!({"thread":{"id":"thr-1"}})),
        (3, json!({"turn":{"id":"turn-1"}})),
    ] {
        codex
            .on_line(&json!({"jsonrpc":"2.0","id":id,"result":result}).to_string())
            .unwrap();
    }
    let step = codex
        .on_line(&rpc(
            7,
            "item/commandExecution/requestApproval",
            json!({"threadId":"thr-1","turnId":"turn-1","startedAtMs":0,"command":"cargo test","itemId":"c1"}),
        ))
        .unwrap();
    let id = approval_id(&step.updates);
    let reply = codex
        .answer(
            &id,
            &VendorAnswer {
                allow: true,
                for_session: true,
                note: None,
            },
        )
        .unwrap();
    assert!(
        reply[0].contains("\"decision\":\"acceptForSession\""),
        "{reply:?}"
    );
    let step = codex
        .on_line(&rpc(
            8,
            "execCommandApproval",
            json!({"conversationId":"thr-1","callId":"c2","command":["cargo","test"],"cwd":root.path(),"parsedCmd":[]}),
        ))
        .unwrap();
    let id = approval_id(&step.updates);
    let reply = codex
        .answer(
            &id,
            &VendorAnswer {
                allow: true,
                for_session: true,
                note: None,
            },
        )
        .unwrap();
    assert!(reply[0].contains("approved_for_session"), "{reply:?}");
    let step = codex
        .on_line(&rpc(
            9,
            "applyPatchApproval",
            json!({"conversationId":"thr-1","callId":"f1","fileChanges":{"a.txt":{"add":{"content":"x\n"}}}}),
        ))
        .unwrap();
    // The patch travels with the prompt, for the approval card's diff.
    let prompt = step
        .updates
        .iter()
        .find_map(|u| match u {
            Update::Approval(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        prompt.arguments["changes"]["a.txt"]["add"]["content"],
        "x\n"
    );
    assert!(!codex.deny_note(), "Codex decisions carry no reason");

    let mut claude = adapter_for(Vendor::Claude, false);
    let step = claude
        .on_line(r#"{"type":"control_request","request_id":"r1","request":{"subtype":"can_use_tool","tool_name":"Bash","input":{"command":"rm -rf build"}}}"#)
        .unwrap();
    let id = approval_id(&step.updates);
    assert!(claude.deny_note());
    let reply = claude
        .answer(
            &id,
            &VendorAnswer {
                allow: false,
                for_session: false,
                note: Some("use make clean".into()),
            },
        )
        .unwrap();
    assert!(reply[0].contains("\"behavior\":\"deny\""));
    assert!(reply[0].contains("and said: use make clean"), "{reply:?}");
}
