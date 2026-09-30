//! Approval cards explain each action, and "Always allow in this project"
//! covers exactly one low-risk command from then on.
use serde_json::json;
use shadowcode_core::{
    approvals::{always, Answer, ApprovalHub},
    config::{Config, PermissionMode},
    events::TaskEvents,
    models::ToolCall,
    store::{keys, Store},
    tools::{ToolExecutor, ToolResult},
    workspace::Workspace,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _root: tempfile::TempDir,
    project: PathBuf,
    store: Arc<Store>,
    session: String,
    tools: Arc<ToolExecutor>,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("Makefile"), "test:\n\t@echo checks passed\n").unwrap();
    let project = project.canonicalize().unwrap();
    let store = Arc::new(Store::open(&root.path().join("db")).unwrap());
    let session = store.create_session(&project, "mock", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let tools = executor(&store, &project, &session);
    Fixture {
        _root: root,
        project,
        store,
        session,
        tools,
    }
}

/// Tools for a new task of `session`, which runs in `folder`, in Ask mode.
fn executor(store: &Arc<Store>, folder: &Path, session: &str) -> Arc<ToolExecutor> {
    let task = store.create_task(session, "test").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(100);
    let mut config = Config::default();
    config.permissions.mode = PermissionMode::Ask;
    config.permissions.approve_shell = true;
    let tools = ToolExecutor::new(
        Arc::new(Workspace::open(folder).unwrap()),
        config,
        ApprovalHub::default(),
        TaskEvents {
            store: store.clone(),
            session_id: session.to_owned(),
            task_id: task,
            sender,
        },
        CancellationToken::new(),
    )
    .unwrap();
    Arc::new(tools)
}

fn exec(tools: &Arc<ToolExecutor>, command: &str) -> tokio::task::JoinHandle<ToolResult> {
    let tools = tools.clone();
    let command = command.to_owned();
    tokio::spawn(async move {
        tools
            .execute(ToolCall {
                id: shadowcode_core::id(),
                name: "exec".into(),
                arguments: json!({"command":command}),
            })
            .await
            .unwrap()
    })
}

async fn pending(tools: &ToolExecutor) -> Option<shadowcode_core::approvals::Approval> {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(a) = tools.approvals.list(None).pop() {
                break a;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .ok()
}

#[tokio::test]
async fn a_command_allowed_for_the_project_runs_without_asking_again() {
    let f = fixture();
    let first = exec(&f.tools, "make test");
    let card = pending(&f.tools).await.expect("the first run asks");
    assert_eq!(card.assessment["explanation"], "Runs make for test.");
    assert_eq!(card.assessment["risk"], "changes_files");
    assert_eq!(card.assessment["undo_label"], "Rewind can undo this");
    assert_eq!(card.always, "Always allow `make test` in this project");
    f.tools
        .approvals
        .answer(
            &card.id,
            &f.session,
            Answer {
                allow: true,
                for_project: true,
                ..Answer::default()
            },
        )
        .unwrap();
    let result = first.await.unwrap();
    assert!(result.success, "{}", result.error);
    let rules = always::list(&f.store, &f.project).unwrap();
    assert_eq!(rules.len(), 1);
    assert_eq!(rules[0].command, "make test");

    // The same command runs without a card; the transcript says why.
    let again = exec(&f.tools, "make  test").await.unwrap();
    assert!(again.success, "{}", again.error);
    assert!(f.tools.approvals.list(None).is_empty());
    let granted = f
        .store
        .recent_events(&f.session, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == "approval.granted")
        .collect::<Vec<_>>();
    assert_eq!(granted.len(), 1);
    assert_eq!(granted[0]["payload"]["scope"], "project");
    assert_eq!(granted[0]["payload"]["command"], "make test");

    // Anything else still asks: other arguments, or more steps.
    for command in ["make test && rm -rf src", "make install"] {
        let asking = exec(&f.tools, command);
        let card = pending(&f.tools).await.expect("asks again");
        assert!(
            card.always.is_empty() || !command.contains("&&"),
            "{command}"
        );
        f.tools
            .approvals
            .answer(&card.id, &f.session, Answer::deny())
            .unwrap();
        assert!(!asking.await.unwrap().success);
    }

    // Removed in Settings: it asks again.
    always::remove(&f.store, &f.project, "make test").unwrap();
    let asking = exec(&f.tools, "make test");
    let card = pending(&f.tools).await.expect("asks after removal");
    f.tools
        .approvals
        .answer(&card.id, &f.session, Answer::deny())
        .unwrap();
    assert!(!asking.await.unwrap().success);
}

#[tokio::test]
async fn a_subagent_in_its_own_worktree_uses_the_projects_rules() {
    // A write subagent or an implement role works in a throwaway worktree;
    // "Always allow in this project" there means its parent's project.
    let f = fixture();
    let worktree = f._root.path().join("worktree");
    fs::create_dir(&worktree).unwrap();
    fs::copy(f.project.join("Makefile"), worktree.join("Makefile")).unwrap();
    let worktree = worktree.canonicalize().unwrap();
    let child = f.store.create_session(&worktree, "mock", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.store
        .set_session_meta(&child, keys::SUBAGENT_PARENT, &f.session)
        .unwrap();
    let tools = executor(&f.store, &worktree, &child);
    let first = exec(&tools, "make test");
    let card = pending(&tools).await.expect("the first run asks");
    assert_eq!(card.always, "Always allow `make test` in this project");
    tools
        .approvals
        .answer(
            &card.id,
            &card.session_id,
            Answer {
                allow: true,
                for_project: true,
                ..Answer::default()
            },
        )
        .unwrap();
    assert!(first.await.unwrap().success);
    assert_eq!(always::list(&f.store, &f.project).unwrap().len(), 1);
    assert!(always::list(&f.store, &worktree).unwrap().is_empty());
    // The project's own tasks and its next subagent run it without asking.
    let again = exec(&f.tools, "make test").await.unwrap();
    assert!(again.success, "{}", again.error);
    assert!(f.tools.approvals.list(None).is_empty());
    let next = executor(&f.store, &worktree, &child);
    let again = exec(&next, "make test").await.unwrap();
    assert!(again.success, "{}", again.error);
    assert!(next.approvals.list(None).is_empty());
}

#[tokio::test]
async fn risky_commands_are_explained_and_never_offered_always_allow() {
    let f = fixture();
    let asking = exec(&f.tools, "rm -rf ~/.cache/example");
    let card = pending(&f.tools).await.expect("asks");
    assert_eq!(card.assessment["risk"], "destructive");
    assert_eq!(card.assessment["undo"], "no");
    assert!(card.always.is_empty());
    // "Always allow" is refused for a card that does not offer it.
    assert!(f
        .tools
        .approvals
        .answer(
            &card.id,
            &f.session,
            Answer {
                allow: true,
                for_project: true,
                ..Answer::default()
            }
        )
        .is_err());
    f.tools
        .approvals
        .answer(&card.id, &f.session, Answer::deny())
        .unwrap();
    assert!(!asking.await.unwrap().success);
    assert!(always::list(&f.store, &f.project).unwrap().is_empty());
}

#[tokio::test]
async fn only_change_these_asks_before_editing_other_files() {
    use shadowcode_core::mentions::Mention;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(project.join("src/ui")).unwrap();
    fs::write(project.join("src/ui/button.ts"), "export const a = 1;\n").unwrap();
    fs::write(project.join("README.md"), "# Demo\n").unwrap();
    let store = Arc::new(Store::open(&root.path().join("db")).unwrap());
    let session = store.create_session(&project, "mock", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task = store.create_task(&session, "t").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(100);
    let mut config = Config::default();
    config.permissions.mode = PermissionMode::AllowEdits;
    let tools = Arc::new(
        ToolExecutor::new(
            Arc::new(Workspace::open(&project).unwrap()),
            config,
            ApprovalHub::default(),
            TaskEvents {
                store: store.clone(),
                session_id: session.clone(),
                task_id: task,
                sender,
            },
            CancellationToken::new(),
        )
        .unwrap()
        .with_scope(Some(vec![Mention {
            path: "src/ui".into(),
            kind: "dir".into(),
        }])),
    );
    let write = |path: &str| {
        let tools = tools.clone();
        let path = path.to_owned();
        tokio::spawn(async move {
            tools
                .execute(ToolCall {
                    id: shadowcode_core::id(),
                    name: "write_file".into(),
                    arguments: json!({"path":path,"content":"changed\n"}),
                })
                .await
                .unwrap()
        })
    };
    // Inside the chosen folder: allowed as usual (edits are allowed).
    let inside = write("src/ui/new.ts").await.unwrap();
    assert!(inside.success, "{}", inside.error);
    assert!(tools.approvals.list(None).is_empty());
    // Elsewhere: asks, and says why.
    let outside = write("README.md");
    let card = pending(&tools).await.expect("asks");
    assert_eq!(
        card.reason,
        "Outside the files you chose for this task: README.md"
    );
    tools
        .approvals
        .answer(&card.id, &session, Answer::deny())
        .unwrap();
    assert!(!outside.await.unwrap().success);
    assert_eq!(
        fs::read_to_string(project.join("README.md")).unwrap(),
        "# Demo\n"
    );
}
