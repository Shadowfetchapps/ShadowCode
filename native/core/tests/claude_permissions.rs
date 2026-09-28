//! Claude's documented stdio permission contract, without a vendor account.
use serde_json::{json, Value};
use shadowcode_core::cli_agent::{
    adapter_for, CliAgentsConfig, LaunchOptions, Update, Vendor, VendorAnswer,
};
use std::{fs, path::Path, time::Duration};

fn launch(workspace: &Path) -> LaunchOptions {
    LaunchOptions {
        binary: "claude".into(),
        workspace: workspace.into(),
        model: "default".into(),
        read_only: false,
        resume: None,
        effort: None,
        legacy_effort: false,
        mcp_servers: Vec::new(),
    }
}

#[test]
fn claude_registers_stdio_permission_handler_without_widening_permissions() {
    let root = tempfile::tempdir().unwrap();
    let adapter = adapter_for(Vendor::Claude, false);
    for read_only in [false, true] {
        let mut options = launch(root.path());
        options.read_only = read_only;
        let (_, args) = adapter.command(&options);
        assert!(
            args.windows(2)
                .any(|pair| pair == ["--permission-prompt-tool", "stdio"]),
            "host prompts require the stdio permission handler: {args:?}"
        );
        assert!(args
            .windows(2)
            .any(|pair| pair == ["--permission-prompts", "host"]));
        assert!(!args.iter().any(|arg| matches!(
            arg.as_str(),
            "--allowedTools"
                | "--allowed-tools"
                | "--dangerously-skip-permissions"
                | "bypassPermissions"
                | "acceptEdits"
                | "--settings"
        )));
        assert_eq!(
            args.windows(2)
                .any(|pair| pair == ["--permission-mode", "plan"]),
            read_only
        );
    }
}

#[test]
fn claude_allow_returns_each_original_input_without_persisting_permission_rules() {
    let mut adapter = adapter_for(Vendor::Claude, false);
    let inputs = [
        json!({"file_path":"helpers.py","content":"print('first')\n"}),
        json!({"file_path":"other.py","old_string":"old","new_string":"new","replace_all":false}),
    ];
    for (index, input) in inputs.iter().enumerate() {
        let step = adapter
            .on_line(
                &json!({
                    "type":"control_request", "request_id":format!("request-{index}"),
                    "request":{"subtype":"can_use_tool","tool_name":"Edit","input":input}
                })
                .to_string(),
            )
            .unwrap();
        assert!(matches!(&step.updates[0], Update::Approval(p) if p.kind == "file_change"));
    }
    // Reverse order also checks that pending inputs stay bound to their IDs.
    for index in [1, 0] {
        let id = format!("request-{index}");
        let lines = adapter
            .answer(
                &id,
                &VendorAnswer {
                    allow: true,
                    for_session: true,
                    note: None,
                },
            )
            .unwrap();
        let frame: Value = serde_json::from_str(&lines[0]).unwrap();
        assert_eq!(frame["response"]["request_id"], id);
        assert_eq!(frame["response"]["response"]["behavior"], "allow");
        assert_eq!(frame["response"]["response"]["updatedInput"], inputs[index]);
        assert!(frame["response"]["response"]
            .get("updatedPermissions")
            .is_none());
        assert!(
            adapter.approve(&id, true).is_err(),
            "approval is consumed once"
        );
    }
}

#[test]
fn claude_denial_carries_the_note_without_returning_allowed_input() {
    let mut adapter = adapter_for(Vendor::Claude, false);
    adapter
        .on_line(
            &json!({
                "type":"control_request", "request_id":"denied",
                "request":{"subtype":"can_use_tool","tool_name":"Write",
                "input":{"file_path":"helpers.py","content":"must not write"}}
            })
            .to_string(),
        )
        .unwrap();
    let lines = adapter
        .answer(
            "denied",
            &VendorAnswer {
                allow: false,
                note: Some("Leave this file unchanged".into()),
                for_session: false,
            },
        )
        .unwrap();
    let frame: Value = serde_json::from_str(&lines[0]).unwrap();
    let answer = &frame["response"]["response"];
    assert_eq!(answer["behavior"], "deny");
    assert!(answer["message"]
        .as_str()
        .unwrap()
        .contains("Leave this file unchanged"));
    assert!(answer.get("updatedInput").is_none());
    assert!(answer.get("updatedPermissions").is_none());
    assert!(adapter.approve("denied", true).is_err());
}

#[cfg(unix)]
const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, sys
def send(value):
    print(json.dumps(value), flush=True)
args = sys.argv[1:]
stdio = any(args[i:i+2] == ['--permission-prompt-tool', 'stdio'] for i in range(len(args)))
original = {'file_path': os.path.join(os.getcwd(), 'helpers.py'), 'content': 'after\n'}
for raw in sys.stdin:
    frame = json.loads(raw)
    if frame.get('type') == 'user':
        send({'type':'system','subtype':'init','session_id':'fixture-session'})
        send({'type':'assistant','message':{'content':[{'type':'tool_use','id':'edit-1','name':'Write','input':original}]}})
        if not stdio:
            send({'type':'user','message':{'content':[{'type':'tool_result','tool_use_id':'edit-1','is_error':True,'content':'Permission was not granted: no stdio permission handler'}]}})
            send({'type':'result','subtype':'success','result':'Edit was denied before a host approval request'})
        else:
            send({'type':'control_request','request_id':'write-permission','request':{'subtype':'can_use_tool','tool_name':'Write','input':original}})
    elif frame.get('type') == 'control_response':
        outer = frame['response']
        answer = outer['response']
        assert outer['request_id'] == 'write-permission'
        assert 'updatedPermissions' not in answer
        if answer['behavior'] == 'allow':
            if answer.get('updatedInput') != original:
                send({'type':'result','subtype':'error_during_execution','is_error':True,'result':'Allowed permission response did not preserve the original tool input'})
                continue
            with open(original['file_path'], 'w') as out:
                out.write(original['content'])
            success = True
        else:
            success = False
        send({'type':'user','message':{'content':[{'type':'tool_result','tool_use_id':'edit-1','is_error':not success,'content':'write complete' if success else 'host denied write'}]}})
        send({'type':'result','subtype':'success','result':'Changed helpers.py' if success else 'Left helpers.py unchanged'})
"#;

#[cfg(unix)]
async fn run_permission_fixture(allow: bool, read_only: bool) {
    use shadowcode_core::{
        approvals::ApprovalHub, cli_agent::runner, events::TaskEvents, paths::AppPaths,
        steering::SteerControl, store::Store,
    };
    use std::{os::unix::fs::PermissionsExt, sync::Arc};
    use tokio_util::sync::CancellationToken;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("helpers.py"), "before\n").unwrap();
    let binary = root.path().join("fake-claude");
    fs::write(&binary, FAKE_CLAUDE).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let store = Arc::new(Store::open(&paths.database()).unwrap());
    let session = store.create_session(&project, "cli:claude", "").unwrap();
    let session_id = session["id"].as_str().unwrap().to_owned();
    let task_id = store.create_task(&session_id, "edit fixture").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(64);
    let events = TaskEvents {
        store: store.clone(),
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        sender,
    };
    let hub = ApprovalHub::default();
    let cancel = CancellationToken::new();
    let steer = SteerControl::default();
    let config = CliAgentsConfig {
        approval_timeout_sec: 10,
        max_run_time_sec: 10,
        ..Default::default()
    };
    let mut options = launch(&project);
    options.binary = binary.to_string_lossy().into_owned();
    options.read_only = read_only;
    let result = tokio::time::timeout(Duration::from_secs(5), async {
        let run = runner::run(runner::Request {
            vendor: Vendor::Claude, options, config: &config, prompt: "edit fixture".into(),
            images: Vec::new(), session_id: session_id.clone(), task_id: task_id.clone(),
            job_id: "fixture-job".into(), events: &events, approvals: &hub,
            cancel, steer: &steer, approvals_required: true, catalog: None,
        });
        tokio::pin!(run);
        loop {
            tokio::select! {
                result = &mut run => break result,
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    if let Some(approval) = hub.list(Some(&session_id)).into_iter().next() {
                        assert!(!read_only, "read-only writes should be declined without a host prompt");
                        assert_eq!(fs::read_to_string(project.join("helpers.py")).unwrap(), "before\n", "no write before approval");
                        hub.decide(&approval.id, &session_id, allow).unwrap();
                    }
                }
            }
        }
    }).await.expect("permission fixture timed out").unwrap();
    let events = store.recent_events(&session_id, 100).unwrap();
    let requested: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "approval.requested")
        .collect();
    assert_eq!(
        requested.len(),
        usize::from(!read_only),
        "missing stdio approval bridge: {events:?}"
    );
    let changed = allow && !read_only;
    assert_eq!(
        fs::read_to_string(project.join("helpers.py")).unwrap(),
        if changed { "after\n" } else { "before\n" }
    );
    let tool = events
        .iter()
        .find(|e| e["type"] == "tool.completed")
        .unwrap();
    assert_eq!(tool["payload"]["success"], changed);
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "files.changed")
            .count(),
        usize::from(changed)
    );
    assert_eq!(
        result.text,
        if changed {
            "Changed helpers.py"
        } else {
            "Left helpers.py unchanged"
        }
    );
    assert!(hub.list(None).is_empty());
}

#[cfg(unix)]
#[tokio::test]
async fn claude_write_waits_for_explicit_host_approval_then_changes_only_the_requested_file() {
    run_permission_fixture(true, false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_write_denial_preserves_the_file() {
    run_permission_fixture(false, false).await;
}

#[cfg(unix)]
#[tokio::test]
async fn claude_read_only_write_is_denied_without_requesting_user_approval() {
    run_permission_fixture(true, true).await;
}
