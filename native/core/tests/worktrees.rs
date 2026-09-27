use anyhow::Result;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
    worktrees,
};
use std::{fs, path::Path, process::Command};
use tokio_util::sync::CancellationToken;
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
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}
fn repository(root: &Path) {
    fs::create_dir(root).unwrap();
    git(root, &["init", "-q"]);
    git(root, &["config", "user.name", "Worktree Test"]);
    git(root, &["config", "user.email", "test@example.invalid"]);
    fs::write(root.join("tracked.txt"), "committed\n").unwrap();
    git(root, &["add", "tracked.txt"]);
    git(root, &["commit", "-qm", "Base"]);
}
async fn call(service: &Service, method: &str, body: Value) -> Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: "/api/worktrees".into(),
            body,
        })
        .await
}
#[tokio::test]
async fn isolated_checkout_preserves_dirty_source_and_disables_checkout_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    fs::write(project.join("tracked.txt"), "staged change\n").unwrap();
    git(&project, &["add", "tracked.txt"]);
    fs::write(project.join("tracked.txt"), "unstaged change\n").unwrap();
    fs::write(project.join("untracked.txt"), "keep me\n").unwrap();
    let before = git(&project, &["status", "--porcelain=v1"]);
    let head = git(&project, &["rev-parse", "HEAD"]);
    let staged = git(&project, &["show", ":tracked.txt"]);
    let hooks = project.join(".git/hooks");
    let hook = hooks.join("post-checkout");
    let marker = root.path().join("hook-executed");
    fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", marker.display())).unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let record = call(&service, "POST", json!({"reference":"HEAD"}))
        .await
        .unwrap();
    let checkout = Path::new(record["path"].as_str().unwrap());
    assert_eq!(record["state"], "ready");
    assert_eq!(record["base_commit"], head);
    assert_eq!(
        fs::read_to_string(checkout.join("tracked.txt")).unwrap(),
        "committed\n"
    );
    assert!(!checkout.join("untracked.txt").exists());
    assert!(!marker.exists());
    assert_eq!(git(&project, &["status", "--porcelain=v1"]), before);
    assert_eq!(git(&project, &["show", ":tracked.txt"]), staged);
    assert_eq!(
        fs::read_to_string(project.join("untracked.txt")).unwrap(),
        "keep me\n"
    );
    assert_eq!(
        git(checkout, &["branch", "--show-current"]),
        record["branch"].as_str().unwrap()
    );
    fs::write(checkout.join("tracked.txt"), "isolated edit\n").unwrap();
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "unstaged change\n"
    );
    let listed = call(&service, "GET", Value::Null).await.unwrap();
    assert_eq!(listed["worktrees"][0]["id"], record["id"]);
    assert_eq!(service.workspace().unwrap(), project);
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn invalid_refs_untrusted_read_only_and_nested_projects_do_not_create_worktrees() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    assert!(call(&service, "POST", json!({}))
        .await
        .unwrap_err()
        .to_string()
        .contains("Trust"));
    Config::patch(
        &paths,
        json!({"trusted_workspaces":[project],"permissions":{"level":"read_only"}}),
    )
    .unwrap();
    assert!(call(&service, "POST", json!({}))
        .await
        .unwrap_err()
        .to_string()
        .contains("read-only"));
    Config::patch(&paths, json!({"permissions":{"level":"workspace"}})).unwrap();
    for reference in ["missing-ref", "--help", "HEAD\nHEAD", ""] {
        assert!(call(&service, "POST", json!({"reference":reference}))
            .await
            .is_err());
    }
    let child = project.join("child");
    fs::create_dir(&child).unwrap();
    assert!(
        worktrees::create(&paths, &child, "HEAD", CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("repository root")
    );
    let different = root.path().join("different");
    fs::create_dir(&different).unwrap();
    assert!(call(&service, "POST", json!({"workspace":different}))
        .await
        .unwrap_err()
        .to_string()
        .contains("Project changed"));
    assert!(worktrees::list(&paths, &project).unwrap().is_empty());
    assert_eq!(
        git(&project, &["worktree", "list", "--porcelain"])
            .matches("worktree ")
            .count(),
        1
    );
    let _reservation = service.engine.reserve_workspace(&project).unwrap();
    assert!(call(&service, "POST", json!({})).await.is_err());
    drop(_reservation);
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn cancelled_checkout_leaves_a_recovery_record_and_source_intact() {
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    fs::write(project.join(".gitattributes"), "tracked.txt filter=slow\n").unwrap();
    git(&project, &["add", ".gitattributes"]);
    git(&project, &["commit", "-qm", "Checkout fixture"]);
    let marker = root.path().join("filter-started");
    git(
        &project,
        &[
            "config",
            "filter.slow.smudge",
            &format!("touch '{}'; sleep 60; cat", marker.display()),
        ],
    );
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let cancel = CancellationToken::new();
    let worker_paths = paths.clone();
    let worker_project = project.clone();
    let worker_cancel = cancel.clone();
    let worker = tokio::spawn(async move {
        worktrees::create(&worker_paths, &worker_project, "HEAD", worker_cancel).await
    });
    tokio::time::timeout(Duration::from_secs(10), async {
        while !marker.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    let failure = tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap()
        .unwrap_err();
    assert!(failure.to_string().contains("Recovery record"));
    let records = worktrees::list(&paths, &project).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, "needs_attention");
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "committed\n"
    );
    assert_eq!(git(&project, &["status", "--porcelain=v1"]), "");
}

async fn operation(service: &Service, path: &str, body: Value) -> Result<Value> {
    service
        .dispatch(Request {
            method: "POST".into(),
            path: path.into(),
            body,
        })
        .await
}
#[tokio::test]
async fn removal_refuses_changes_ignored_files_stale_review_and_busy_tasks_preserves_commits() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    fs::write(project.join(".gitignore"), "ignored.txt\n").unwrap();
    git(&project, &["add", ".gitignore"]);
    git(&project, &["commit", "-qm", "Ignore fixture"]);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let created = call(&service, "POST", json!({})).await.unwrap();
    let id = created["id"].as_str().unwrap();
    let checkout = Path::new(created["path"].as_str().unwrap());
    git(
        &project,
        &[
            "worktree",
            "lock",
            "--reason",
            "Keep this checkout",
            checkout.to_str().unwrap(),
        ],
    );
    let locked = operation(&service, "/api/worktrees/inspect", json!({"id":id}))
        .await
        .unwrap();
    assert_eq!(locked["can_remove"], false);
    assert!(locked["reason"].as_str().unwrap().contains("locked"));
    git(
        &project,
        &["worktree", "unlock", checkout.to_str().unwrap()],
    );
    let initial = operation(&service, "/api/worktrees/inspect", json!({"id":id}))
        .await
        .unwrap();
    assert_eq!(initial["can_remove"], true);
    for file in ["tracked.txt", "untracked.txt", "ignored.txt"] {
        fs::write(checkout.join(file), "local data\n").unwrap();
        let view = operation(&service, "/api/worktrees/inspect", json!({"id":id}))
            .await
            .unwrap();
        assert_eq!(view["can_remove"], false);
        assert!(operation(
            &service,
            "/api/worktrees/remove",
            json!({"id":id,"hash":view["hash"]})
        )
        .await
        .is_err());
        assert_eq!(
            fs::read_to_string(checkout.join(file)).unwrap(),
            "local data\n"
        );
        if file == "tracked.txt" {
            git(checkout, &["restore", "tracked.txt"]);
        } else {
            fs::remove_file(checkout.join(file)).unwrap();
        }
    }
    fs::write(checkout.join("tracked.txt"), "isolated commit\n").unwrap();
    git(checkout, &["add", "tracked.txt"]);
    git(checkout, &["commit", "-qm", "Keep isolated commit"]);
    let commit = git(checkout, &["rev-parse", "HEAD"]);
    assert!(operation(
        &service,
        "/api/worktrees/remove",
        json!({"id":id,"hash":initial["hash"]})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("changed"));
    let latest = operation(&service, "/api/worktrees/inspect", json!({"id":id}))
        .await
        .unwrap();
    let reservation = service.engine.reserve_workspace(checkout).unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/remove",
        json!({"id":id,"hash":latest["hash"]})
    )
    .await
    .is_err());
    drop(reservation);
    Config::patch(&paths, json!({"trusted_workspaces":[project,checkout]})).unwrap();
    let config = Config::load(&paths, Some(checkout)).unwrap();
    let process = service
        .engine
        .background()
        .start(checkout, &config, None, "worktree-server", "sleep 60")
        .unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/remove",
        json!({"id":id,"hash":latest["hash"]})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("background"));
    service.engine.background().stop(&process.id).await.unwrap();
    let removed = operation(
        &service,
        "/api/worktrees/remove",
        json!({"id":id,"hash":latest["hash"]}),
    )
    .await
    .unwrap();
    assert_eq!(removed["state"], "removed");
    assert!(!checkout.exists());
    assert_eq!(
        git(
            &project,
            &["rev-parse", created["branch"].as_str().unwrap()]
        ),
        commit
    );
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "committed\n"
    );
    assert!(worktrees::list(&paths, &project).unwrap().is_empty());
    assert!(paths
        .data
        .join("managed-worktrees/records/archive")
        .join(format!("{id}.json"))
        .exists());
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn worktree_inspection_rejects_detached_heads_symlinks_and_forged_paths() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    git(&record.path, &["checkout", "--detach", "-q"]);
    let detached = worktrees::inspect(&paths, &project, &record.id, CancellationToken::new())
        .await
        .unwrap();
    assert!(!detached.can_remove);
    assert!(detached.reason.contains("Detached"));
    let moved = record.path.with_extension("saved");
    fs::rename(&record.path, &moved).unwrap();
    symlink(&project, &record.path).unwrap();
    assert!(
        worktrees::inspect(&paths, &project, &record.id, CancellationToken::new())
            .await
            .is_err()
    );
    fs::remove_file(&record.path).unwrap();
    fs::rename(moved, &record.path).unwrap();
    let mut forged = record.clone();
    forged.path = project.clone();
    let file = paths
        .data
        .join("managed-worktrees/records")
        .join(format!("{}.json", record.id));
    fs::write(&file, serde_json::to_vec(&forged).unwrap()).unwrap();
    assert!(worktrees::list(&paths, &project)
        .unwrap_err()
        .to_string()
        .contains("identity changed"));
    assert!(
        worktrees::inspect(&paths, &project, &record.id, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("identity changed")
    );
    fs::remove_file(&file).unwrap();
    let outside = root.path().join("outside.json");
    fs::write(&outside, serde_json::to_vec(&record).unwrap()).unwrap();
    symlink(&outside, &file).unwrap();
    assert!(worktrees::list(&paths, &project).is_err());
    assert!(project.join("tracked.txt").exists());
}

#[tokio::test]
async fn missing_checkout_rescue_retains_commits_index_and_original_registration() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    assert!(
        worktrees::recovery(&paths, &project, &record.id, CancellationToken::new())
            .await
            .is_err()
    );
    fs::write(record.path.join("retained.txt"), "unmerged commit\n").unwrap();
    git(&record.path, &["add", "."]);
    git(&record.path, &["commit", "-qm", "Retained work"]);
    let commit = git(&record.path, &["rev-parse", "HEAD"]);
    fs::write(
        record.path.join("tracked.txt"),
        "staged recovery evidence\n",
    )
    .unwrap();
    git(&record.path, &["add", "tracked.txt"]);
    let index = git(
        &record.path,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    );
    let index_bytes = fs::read(&index).unwrap();
    let record_file = paths
        .data
        .join("managed-worktrees/records")
        .join(format!("{}.json", record.id));
    let record_bytes = fs::read(&record_file).unwrap();
    let saved = root.path().join("saved-checkout");
    fs::rename(&record.path, &saved).unwrap();
    git(
        &project,
        &["worktree", "lock", record.path.to_str().unwrap()],
    );
    assert!(
        worktrees::recovery(&paths, &project, &record.id, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("locked")
    );
    git(
        &project,
        &["worktree", "unlock", record.path.to_str().unwrap()],
    );
    let review = operation(&service, "/api/worktrees/recovery", json!({"id":record.id}))
        .await
        .unwrap();
    assert_eq!(review["commit"], commit);
    assert_eq!(review["branch"], record.branch);
    assert!(operation(
        &service,
        "/api/worktrees/restore",
        json!({"id":record.id,"hash":"stale"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("changed"));
    assert_eq!(worktrees::list(&paths, &project).unwrap().len(), 1);
    let restored = operation(
        &service,
        "/api/worktrees/restore",
        json!({"id":record.id,"hash":review["hash"]}),
    )
    .await
    .unwrap();
    let new_path = Path::new(restored["path"].as_str().unwrap());
    assert_ne!(new_path, record.path);
    assert_eq!(git(new_path, &["rev-parse", "HEAD"]), commit);
    assert_eq!(
        fs::read_to_string(new_path.join("retained.txt")).unwrap(),
        "unmerged commit\n"
    );
    assert_eq!(
        fs::read_to_string(new_path.join("tracked.txt")).unwrap(),
        "committed\n"
    );
    assert_eq!(fs::read(index).unwrap(), index_bytes);
    assert_eq!(fs::read(record_file).unwrap(), record_bytes);
    assert_eq!(git(&project, &["rev-parse", &record.branch]), commit);
    assert!(
        git(&project, &["worktree", "list", "--porcelain"]).contains(record.path.to_str().unwrap())
    );
    assert_eq!(
        fs::read_to_string(saved.join("tracked.txt")).unwrap(),
        "staged recovery evidence\n"
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn reviewed_return_preserves_diverged_source_and_requires_a_separate_commit() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    fs::write(record.path.join("incoming.txt"), "incoming work\n").unwrap();
    git(&record.path, &["add", "."]);
    git(&record.path, &["commit", "-qm", "Incoming"]);
    let first = operation(
        &service,
        "/api/worktrees/review-return",
        json!({"id":record.id}),
    )
    .await
    .unwrap();
    assert!(first["diff"].as_str().unwrap().contains("incoming work"));
    fs::write(project.join("source.txt"), "source branch work\n").unwrap();
    git(&project, &["add", "."]);
    git(&project, &["commit", "-qm", "Source diverged"]);
    let head = git(&project, &["rev-parse", "HEAD"]);
    assert!(operation(
        &service,
        "/api/worktrees/return",
        json!({"id":record.id,"hash":first["hash"]})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("changed"));
    assert!(!project.join("incoming.txt").exists());
    let review = operation(
        &service,
        "/api/worktrees/review-return",
        json!({"id":record.id}),
    )
    .await
    .unwrap();
    let busy = service.engine.reserve_workspace(&record.path).unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/return",
        json!({"id":record.id,"hash":review["hash"]})
    )
    .await
    .is_err());
    drop(busy);
    fs::write(project.join("untracked.txt"), "preserve me").unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/return",
        json!({"id":record.id,"hash":review["hash"]})
    )
    .await
    .is_err());
    assert_eq!(
        fs::read_to_string(project.join("untracked.txt")).unwrap(),
        "preserve me"
    );
    fs::remove_file(project.join("untracked.txt")).unwrap();
    let returned = operation(
        &service,
        "/api/worktrees/return",
        json!({"id":record.id,"hash":review["hash"]}),
    )
    .await
    .unwrap();
    assert_eq!(returned["state"], "merge_pending", "{returned}");
    assert_eq!(git(&project, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        git(&project, &["rev-parse", "MERGE_HEAD"]),
        review["worktree_head"]
    );
    assert_eq!(
        fs::read_to_string(project.join("source.txt")).unwrap(),
        "source branch work\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("incoming.txt")).unwrap(),
        "incoming work\n"
    );
    assert!(git(&record.path, &["status", "--porcelain"]).is_empty());
    assert!(operation(
        &service,
        "/api/worktrees/review-return",
        json!({"id":record.id})
    )
    .await
    .is_err());
    git(&project, &["commit", "-qm", "Reviewed return"]);
    assert_eq!(
        git(&project, &["rev-list", "--parents", "-n", "1", "HEAD"])
            .split_whitespace()
            .count(),
        3
    );
    assert!(operation(
        &service,
        "/api/worktrees/review-return",
        json!({"id":record.id})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("already present"));
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn reviewed_return_preserves_conflicts_for_resolution_or_abort() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    for (checkout, text) in [
        (&project, "source version\n"),
        (&record.path, "worktree version\n"),
    ] {
        fs::write(checkout.join("tracked.txt"), text).unwrap();
        git(checkout, &["add", "."]);
        git(checkout, &["commit", "-qm", "Divergent edit"]);
    }
    let head = git(&project, &["rev-parse", "HEAD"]);
    let review = operation(
        &service,
        "/api/worktrees/review-return",
        json!({"id":record.id}),
    )
    .await
    .unwrap();
    let result = operation(
        &service,
        "/api/worktrees/return",
        json!({"id":record.id,"hash":review["hash"]}),
    )
    .await
    .unwrap();
    assert_eq!(result["state"], "needs_attention");
    assert!(result["detail"].as_str().unwrap().contains("merge --abort"));
    assert_eq!(git(&project, &["rev-parse", "HEAD"]), head);
    assert!(
        git(&project, &["status", "--porcelain"]).contains("UU tracked.txt"),
        "{result}"
    );
    let conflict = fs::read_to_string(project.join("tracked.txt")).unwrap();
    assert!(conflict.contains("source version") && conflict.contains("worktree version"));
    assert_eq!(
        git(&record.path, &["rev-parse", "HEAD"]),
        review["worktree_head"]
    );
    git(&project, &["merge", "--abort"]);
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "source version\n"
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn reviewed_return_does_not_overwrite_ignored_source_files() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    fs::write(record.path.join("private-cache"), "incoming tracked file").unwrap();
    git(&record.path, &["add", "private-cache"]);
    git(&record.path, &["commit", "-qm", "Incoming path"]);
    fs::write(project.join(".git/info/exclude"), "private-cache\n").unwrap();
    let review = operation(
        &service,
        "/api/worktrees/review-return",
        json!({"id":record.id}),
    )
    .await
    .unwrap();
    let config = Config::load(&paths, Some(&project)).unwrap();
    let background = service
        .engine
        .background()
        .start(&project, &config, None, "source-server", "sleep 60")
        .unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/return",
        json!({"id":record.id,"hash":review["hash"]})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("background"));
    service
        .engine
        .background()
        .stop(&background.id)
        .await
        .unwrap();
    fs::write(project.join("private-cache"), "local ignored contents").unwrap();
    let result = operation(
        &service,
        "/api/worktrees/return",
        json!({"id":record.id,"hash":review["hash"]}),
    )
    .await
    .unwrap_err();
    assert!(result.to_string().contains("ignored source files"));
    assert_eq!(
        fs::read_to_string(project.join("private-cache")).unwrap(),
        "local ignored contents"
    );
    assert_eq!(git(&project, &["rev-parse", "HEAD"]), review["source_head"]);
    assert_eq!(
        fs::read_to_string(record.path.join("private-cache")).unwrap(),
        "incoming tracked file"
    );
    let redirected = root.path().join("redirected");
    fs::create_dir(&redirected).unwrap();
    git(
        &project,
        &["config", "core.worktree", redirected.to_str().unwrap()],
    );
    assert!(operation(
        &service,
        "/api/worktrees/review-return",
        json!({"id":record.id})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("root changed"));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn copied_source_changes_preserve_staging_binary_untracked_modes_and_source() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    fs::write(project.join("tracked.txt"), "staged\n").unwrap();
    git(&project, &["add", "tracked.txt"]);
    fs::write(project.join("tracked.txt"), "unstaged\n").unwrap();
    fs::write(project.join("binary.dat"), [0, 1, 2, 255]).unwrap();
    git(&project, &["add", "binary.dat"]);
    fs::write(project.join("binary.dat"), [0, 1, 4, 254]).unwrap();
    fs::write(project.join("intent.txt"), "intent-to-add contents\n").unwrap();
    git(&project, &["add", "--intent-to-add", "intent.txt"]);
    fs::create_dir(project.join("nested")).unwrap();
    fs::write(project.join("nested/tool"), "#!/bin/sh\necho original\n").unwrap();
    fs::set_permissions(
        project.join("nested/tool"),
        fs::Permissions::from_mode(0o750),
    )
    .unwrap();
    fs::write(project.join(".git/info/exclude"), "ignored.txt\n").unwrap();
    fs::write(project.join("ignored.txt"), "private ignored contents").unwrap();
    let status = git(&project, &["status", "--porcelain"]);
    let index = git(&project, &["show", ":tracked.txt"]);
    let head = git(&project, &["rev-parse", "HEAD"]);
    let review = operation(&service, "/api/worktrees/review-changes", json!({}))
        .await
        .unwrap();
    assert_eq!(review["untracked"].as_array().unwrap().len(), 1);
    assert!(operation(
        &service,
        "/api/worktrees/copy-changes",
        json!({"hash":"stale"})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("changed"));
    assert!(worktrees::list(&paths, &project).unwrap().is_empty());
    let copied = operation(
        &service,
        "/api/worktrees/copy-changes",
        json!({"hash":review["hash"]}),
    )
    .await
    .unwrap();
    let checkout = Path::new(copied["path"].as_str().unwrap());
    assert_eq!(copied["state"], "ready");
    assert_eq!(git(checkout, &["status", "--porcelain"]), status);
    assert_eq!(git(checkout, &["show", ":tracked.txt"]), index);
    assert_eq!(
        fs::read(checkout.join("binary.dat")).unwrap(),
        [0, 1, 4, 254]
    );
    assert_eq!(
        fs::read(checkout.join("nested/tool")).unwrap(),
        fs::read(project.join("nested/tool")).unwrap()
    );
    assert_eq!(
        fs::metadata(checkout.join("nested/tool"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o750
    );
    assert!(!checkout.join("ignored.txt").exists());
    assert_eq!(git(&project, &["status", "--porcelain"]), status);
    assert_eq!(git(&project, &["show", ":tracked.txt"]), index);
    assert_eq!(git(&project, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(project.join("ignored.txt")).unwrap(),
        "private ignored contents"
    );
    service.engine.shutdown().await.unwrap();
}
#[tokio::test]
async fn copy_review_rejects_symlinks_changed_contents_and_excludes_non_git_files() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    fs::write(project.join("new.txt"), "reviewed").unwrap();
    let review = operation(&service, "/api/worktrees/review-changes", json!({}))
        .await
        .unwrap();
    fs::write(project.join("new.txt"), "changed").unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/copy-changes",
        json!({"hash":review["hash"]})
    )
    .await
    .is_err());
    assert!(worktrees::list(&paths, &project).unwrap().is_empty());
    symlink("new.txt", project.join("link")).unwrap();
    assert!(
        operation(&service, "/api/worktrees/review-changes", json!({}))
            .await
            .is_err()
    );
    fs::remove_file(project.join("link")).unwrap();
    let fifo = std::ffi::CString::new(project.join("pipe").to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let inspected = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        operation(&service, "/api/worktrees/review-changes", json!({})),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(inspected["untracked"]
        .as_array()
        .unwrap()
        .iter()
        .all(|entry| entry["path"] != "pipe"));
    assert!(project.join("pipe").exists());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn interrupted_copy_retains_destination_record_and_never_resets_source() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    fs::write(project.join("tracked.txt"), "keep these edits\n").unwrap();
    let review = worktrees::changes::review(&project, CancellationToken::new())
        .await
        .unwrap();
    let error = worktrees::changes::copy(
        &paths,
        &project,
        &review.hash,
        CancellationToken::new(),
        |_| Err::<(), _>(anyhow::anyhow!("Destination reservation refused")),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("Partial copy retained"));
    let records = worktrees::list(&paths, &project).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].state, "needs_attention");
    assert!(records[0].path.exists());
    assert_eq!(
        fs::read_to_string(project.join("tracked.txt")).unwrap(),
        "keep these edits\n"
    );
    assert_eq!(git(&project, &["show", ":tracked.txt"]), "committed");
}

#[tokio::test]
async fn return_without_git_identity_preserves_source_and_can_be_retried() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    fs::write(record.path.join("incoming.txt"), "reviewed work\n").unwrap();
    git(&record.path, &["add", "incoming.txt"]);
    git(&record.path, &["commit", "-qm", "Incoming"]);
    let head = git(&project, &["rev-parse", "HEAD"]);
    // Empty repository-local values override any identity on the developer machine.
    git(&project, &["config", "user.name", ""]);
    git(&project, &["config", "user.email", ""]);
    let review = worktrees::review_return(&paths, &project, &record.id, CancellationToken::new())
        .await
        .unwrap();
    let result = worktrees::return_changes(
        &paths,
        &project,
        &record.id,
        &review.hash,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.state, "needs_attention", "{result:?}");
    assert!(
        result.detail.contains("identity") || result.detail.contains("empty ident"),
        "{}",
        result.detail
    );
    assert_eq!(git(&project, &["rev-parse", "HEAD"]), head);
    assert!(git(&project, &["status", "--porcelain"]).is_empty());
    assert!(!project.join("incoming.txt").exists());
    assert!(!project.join(".git/MERGE_HEAD").exists());
    git(&project, &["config", "user.name", "Worktree Test"]);
    git(&project, &["config", "user.email", "test@example.invalid"]);
    let review = worktrees::review_return(&paths, &project, &record.id, CancellationToken::new())
        .await
        .unwrap();
    let result = worktrees::return_changes(
        &paths,
        &project,
        &record.id,
        &review.hash,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.state, "merge_pending", "{result:?}");
    assert_eq!(git(&project, &["rev-parse", "HEAD"]), head);
    assert_eq!(
        fs::read_to_string(project.join("incoming.txt")).unwrap(),
        "reviewed work\n"
    );
}

#[tokio::test]
async fn reviewed_connection_repair_preserves_index_files_and_rejects_stale_state() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    fs::write(record.path.join("tracked.txt"), "staged\n").unwrap();
    git(&record.path, &["add", "tracked.txt"]);
    fs::write(record.path.join("tracked.txt"), "unstaged\n").unwrap();
    fs::write(record.path.join("untracked.txt"), "keep me\n").unwrap();
    let admin = record.common_directory.join("worktrees").join(&record.id);
    let index = fs::read(admin.join("index")).unwrap();
    let original_pointer = fs::read(record.path.join(".git")).unwrap();
    let original_registration = fs::read(admin.join("gitdir")).unwrap();
    fs::remove_file(record.path.join(".git")).unwrap();
    let review = operation(
        &service,
        "/api/worktrees/review-repair",
        json!({"id":record.id}),
    )
    .await
    .unwrap();
    assert!(review["checkout_pointer"].is_null());
    fs::remove_file(admin.join("gitdir")).unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/repair",
        json!({"id":record.id,"hash":review["hash"]})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("changed"));
    assert!(!record.path.join(".git").exists());
    let review = operation(
        &service,
        "/api/worktrees/review-repair",
        json!({"id":record.id}),
    )
    .await
    .unwrap();
    let busy = service.engine.reserve_workspace(&record.path).unwrap();
    assert!(operation(
        &service,
        "/api/worktrees/repair",
        json!({"id":record.id,"hash":review["hash"]})
    )
    .await
    .is_err());
    drop(busy);
    let repaired = operation(
        &service,
        "/api/worktrees/repair",
        json!({"id":record.id,"hash":review["hash"]}),
    )
    .await
    .unwrap();
    assert_eq!(repaired["state"], "ready");
    assert_eq!(fs::read(admin.join("index")).unwrap(), index);
    assert_eq!(
        fs::read(record.path.join(".git")).unwrap(),
        original_pointer
    );
    assert_eq!(
        fs::read(admin.join("gitdir")).unwrap(),
        original_registration
    );
    assert_eq!(git(&record.path, &["show", ":tracked.txt"]), "staged");
    assert_eq!(
        fs::read_to_string(record.path.join("tracked.txt")).unwrap(),
        "unstaged\n"
    );
    assert_eq!(
        fs::read_to_string(record.path.join("untracked.txt")).unwrap(),
        "keep me\n"
    );
    assert_eq!(git(&project, &["status", "--porcelain"]), "");
    assert!(operation(
        &service,
        "/api/worktrees/review-repair",
        json!({"id":record.id})
    )
    .await
    .unwrap_err()
    .to_string()
    .contains("already intact"));
    assert_eq!(
        fs::read_dir(paths.data.join("managed-worktrees/records/repairs"))
            .unwrap()
            .count(),
        1
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn connection_repair_refuses_foreign_links_locks_and_lost_indexes() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    let admin = record.common_directory.join("worktrees").join(&record.id);
    fs::write(record.path.join(".git"), "gitdir: /foreign/repository\n").unwrap();
    assert!(
        worktrees::repair::review(&paths, &project, &record.id, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("somewhere else")
    );
    fs::remove_file(record.path.join(".git")).unwrap();
    symlink(admin.join("gitdir"), record.path.join(".git")).unwrap();
    assert!(
        worktrees::repair::review(&paths, &project, &record.id, CancellationToken::new())
            .await
            .is_err()
    );
    fs::remove_file(record.path.join(".git")).unwrap();
    fs::write(admin.join("locked"), "unavailable drive").unwrap();
    assert!(
        worktrees::repair::review(&paths, &project, &record.id, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("Unlock")
    );
    fs::remove_file(admin.join("locked")).unwrap();
    fs::rename(admin.join("index"), admin.join("preserved-index")).unwrap();
    assert!(
        worktrees::repair::review(&paths, &project, &record.id, CancellationToken::new())
            .await
            .unwrap_err()
            .to_string()
            .contains("index is missing")
    );
    assert!(!record.path.join(".git").exists());
}

#[tokio::test]
async fn repair_refuses_relocated_checkout_without_guessing_a_path() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    repository(&project);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
        .await
        .unwrap();
    let elsewhere = root.path().join("elsewhere");
    fs::rename(&record.path, &elsewhere).unwrap();
    let error = worktrees::repair::review(&paths, &project, &record.id, CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(
        error.contains("original real directory")
            || error.contains("Repair requires")
            || error.contains("No such file")
            || error.contains("os error 2"),
        "{error}"
    );
    assert!(
        !record.path.exists(),
        "must not recreate a guessed checkout"
    );
    assert!(elsewhere.exists(), "relocated tree is left untouched");
    let advice = shadowcode_core::autonomy::worktree_recovery_advice("worktree path was moved");
    assert_eq!(advice["auto_recover"], false);
    assert_eq!(advice["guess_paths"], false);
}

#[cfg(target_os = "linux")]
mod cleanup_failure {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};
    use std::path::PathBuf;

    struct Fixture {
        // Drop restores permissions before TempDir removes fixture evidence.
        root: tempfile::TempDir,
        project: PathBuf,
        paths: AppPaths,
        record: worktrees::Record,
        mode: u32,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            for path in [
                self.record.path.join("blocked"),
                retained(self).join("blocked"),
            ] {
                if fs::symlink_metadata(&path)
                    .is_ok_and(|m| m.is_dir() && !m.file_type().is_symlink())
                {
                    let _ = fs::set_permissions(&path, fs::Permissions::from_mode(self.mode));
                }
            }
        }
    }
    async fn failed() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let project = root.path().join("project");
        repository(&project);
        let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
        let record = worktrees::create(&paths, &project, "HEAD", CancellationToken::new())
            .await
            .unwrap();
        fs::create_dir(record.path.join("blocked")).unwrap();
        fs::write(
            record.path.join("blocked/new.txt"),
            "recoverable new data\n",
        )
        .unwrap();
        let mode = fs::metadata(record.path.join("blocked"))
            .unwrap()
            .permissions()
            .mode();
        let f = Fixture {
            root,
            project,
            paths,
            record,
            mode,
        };
        fs::set_permissions(
            f.record.path.join("blocked"),
            fs::Permissions::from_mode(mode & !0o222),
        )
        .unwrap();
        assert_eq!(
            fs::remove_file(f.record.path.join("blocked/new.txt"))
                .expect_err("Run without root/DAC override")
                .kind(),
            std::io::ErrorKind::PermissionDenied
        );
        let error =
            worktrees::dispose(&f.paths, &f.project, &f.record.id, CancellationToken::new())
                .await
                .unwrap_err();
        assert!(
            format!("{error:#}").contains("Git worktree operation failed"),
            "{error:#}"
        );
        fs::set_permissions(
            retained(&f).join("blocked"),
            fs::Permissions::from_mode(mode),
        )
        .unwrap();
        assert!(journal(&f).is_file());
        assert_eq!(git(&f.project, &["worktree", "list"]).lines().count(), 1);
        f
    }
    fn retained(f: &Fixture) -> PathBuf {
        f.record
            .path
            .parent()
            .unwrap()
            .join(".cleanup")
            .join(&f.record.id)
            .join("checkout")
    }
    fn journal(f: &Fixture) -> PathBuf {
        f.paths
            .data
            .join("managed-worktrees/records/cleanup")
            .join(format!("{}.json", f.record.id))
    }
    async fn refused(f: &Fixture, reason: &str) {
        let before = fs::read(journal(f)).unwrap();
        let source = fs::read(f.project.join("tracked.txt")).unwrap();
        let error =
            worktrees::dispose(&f.paths, &f.project, &f.record.id, CancellationToken::new())
                .await
                .unwrap_err();
        assert!(format!("{error:#}").contains(reason), "{error:#}");
        assert_eq!(fs::read(journal(f)).unwrap(), before);
        assert_eq!(fs::read(f.project.join("tracked.txt")).unwrap(), source);
        assert_eq!(
            git(&f.project, &["rev-parse", &f.record.branch]),
            f.record.base_commit
        );
        assert_eq!(
            worktrees::list(&f.paths, &f.project).unwrap()[0].state,
            "removing"
        );
    }
    #[tokio::test]
    async fn changed_survivors_and_foreign_git_pointer_are_preserved() {
        for variant in [
            "new",
            "changed",
            "replacement",
            "foreign",
            "symlink",
            "foreign_git",
        ] {
            let f = failed().await;
            let file = if variant == "new" {
                retained(&f).join("later.txt")
            } else if variant == "changed" {
                retained(&f).join("blocked/new.txt")
            } else if variant == "foreign_git" {
                retained(&f).join(".git")
            } else {
                retained(&f).join("blocked/new.txt")
            };
            match variant {
                "replacement" => {
                    let bytes = fs::read(&file).unwrap();
                    fs::rename(&file, f.root.path().join("old-pointer")).unwrap();
                    fs::write(&file, bytes).unwrap();
                }
                "symlink" => {
                    fs::remove_file(&file).unwrap();
                    symlink(f.project.join("tracked.txt"), &file).unwrap();
                }
                _ => fs::write(&file, "later user data\n").unwrap(),
            }
            refused(&f, "new or changed data").await;
            assert!(fs::symlink_metadata(&file).is_ok());
        }
    }
    #[tokio::test]
    async fn replacement_directory_and_symlink_lane_are_preserved() {
        for link in [false, true] {
            let f = failed().await;
            let original = f.root.path().join("original-lane");
            fs::rename(retained(&f), &original).unwrap();
            if link {
                symlink(&f.project, retained(&f)).unwrap();
            } else {
                fs::create_dir(retained(&f)).unwrap();
                fs::write(retained(&f).join("foreign.txt"), "foreign user data\n").unwrap();
            }
            refused(
                &f,
                if link {
                    "real directory"
                } else {
                    "checkout was replaced"
                },
            )
            .await;
            assert_eq!(
                fs::read_to_string(original.join("blocked/new.txt")).unwrap(),
                "recoverable new data\n"
            );
            if !link {
                assert_eq!(
                    fs::read_to_string(retained(&f).join("foreign.txt")).unwrap(),
                    "foreign user data\n"
                );
            }
        }
    }
    #[tokio::test]
    async fn foreign_admin_and_changed_branch_are_preserved() {
        for branch in [false, true] {
            let f = failed().await;
            if branch {
                let tree = git(&f.project, &["rev-parse", "HEAD^{tree}"]);
                let commit = git(
                    &f.project,
                    &["commit-tree", &tree, "-p", "HEAD", "-m", "New branch work"],
                );
                git(
                    &f.project,
                    &[
                        "update-ref",
                        &format!("refs/heads/{}", f.record.branch),
                        &commit,
                    ],
                );
                let before = fs::read(journal(&f)).unwrap();
                let error = worktrees::dispose(
                    &f.paths,
                    &f.project,
                    &f.record.id,
                    CancellationToken::new(),
                )
                .await
                .unwrap_err();
                assert!(
                    error.to_string().contains("Managed branch changed"),
                    "{error:#}"
                );
                assert_eq!(git(&f.project, &["rev-parse", &f.record.branch]), commit);
                assert_eq!(fs::read(journal(&f)).unwrap(), before);
            } else {
                let admin = f
                    .record
                    .common_directory
                    .join("worktrees")
                    .join(&f.record.id);
                fs::create_dir_all(&admin).unwrap();
                // Git removed the empty parent. Its inode can be immediately
                // reused, which exercises the different admin-content guard.
                // Keep that allocation alive if necessary so this fixture
                // deterministically reaches the replaced-parent boundary.
                use std::os::unix::fs::MetadataExt;
                let intent: Value =
                    serde_json::from_slice(&fs::read(journal(&f)).unwrap()).unwrap();
                let expected_parent = (
                    intent["admin_parent_identity"]["dev"].as_u64().unwrap(),
                    intent["admin_parent_identity"]["ino"].as_u64().unwrap(),
                );
                let parent = admin.parent().unwrap();
                let metadata = fs::metadata(parent).unwrap();
                if (metadata.dev(), metadata.ino()) == expected_parent {
                    fs::rename(parent, f.root.path().join("retained-parent-allocation")).unwrap();
                    fs::create_dir_all(&admin).unwrap();
                }
                let metadata = fs::metadata(parent).unwrap();
                assert_ne!((metadata.dev(), metadata.ino()), expected_parent);
                fs::write(admin.join("foreign.txt"), "foreign registration data\n").unwrap();
                refused(&f, "administration parent changed around an occupied ID").await;
                assert_eq!(
                    fs::read_to_string(admin.join("foreign.txt")).unwrap(),
                    "foreign registration data\n"
                );
            }
        }
    }
    #[tokio::test]
    async fn old_unjournaled_failure_and_budget_overflow_stay_pending() {
        let f = failed().await;
        let backup = fs::read(journal(&f)).unwrap();
        fs::remove_file(journal(&f)).unwrap();
        let error =
            worktrees::dispose(&f.paths, &f.project, &f.record.id, CancellationToken::new())
                .await
                .unwrap_err();
        assert!(
            error.to_string().contains("no ownership journal"),
            "{error:#}"
        );
        assert!(retained(&f).join("blocked/new.txt").is_file());
        fs::write(journal(&f), backup).unwrap();
        let huge = retained(&f).join("large-build-output");
        fs::File::create(&huge)
            .unwrap()
            .set_len(8_u64 * 1024 * 1024 * 1024 + 1)
            .unwrap();
        refused(&f, "byte budget").await;
        assert_eq!(
            fs::metadata(huge).unwrap().len(),
            8_u64 * 1024 * 1024 * 1024 + 1
        );
    }
    #[tokio::test]
    async fn later_worktree_from_another_profile_survives_cleanup_parent_rebind() {
        let f = failed().await;
        git(&f.project, &["worktree", "prune", "--expire", "now"]);
        let other_paths = AppPaths::isolated(&f.root.path().join("other-profile")).unwrap();
        let other = worktrees::create(&other_paths, &f.project, "HEAD", CancellationToken::new())
            .await
            .unwrap();
        let admin = other.common_directory.join("worktrees").join(&other.id);
        let index = fs::read(admin.join("index")).unwrap();
        worktrees::dispose(&f.paths, &f.project, &f.record.id, CancellationToken::new())
            .await
            .unwrap();
        assert!(!retained(&f).exists());
        assert!(other.path.join("tracked.txt").is_file());
        assert_eq!(fs::read(admin.join("index")).unwrap(), index);
        assert_eq!(
            git(&f.project, &["rev-parse", &other.branch]),
            other.base_commit
        );
        assert_eq!(
            worktrees::list(&other_paths, &f.project).unwrap()[0].state,
            "ready"
        );
    }
    #[tokio::test]
    async fn symlinked_admin_parent_never_receives_restored_metadata() {
        let f = failed().await;
        let outside = f.root.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("user.txt"), "foreign data\n").unwrap();
        symlink(&outside, f.record.common_directory.join("worktrees")).unwrap();
        refused(&f, "real directory").await;
        assert_eq!(
            fs::read_to_string(outside.join("user.txt")).unwrap(),
            "foreign data\n"
        );
        assert_eq!(fs::read_dir(&outside).unwrap().count(), 1);
    }
    #[test]
    fn cross_process_disposal_child() {
        let Some(root) = std::env::var_os("SHADOWCODE_DISPOSAL_LOCK_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        let paths = AppPaths::isolated(&root.join("profile")).unwrap();
        let project = root.join("project");
        let record = worktrees::list(&paths, &project).unwrap().pop().unwrap();
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let error = runtime
            .block_on(worktrees::dispose(
                &paths,
                &project,
                &record.id,
                CancellationToken::new(),
            ))
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("Another process owns worktree cleanup"),
            "{error:#}"
        );
        let targets: Value =
            serde_json::from_slice(&fs::read(root.join("mutation-targets.json")).unwrap()).unwrap();
        let other = AppPaths::isolated(&root.join("other-profile")).unwrap();
        let create_profile = AppPaths::isolated(&root.join("third-profile")).unwrap();
        let create = runtime
            .block_on(worktrees::create(
                &create_profile,
                &project,
                "HEAD",
                CancellationToken::new(),
            ))
            .unwrap_err();
        let remove = runtime
            .block_on(worktrees::remove(
                &other,
                &project,
                targets["clean_id"].as_str().unwrap(),
                targets["clean_hash"].as_str().unwrap(),
                CancellationToken::new(),
            ))
            .unwrap_err();
        let repair = runtime
            .block_on(worktrees::repair::apply(
                &other,
                &project,
                targets["repair_id"].as_str().unwrap(),
                targets["repair_hash"].as_str().unwrap(),
                CancellationToken::new(),
            ))
            .unwrap_err();
        for error in [create, remove, repair] {
            assert!(
                error
                    .to_string()
                    .contains("Another process owns worktree cleanup"),
                "Mutation bypassed common-directory ownership: {error:#}"
            );
        }
        assert!(worktrees::list(&create_profile, &project)
            .unwrap()
            .is_empty());
        assert!(record
            .path
            .parent()
            .unwrap()
            .join(".cleanup")
            .join(&record.id)
            .join("checkout/blocked/new.txt")
            .is_file());
    }
    #[tokio::test]
    async fn non_compare_cleanup_obeys_cross_process_disposal_lock() {
        use fs2::FileExt;
        let f = failed().await;
        let other = AppPaths::isolated(&f.root.path().join("other-profile")).unwrap();
        let clean = worktrees::create(&other, &f.project, "HEAD", CancellationToken::new())
            .await
            .unwrap();
        let clean_review =
            worktrees::inspect(&other, &f.project, &clean.id, CancellationToken::new())
                .await
                .unwrap();
        let repair = worktrees::create(&other, &f.project, "HEAD", CancellationToken::new())
            .await
            .unwrap();
        fs::remove_file(repair.path.join(".git")).unwrap();
        let repair_review =
            worktrees::repair::review(&other, &f.project, &repair.id, CancellationToken::new())
                .await
                .unwrap();
        fs::write(f.root.path().join("mutation-targets.json"), serde_json::to_vec(&json!({"clean_id":clean.id,"clean_hash":clean_review.hash,"repair_id":repair.id,"repair_hash":repair_review.hash})).unwrap()).unwrap();
        let source_index = fs::read(f.record.common_directory.join("index")).unwrap();
        let clean_index = fs::read(
            f.record
                .common_directory
                .join("worktrees")
                .join(&clean.id)
                .join("index"),
        )
        .unwrap();
        let repair_index = fs::read(
            f.record
                .common_directory
                .join("worktrees")
                .join(&repair.id)
                .join("index"),
        )
        .unwrap();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(f.record.common_directory.join("shadowcode-disposal.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let before = fs::read(journal(&f)).unwrap();
        let child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "cleanup_failure::cross_process_disposal_child",
                "--nocapture",
            ])
            .env("SHADOWCODE_DISPOSAL_LOCK_FIXTURE", f.root.path())
            .output()
            .unwrap();
        assert!(
            child.status.success(),
            "{}{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
        assert!(String::from_utf8_lossy(&child.stdout).contains("1 passed"));
        assert_eq!(fs::read(journal(&f)).unwrap(), before);
        assert_eq!(
            fs::read(f.record.common_directory.join("index")).unwrap(),
            source_index
        );
        assert_eq!(
            fs::read(
                f.record
                    .common_directory
                    .join("worktrees")
                    .join(&clean.id)
                    .join("index")
            )
            .unwrap(),
            clean_index
        );
        assert_eq!(
            fs::read(
                f.record
                    .common_directory
                    .join("worktrees")
                    .join(&repair.id)
                    .join("index")
            )
            .unwrap(),
            repair_index
        );
        assert!(clean.path.join(".git").is_file());
        assert!(!repair.path.join(".git").exists());
        drop(lock);
        worktrees::dispose(&f.paths, &f.project, &f.record.id, CancellationToken::new())
            .await
            .unwrap();
        worktrees::remove(
            &other,
            &f.project,
            &clean.id,
            &clean_review.hash,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        worktrees::repair::apply(
            &other,
            &f.project,
            &repair.id,
            &repair_review.hash,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        worktrees::dispose(&other, &f.project, &repair.id, CancellationToken::new())
            .await
            .unwrap();
        assert!(!f.record.path.exists());
    }
}
// Real processes contend for one profile's final slot while mutating different
// repositories. A parent-held admission file proves both reached that boundary;
// releasing it then permits exactly one durable reservation, never two.
#[cfg(unix)]
mod profile_admission {
    use super::*;
    use fs2::FileExt;
    use std::{
        path::PathBuf,
        process::{Child, Stdio},
        time::{Duration, Instant},
    };

    struct OwnedChild(Child);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.0.try_wait().ok().flatten().is_none() {
                let _ = self.0.kill();
            }
            let _ = self.0.wait();
        }
    }

    #[test]
    fn child() {
        let Some(root) = std::env::var_os("SHADOWCODE_PROFILE_ADMISSION_FIXTURE") else {
            return;
        };
        let root = PathBuf::from(root);
        let label = std::env::var("SHADOWCODE_PROFILE_ADMISSION_CHILD").unwrap();
        assert!(matches!(label.as_str(), "a" | "b"));
        let paths = AppPaths::isolated(&root.join("profile")).unwrap();
        let project = root.join(&label);
        fs::write(root.join(format!("{label}.ready")), b"ready").unwrap();
        let started = Instant::now();
        while !root.join("go").is_file() {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "Parent never released child"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let result = runtime.block_on(async {
            let cancel = CancellationToken::new();
            let expiry = cancel.clone();
            let watchdog = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_secs(10)).await;
                expiry.cancel();
            });
            let result = loop {
                match worktrees::create(&paths, &project, "HEAD", cancel.clone()).await {
                    Ok(record) => break json!({"outcome":"created", "record":record}),
                    Err(error)
                        if error
                            .to_string()
                            .contains("Another process owns worktree admission") =>
                    {
                        fs::write(
                            root.join(format!("{label}.blocked")),
                            b"observed actual advisory contention",
                        )
                        .unwrap();
                        assert!(
                            !cancel.is_cancelled(),
                            "Admission remained busy until deadline: {error:#}"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    Err(error) => {
                        assert!(
                            error
                                .to_string()
                                .contains("At most 64 managed worktree records"),
                            "Unexpected create failure: {error:#}"
                        );
                        break json!({"outcome":"capacity", "error":format!("{error:#}")});
                    }
                }
            };
            watchdog.abort();
            let _ = watchdog.await;
            result
        });
        fs::write(
            root.join(format!("{label}.result.json")),
            serde_json::to_vec_pretty(&result).unwrap(),
        )
        .unwrap();
    }

    async fn wait_for(root: &Path, names: &[&str], children: &mut [OwnedChild]) {
        let started = Instant::now();
        loop {
            if names.iter().all(|name| root.join(name).is_file()) {
                return;
            }
            for (index, child) in children.iter_mut().enumerate() {
                if let Some(status) = child.0.try_wait().unwrap() {
                    panic!(
                        "Child {index} exited {status} before barrier {names:?}: {}",
                        fs::read_to_string(root.join(format!("{}.log", ["a", "b"][index])))
                            .unwrap()
                    );
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "Timed out at {names:?}"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn different_processes_and_repositories_reserve_only_one_final_profile_slot() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        repository(&a);
        repository(&b);
        let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
        let first = worktrees::create(&paths, &a, "HEAD", CancellationToken::new())
            .await
            .unwrap();
        let records = paths.data.join("managed-worktrees/records");
        for _ in 0..62 {
            let mut pending = first.clone();
            pending.id = shadowcode_core::id();
            pending.path = paths
                .data
                .join("managed-worktrees/checkouts")
                .join(&pending.id);
            pending.branch = format!("shadowcode/{}", pending.id);
            pending.state = "creating".into();
            pending.detail =
                "Durable interrupted pre-Git reservation; never silently reclaim".into();
            fs::write(
                records.join(format!("{}.json", pending.id)),
                serde_json::to_vec(&pending).unwrap(),
            )
            .unwrap();
        }
        let source_before: Vec<_> = [&a, &b]
            .into_iter()
            .map(|path| {
                (
                    git(path, &["rev-parse", "HEAD"]),
                    fs::read(path.join(".git/index")).unwrap(),
                )
            })
            .collect();
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(records.join(".admission.lock"))
            .unwrap();
        lock.lock_exclusive().unwrap();
        let mut children = Vec::new();
        for label in ["a", "b"] {
            let log = fs::File::create(root.path().join(format!("{label}.log"))).unwrap();
            children.push(OwnedChild(
                Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", "profile_admission::child", "--nocapture"])
                    .env("SHADOWCODE_PROFILE_ADMISSION_FIXTURE", root.path())
                    .env("SHADOWCODE_PROFILE_ADMISSION_CHILD", label)
                    .stdin(Stdio::null())
                    .stdout(log.try_clone().unwrap())
                    .stderr(log)
                    .spawn()
                    .unwrap(),
            ));
        }
        wait_for(root.path(), &["a.ready", "b.ready"], &mut children).await;
        fs::write(root.path().join("go"), b"both processes ready").unwrap();
        wait_for(root.path(), &["a.blocked", "b.blocked"], &mut children).await;
        drop(lock);
        let started = Instant::now();
        for (index, child) in children.iter_mut().enumerate() {
            let status = loop {
                if let Some(status) = child.0.try_wait().unwrap() {
                    break status;
                }
                assert!(
                    started.elapsed() < Duration::from_secs(15),
                    "Child did not complete"
                );
                tokio::time::sleep(Duration::from_millis(5)).await;
            };
            let log =
                fs::read_to_string(root.path().join(format!("{}.log", ["a", "b"][index]))).unwrap();
            assert!(status.success(), "{status}: {log}");
            assert!(
                log.contains("1 passed; 0 failed"),
                "Child test selector did not run: {log}"
            );
        }
        let results: Vec<Value> = ["a", "b"]
            .into_iter()
            .map(|label| {
                serde_json::from_slice(
                    &fs::read(root.path().join(format!("{label}.result.json"))).unwrap(),
                )
                .unwrap()
            })
            .collect();
        assert_eq!(
            results
                .iter()
                .filter(|result| result["outcome"] == "created")
                .count(),
            1,
            "{results:?}"
        );
        assert_eq!(
            results
                .iter()
                .filter(|result| result["outcome"] == "capacity")
                .count(),
            1,
            "{results:?}"
        );
        let active: Vec<worktrees::Record> = fs::read_dir(&records)
            .unwrap()
            .map(Result::unwrap)
            .filter(|entry| entry.path().extension().and_then(|s| s.to_str()) == Some("json"))
            .map(|entry| serde_json::from_slice(&fs::read(entry.path()).unwrap()).unwrap())
            .collect();
        assert_eq!(active.len(), 64);
        assert_eq!(
            active
                .iter()
                .filter(|record| record.state == "creating")
                .count(),
            62,
            "Restart must not silently free interrupted reservations"
        );
        for (path, (head, index)) in [&a, &b].into_iter().zip(source_before) {
            assert_eq!(git(path, &["rev-parse", "HEAD"]), head);
            assert_eq!(fs::read(path.join(".git/index")).unwrap(), index);
            assert_eq!(fs::read(path.join("tracked.txt")).unwrap(), b"committed\n");
        }
        for record in active.iter().filter(|record| record.state == "ready") {
            assert_eq!(
                fs::read(record.path.join("tracked.txt")).unwrap(),
                b"committed\n"
            );
            worktrees::dispose(&paths, &record.source, &record.id, CancellationToken::new())
                .await
                .unwrap();
        }
        let retained = worktrees::list(&paths, &a).unwrap();
        assert_eq!(retained.len(), 62);
        assert!(retained.iter().all(|record| record.state == "creating"));
    }
}
