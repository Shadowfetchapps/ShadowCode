use serde_json::json;
use shadowcode_core::{
    approvals::{Approval, ApprovalHub},
    checkpoint,
    config::{PermissionLevel, PermissionsConfig},
    permissions::{self, Decision},
    store::Store,
    workspace::Workspace,
};
use std::{fs, time::Duration};
use tokio_util::sync::CancellationToken;

fn approval() -> Approval {
    Approval {
        id: String::new(),
        session_id: "session".into(),
        task_id: "task".into(),
        tool: "exec".into(),
        arguments: json!({"command":"cargo test"}),
        command: "cargo test".into(),
        reason: "Run tests".into(),
        pending: false,
        created_at: 0.0,
        expires_at: 0.0,
        preview: serde_json::Value::Null,
        grant: String::new(),
        note: true,
        assessment: serde_json::Value::Null,
        always: String::new(),
    }
}

#[tokio::test]
async fn approval_is_bound_to_session_and_consumed_once() {
    let hub = ApprovalHub::default();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let worker = hub.clone();
    let task = tokio::spawn(async move {
        worker
            .request(
                approval(),
                Duration::from_secs(5),
                CancellationToken::new(),
                |a| {
                    sender.send(a.id.clone()).unwrap();
                },
            )
            .await
            .unwrap()
    });
    let id = receiver.await.unwrap();
    assert!(hub.decide(&id, "different", true).is_err());
    assert_eq!(hub.list(Some("session")).len(), 1);
    assert!(hub.list(Some("different")).is_empty());
    hub.decide(&id, "session", true).unwrap();
    assert!(hub.decide(&id, "session", true).is_err());
    assert!(task.await.unwrap());
    assert!(hub.list(None).is_empty());
}

#[tokio::test]
async fn expired_cancelled_and_aborted_approvals_never_linger() {
    let hub = ApprovalHub::default();
    assert!(!hub
        .request(
            approval(),
            Duration::from_millis(5),
            CancellationToken::new(),
            |_| {}
        )
        .await
        .unwrap());
    for abort in [false, true] {
        let cancel = CancellationToken::new();
        let token = cancel.clone();
        let worker = hub.clone();
        let (sender, receiver) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            worker
                .request(approval(), Duration::from_secs(30), token, |a| {
                    sender.send(a.id.clone()).unwrap();
                })
                .await
        });
        let id = receiver.await.unwrap();
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            cancel.cancel();
            assert!(!task.await.unwrap().unwrap());
        }
        assert!(hub.list(None).is_empty());
        assert!(hub.decide(&id, "session", true).is_err());
    }
}

#[test]
fn read_only_denies_shell_unknown_tools_and_branch_creation() {
    let config = PermissionsConfig {
        level: PermissionLevel::ReadOnly,
        ..Default::default()
    };
    for (tool, args) in [
        ("exec", json!({"command":"pwd"})),
        ("write_file", json!({})),
        ("not_registered", json!({})),
        ("git_branch", json!({"create":true,"name":"new"})),
    ] {
        assert!(
            matches!(permissions::check(&config, tool, &args), Decision::Deny(_)),
            "{tool}"
        );
    }
    assert_eq!(
        permissions::check(&config, "git_branch", &json!({})),
        Decision::Allow
    );
    assert!(matches!(
        permissions::check(
            &PermissionsConfig::default(),
            "exec",
            &json!({"command":"cargo test"})
        ),
        Decision::Ask(_)
    ));
}

#[test]
fn rewind_preflights_every_file_and_preserves_unrelated_edits() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let ws = Workspace::open(&project).unwrap();
    let store = Store::open(&root.path().join("db")).unwrap();
    for name in ["a", "b"] {
        ws.write(name, b"original", None).unwrap();
        checkpoint::record(
            &store,
            &ws,
            "task",
            name,
            &ws.snapshot(name).unwrap(),
            Some(b"agent"),
        )
        .unwrap();
        ws.write(name, b"agent", None).unwrap();
    }
    ws.write("a", b"user edit", None).unwrap();
    assert!(checkpoint::restore(&store, &ws, "task").is_err());
    assert_eq!(ws.read("a").unwrap().content, "user edit");
    assert_eq!(ws.read("b").unwrap().content, "agent");
    ws.write("a", b"agent", None).unwrap();
    assert_eq!(checkpoint::restore(&store, &ws, "task").unwrap().len(), 2);
    assert_eq!(ws.read("b").unwrap().content, "original");
    assert!(checkpoint::restore(&store, &ws, "task").unwrap().is_empty());
}

#[test]
fn rewind_handles_repeated_writes_crash_before_write_creates_deletes_and_mode() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let ws = Workspace::open(&project).unwrap();
    let store = Store::open(&root.path().join("db")).unwrap();
    ws.write("changed", b"original", None).unwrap();
    ws.set_mode("changed", 0o755).unwrap();
    checkpoint::record(
        &store,
        &ws,
        "task",
        "changed",
        &ws.snapshot("changed").unwrap(),
        Some(b"first edit"),
    )
    .unwrap();
    ws.write("changed", b"first edit", None).unwrap();
    // The second intent commits, then the application crashes before writing.
    checkpoint::record(
        &store,
        &ws,
        "task",
        "changed",
        &ws.snapshot("changed").unwrap(),
        Some(b"second edit"),
    )
    .unwrap();
    checkpoint::record(
        &store,
        &ws,
        "task",
        "created",
        &ws.snapshot("created").unwrap(),
        Some(b"new"),
    )
    .unwrap();
    ws.write("created", b"new", None).unwrap();
    ws.write("deleted", b"gone", None).unwrap();
    checkpoint::record(
        &store,
        &ws,
        "task",
        "deleted",
        &ws.snapshot("deleted").unwrap(),
        None,
    )
    .unwrap();
    ws.delete("deleted", None).unwrap();
    drop(store);
    let store = Store::open(&root.path().join("db")).unwrap();
    checkpoint::restore(&store, &ws, "task").unwrap();
    assert_eq!(ws.read("changed").unwrap().content, "original");
    assert_eq!(ws.snapshot("changed").unwrap().mode, Some(0o755));
    assert!(ws.snapshot("created").unwrap().bytes.is_none());
    assert_eq!(ws.read("deleted").unwrap().content, "gone");
}
