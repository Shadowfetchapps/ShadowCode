use serde_json::{json, Value};
use shadowcode_core::{
    approvals::ApprovalHub,
    checkpoint,
    config::{Config, PermissionLevel},
    events::TaskEvents,
    models::ToolCall,
    patch,
    store::Store,
    tools::ToolExecutor,
    workspace::Workspace,
};
use std::{fs, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn fixture(config: Config) -> (tempfile::TempDir, ToolExecutor) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let store = Arc::new(Store::open(&root.path().join("db")).unwrap());
    let session_id = store.create_session(&project, "mock", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task_id = store.create_task(&session_id, "test").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(100);
    let events = TaskEvents {
        store,
        session_id,
        task_id,
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
    (root, tools)
}
async fn call(tools: &ToolExecutor, name: &str, args: Value) -> shadowcode_core::tools::ToolResult {
    tools
        .execute(ToolCall {
            id: shadowcode_core::id(),
            name: name.into(),
            arguments: args,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn system_info_is_read_only_and_cannot_read_an_arbitrary_path() {
    let mut config = Config::default();
    config.permissions.level = PermissionLevel::ReadOnly;
    let (_root, tools) = fixture(config);
    let result = call(&tools, "system_info", json!({})).await;
    assert!(result.success, "{}", result.error);
    assert_eq!(result.output["read_only"], true);
    assert_eq!(result.output["os"], std::env::consts::OS);
    assert!(result.output["displays"]["connectors"].is_array());
    assert!(shadowcode_core::permissions::parallel_safe(
        "system_info",
        &json!({})
    ));
    assert_eq!(
        shadowcode_core::autonomy::replay_class("system_info"),
        shadowcode_core::autonomy::ReplayClass::SafeToReplay
    );
    let denied = call(&tools, "system_info", json!({"path":"/etc/shadow"})).await;
    assert!(!denied.success);
    assert!(denied.error.contains("takes no arguments"));
}

#[tokio::test]
async fn sqlite_builtins_work_in_read_only_mode_without_external_activation() {
    let mut config = Config::default();
    config.permissions.level = PermissionLevel::ReadOnly;
    let (_root, tools) = fixture(config);
    let db = rusqlite::Connection::open(tools.workspace.path.join("app.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE records(value TEXT); INSERT INTO records VALUES('native sqlite');",
    )
    .unwrap();
    drop(db);
    for name in ["mcp_sqlite_tables", "mcp_sqlite_query"] {
        assert!(tools
            .schemas()
            .iter()
            .any(|tool| tool["function"]["name"] == name));
        assert!(shadowcode_core::permissions::parallel_safe(
            name,
            &json!({})
        ));
    }
    let tables = call(&tools, "mcp_sqlite_tables", json!({"path":"app.db"})).await;
    assert!(tables.success, "{}", tables.error);
    let query = call(&tools,"mcp_sqlite_query",json!({"path":"app.db","sql":"SELECT value FROM records WHERE value=?","params":["native sqlite"]})).await;
    assert!(query.success, "{}", query.error);
    assert!(query.output.to_string().contains("native sqlite"));
    assert!(
        !call(
            &tools,
            "mcp_sqlite_query",
            json!({"path":"app.db","sql":"DELETE FROM records RETURNING value"})
        )
        .await
        .success
    );
    assert!(
        !call(&tools, "mcp_sqlite_query", json!({"path":"app.db"}))
            .await
            .success
    );
}

#[tokio::test]
async fn tool_edits_reject_stale_reads_and_rewind_moves_and_new_files() {
    let (_root, tools) = fixture(Config::default());
    let invalid = call(
        &tools,
        "write_file",
        json!({"path":"new", "content":"must not appear", "expected_hash":""}),
    )
    .await;
    assert!(!invalid.success);
    assert!(invalid.error.contains("'missing' for a new file"));
    assert!(tools.workspace.snapshot("new").unwrap().bytes.is_none());
    tools.workspace.write("file", b"original", None).unwrap();
    assert!(
        !call(
            &tools,
            "write_file",
            json!({"path":"file","content":"blind"})
        )
        .await
        .success
    );
    assert!(
        call(&tools, "read_file", json!({"path":"file"}))
            .await
            .success
    );
    tools
        .workspace
        .write("file", b"user changed it", None)
        .unwrap();
    assert!(
        !call(
            &tools,
            "write_file",
            json!({"path":"file","content":"stale overwrite"})
        )
        .await
        .success
    );
    call(&tools, "read_file", json!({"path":"file"})).await;
    assert!(
        call(
            &tools,
            "edit_file",
            json!({"path":"file","old_string":"user changed it","new_string":"agent edit"})
        )
        .await
        .success
    );
    assert!(
        call(&tools, "move_file", json!({"src":"file","dest":"moved"}))
            .await
            .success
    );
    assert!(
        call(&tools, "write_file", json!({"path":"new","content":"new"}))
            .await
            .success
    );
    checkpoint::restore(&tools.events.store, &tools.workspace, &tools.events.task_id).unwrap();
    assert_eq!(
        tools.workspace.read("file").unwrap().content,
        "user changed it"
    );
    assert!(tools.workspace.snapshot("moved").unwrap().bytes.is_none());
    assert!(tools.workspace.snapshot("new").unwrap().bytes.is_none());
}

#[test]
fn patch_preflight_is_atomic_and_accepts_real_unified_and_codex_formats() {
    let (_root, tools) = fixture(Config::default());
    let ws = &tools.workspace;
    ws.write("a", b"one\ntwo\nthree\n", None).unwrap();
    let patch="*** Begin Patch\n*** Update File: a\n@@\n one\n-two\n+changed\n three\n*** Add File: new file.txt\n+hello\n*** End Patch\n";
    let changes = patch::prepare(ws, patch).unwrap();
    assert_eq!(changes.len(), 2);
    assert_eq!(
        changes[0].after.as_deref(),
        Some(b"one\nchanged\nthree\n".as_slice())
    );
    assert_eq!(ws.read("a").unwrap().content, "one\ntwo\nthree\n");
    assert!(patch::prepare(
        ws,
        &patch.replace("+hello", "*** Update File: missing\n@@\n-old\n+new")
    )
    .is_err());
    let changes = patch::prepare(
        ws,
        "--- a/a\n+++ b/a\n@@ -1,3 +1,3 @@\n one\n-two\n+changed\n three\n",
    )
    .unwrap();
    assert_eq!(
        changes[0].after.as_deref(),
        Some(b"one\nchanged\nthree\n".as_slice())
    );
    assert!(patch::prepare(ws, "--- a/a\n+++ b/a\n@@ -1,3 +1,3 @@\n one\n-two\n").is_err());
}

#[test]
fn patches_preserve_newlines_and_disambiguate_repeated_lines_by_position() {
    let (_root, tools) = fixture(Config::default());
    let ws = &tools.workspace;
    ws.write("a", b"one\r\ntwo", None).unwrap();
    let changes = patch::prepare(
        ws,
        "*** Begin Patch\n*** Update File: a\n@@\n-two\n+three\n*** End of File\n*** End Patch",
    )
    .unwrap();
    assert_eq!(
        changes[0].after.as_deref(),
        Some(b"one\r\nthree".as_slice())
    );
    ws.write("a", b"x\nx\nx\nx\nx\n", None).unwrap();
    let changes = patch::prepare(ws, "--- a/a\n+++ b/a\n@@ -5 +5 @@\n-x\n+y\n").unwrap();
    assert_eq!(
        changes[0].after.as_deref(),
        Some(b"x\nx\nx\nx\ny\n".as_slice())
    );
    assert!(patch::prepare(
        ws,
        "*** Begin Patch\n*** Update File: a\n@@\n-x\n+y\n*** End Patch"
    )
    .is_err());
    ws.write("a", b"old", None).unwrap();
    let changes=patch::prepare(ws,"--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n\\ No newline at end of file\n+new\n\\ No newline at end of file\n").unwrap();
    assert_eq!(changes[0].after.as_deref(), Some(b"new".as_slice()));
}

#[tokio::test]
async fn multi_file_patch_rejects_bad_later_context_without_changing_earlier_file() {
    let (_root, tools) = fixture(Config::default());
    tools.workspace.write("a", b"a\n", None).unwrap();
    tools.workspace.write("b", b"b\n", None).unwrap();
    let result=call(&tools,"apply_patch",json!({"patch":"*** Begin Patch\n*** Update File: a\n@@\n-a\n+changed\n*** Update File: b\n@@\n-wrong\n+changed\n*** End Patch"})).await;
    assert!(!result.success);
    assert_eq!(tools.workspace.read("a").unwrap().content, "a\n");
    assert_eq!(
        checkpoint::summary(&tools.events.store, &tools.workspace, &tools.events.task_id).unwrap()
            ["changes"],
        0
    );
}

#[tokio::test]
async fn shell_requires_scoped_approval_and_records_actual_exit_status() {
    let (_root, tools) = fixture(Config::default());
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        call(
            &worker,
            "exec",
            json!({"command":"printf verified; exit 3"}),
        )
        .await
    });
    let record = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(a) = tools.approvals.list(None).pop() {
                break a;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!task.is_finished());
    assert!(tools
        .approvals
        .decide(&record.id, "wrong-session", true)
        .is_err());
    tools
        .approvals
        .decide(&record.id, &tools.events.session_id, true)
        .unwrap();
    let result = task.await.unwrap();
    assert!(!result.success);
    assert_eq!(result.output["exit_code"], 3);
    assert_eq!(result.output["stdout"], "verified");
    let events = tools
        .events
        .store
        .recent_events(&tools.events.session_id, 20)
        .unwrap();
    assert!(events.iter().any(|e| e["type"] == "approval.resolved"));
    assert!(events
        .iter()
        .any(|e| e["type"] == "tool.completed" && e["payload"]["success"] == false));
}

#[tokio::test]
async fn read_only_never_executes_a_command_and_cancellation_clears_pending_approval() {
    let mut config = Config::default();
    config.permissions.level = PermissionLevel::ReadOnly;
    let (_root, tools) = fixture(config);
    assert!(
        !call(&tools, "exec", json!({"command":"touch should-not-exist"}))
            .await
            .success
    );
    assert!(tools
        .workspace
        .snapshot("should-not-exist")
        .unwrap()
        .bytes
        .is_none());
    assert!(tools.approvals.list(None).is_empty());
    let (_root, tools) = fixture(Config::default());
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        call(&worker, "exec", json!({"command":"touch should-not-exist"})).await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while tools.approvals.list(None).is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tools.cancel.cancel();
    assert!(!task.await.unwrap().success);
    assert!(tools.approvals.list(None).is_empty());
    assert!(tools
        .workspace
        .snapshot("should-not-exist")
        .unwrap()
        .bytes
        .is_none());
}

#[tokio::test]
async fn search_is_cooperatively_cancellable_and_plans_validate_state() {
    let (_root, tools) = fixture(Config::default());
    let token = CancellationToken::new();
    token.cancel();
    assert!(tools
        .workspace
        .search_with_control("x", None, ".", 100, &token)
        .is_err());
    assert!(!call(&tools,"update_plan",json!({"steps":[{"title":"a","status":"in_progress"},{"title":"b","status":"in_progress"}]})).await.success);
    assert!(call(&tools,"update_plan",json!({"goal":"test","steps":[{"title":"a","status":"completed"},{"title":"b","status":"in_progress"}]})).await.success);
    assert_eq!(tools.plan()["steps"][1]["status"], "running");
}

#[tokio::test]
async fn invalid_args_do_not_panic_and_truncation_is_visible() {
    let (_root, tools) = fixture(Config::default());
    tools
        .workspace
        .write("big.rs", "line\n".repeat(500).as_bytes(), None)
        .unwrap();
    let invalid = tools
        .execute(ToolCall {
            id: "bad".into(),
            name: "read_file".into(),
            arguments: json!("not-an-object"),
        })
        .await
        .unwrap();
    assert!(!invalid.success);
    assert!(invalid.error.contains("object"));
    let ranged = call(
        &tools,
        "read_file",
        json!({"path":"big.rs","offset":1,"limit":10}),
    )
    .await;
    assert!(ranged.success);
    assert_eq!(ranged.output["truncated"], true);
    assert_eq!(ranged.output["next_offset"], 11);
    assert!(ranged.output["note"]
        .as_str()
        .unwrap()
        .contains("truncated"));
    let message = ranged.message("read_file", 80);
    let content = message["content"].as_str().unwrap();
    assert!(content.contains("truncated") && content.contains("note"));
    for name in [
        "write_file",
        "edit_file",
        "apply_patch",
        "delete_file",
        "exec",
        "git_commit",
        "git_reset",
        "git_clean",
    ] {
        assert_ne!(
            shadowcode_core::autonomy::replay_class(name),
            shadowcode_core::autonomy::ReplayClass::SafeToReplay
        );
    }
}

#[tokio::test]
async fn denied_approval_is_not_success_and_does_not_run_the_command() {
    let (_root, tools) = fixture(Config::default());
    let worker = tools.clone();
    let task = tokio::spawn(async move {
        call(&worker, "exec", json!({"command": "touch must-not-exist"})).await
    });
    let record = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(approval) = tools.approvals.list(None).pop() {
                break approval;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tools
        .approvals
        .decide(&record.id, &tools.events.session_id, false)
        .unwrap();
    let result = task.await.unwrap();
    assert!(!result.success);
    let status =
        shadowcode_core::autonomy::tool_status(result.success, &result.output, &result.error);
    assert_ne!(status, shadowcode_core::autonomy::ToolStatus::Success);
    assert!(
        matches!(
            status,
            shadowcode_core::autonomy::ToolStatus::Denied
                | shadowcode_core::autonomy::ToolStatus::Cancelled
        ),
        "{status:?} from {}",
        result.error
    );
    assert!(tools
        .workspace
        .snapshot("must-not-exist")
        .unwrap()
        .bytes
        .is_none());
}

#[tokio::test]
async fn secret_env_is_refused_and_tokens_redacted_in_tool_messages() {
    let (_root, tools) = fixture(Config::default());
    let openai = format!("{}{}", "sk-test", "abcdefghijklmnopqrstuvwxyz0123");
    let aws = format!("{}{}", "AKIA", "IOSFODNN7EXAMPLE");
    fs::write(
        tools.workspace.path.join(".env"),
        format!("OPENAI_API_KEY={openai}\n"),
    )
    .unwrap();
    fs::write(
        tools.workspace.path.join("leak.txt"),
        format!("token={aws}\n"),
    )
    .unwrap();
    let blocked = call(&tools, "read_file", json!({"path":".env"})).await;
    assert!(
        !blocked.success || blocked.output["redacted"] == true,
        "{}",
        blocked.output
    );
    assert!(
        blocked.output["error"]
            .as_str()
            .unwrap_or("")
            .contains("Refusing")
            || blocked.output["redacted"] == true
    );
    let leaked = call(&tools, "read_file", json!({"path":"leak.txt"})).await;
    assert!(leaked.success, "{}", leaked.error);
    let message = leaked.message("read_file", 50_000);
    let content = message["content"].as_str().unwrap();
    assert!(content.contains("[redacted secret]"), "{content}");
    assert!(!content.contains(&aws), "{content}");
}

#[tokio::test]
async fn workspace_symbols_find_rust_fixture() {
    let (_root, tools) = fixture(Config::default());
    fs::create_dir_all(tools.workspace.path.join("src")).unwrap();
    fs::write(
        tools.workspace.path.join("src/lib.rs"),
        "pub fn alpha() {}\npub struct Beta;\n",
    )
    .unwrap();
    let result = call(
        &tools,
        "workspace_symbols",
        json!({"query":"alpha","max_hits":20}),
    )
    .await;
    assert!(result.success, "{}", result.error);
    assert!(result.output.to_string().contains("alpha"));
}

#[tokio::test]
async fn sandbox_subdirectory_command_can_access_project_and_discards_its_scratch() {
    let mut config = Config::default();
    config.permissions.approve_shell = false;
    let (_root, tools) = fixture(config);
    fs::create_dir(tools.workspace.path.join("src")).unwrap();
    let result=call(&tools,"exec",json!({"cwd":"src","command":"printf once >> ../marker.txt; if test -n \"$SHADOWCODE_SCRATCH\"; then printf temporary > \"$SHADOWCODE_SCRATCH/temp\"; fi"})).await;
    assert!(result.success, "{} {:?}", result.error, result.output);
    assert_eq!(
        fs::read_to_string(tools.workspace.path.join("marker.txt")).unwrap(),
        "once"
    );
    if let Some(path) = result
        .output
        .pointer("/sandbox/scratch")
        .and_then(Value::as_str)
    {
        assert!(!std::path::Path::new(path).exists());
    }
}

/// Exercise the actual tool execution boundary, not only prepare_shell. Each
/// scenario gets its own subprocess so PATH cannot affect concurrent tests.
/// The bwrap stand-ins test refusal/no replay, not kernel containment.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "current_thread")]
async fn strict_sandbox_refuses_execution_and_never_replays_after_launch_failure() {
    const CASE: &str = "SHADOWCODE_SANDBOX_EXEC_CASE";
    const PROBE: &str = "SHADOWCODE_SANDBOX_EXEC_PROBE";
    let scenario = match std::env::var(CASE) {
        Ok(scenario) => scenario,
        Err(_) => {
            use std::os::unix::fs::PermissionsExt;
            for scenario in ["missing", "probe-failed", "launch-failed"] {
                let temp = tempfile::tempdir().unwrap();
                let bin = temp.path().join("bin");
                fs::create_dir(&bin).unwrap();
                let script = match scenario {
                    "missing" => None,
                    "probe-failed" => Some("#!/bin/sh\nexit 77\n"),
                    "launch-failed" => Some(
                        "#!/bin/sh\nprobe=\"$0.probe\"\nif test ! -e \"$probe\"; then\n  printf 'probe\\n' > \"$probe\"\n  exit 0\nfi\nprintf 'launch\\n' >> \"$probe\"\nexit 78\n",
                    ),
                    _ => unreachable!(),
                };
                if let Some(script) = script {
                    let path = bin.join("bwrap");
                    fs::write(&path, script).unwrap();
                    fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
                }
                let output = std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "strict_sandbox_refuses_execution_and_never_replays_after_launch_failure",
                        "--nocapture",
                        "--test-threads=1",
                    ])
                    .env(CASE, scenario)
                    .env(PROBE, bin.join("bwrap.probe"))
                    .env("PATH", &bin)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "{scenario}: {}{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            return;
        }
    };
    let mut config = Config::default();
    config.permissions.approve_shell = false;
    config.sandbox.require = true;
    config.sandbox.landlock = true;
    let (_root, mut tools) = fixture(config);
    let command = json!({"command":"printf once >> marker.txt", "timeout_sec":2});
    let refused = call(&tools, "exec", command.clone()).await;
    assert!(!refused.success, "{scenario}: {:?}", refused.output);
    assert!(
        !tools.workspace.path.join("marker.txt").exists(),
        "The refused command must never execute through a fallback"
    );
    if scenario == "launch-failed" {
        assert_eq!(refused.output["exit_code"], 78);
        assert_eq!(
            fs::read_to_string(std::env::var_os(PROBE).unwrap()).unwrap(),
            "probe\nlaunch\n",
            "Only the availability probe and one attempted launch are allowed"
        );
    } else {
        assert!(refused.error.contains("did not run"), "{}", refused.error);
        assert!(
            refused.error.contains("Require sandbox"),
            "{}",
            refused.error
        );
        assert!(refused.output.is_null());
    }

    // An explicit policy change makes the same valid command runnable. With
    // both isolation layers disabled, the result must report that fact.
    tools.config.sandbox.require = false;
    tools.config.sandbox.landlock = false;
    tools.config.permissions.network = false;
    let allowed = call(&tools, "exec", command).await;
    assert!(allowed.success, "{} {:?}", allowed.error, allowed.output);
    assert_eq!(allowed.output["sandbox"]["mode"], "none");
    assert_eq!(allowed.output["sandbox"]["network_requested"], "off");
    assert_eq!(allowed.output["sandbox"]["network_enforcement"], "none");
    assert_eq!(
        fs::read_to_string(tools.workspace.path.join("marker.txt")).unwrap(),
        "once"
    );
}
