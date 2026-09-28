use serde_json::json;
use shadowcode_core::{
    config::{secret, set_secret, Config, PermissionLevel},
    paths::AppPaths,
    store::Store,
    workspace::Workspace,
};
use std::{fs, sync::Arc};

#[test]
fn profile_lock_prevents_a_second_manager_and_releases_on_drop() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let first = paths.lock().unwrap();
    assert!(paths.lock().is_err());
    drop(first);
    assert!(paths.lock().is_ok());
}

#[test]
#[cfg(unix)]
fn profile_permissions_are_private_and_legacy_data_is_preserved() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let parent = root.path().join("existing-parent");
    fs::create_dir(&parent).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o755)).unwrap();
    // Relocated XDG/profile parents are supported; application-owned leaves
    // themselves cannot be links.
    let alias = root.path().join("parent-alias");
    symlink(&parent, &alias).unwrap();
    let paths = AppPaths::isolated(&alias).unwrap();
    for path in [&paths.config, &paths.data, &paths.state] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        fs::write(path.join("legacy.txt"), "preserve me").unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(paths.state.join("native.lock"), "legacy lock").unwrap();
    fs::set_permissions(
        paths.state.join("native.lock"),
        fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let lock = paths.lock().unwrap();
    for path in [&paths.config, &paths.data, &paths.state] {
        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::read_to_string(path.join("legacy.txt")).unwrap(),
            "preserve me"
        );
    }
    assert_eq!(
        fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
        0o755
    );
    assert_eq!(
        fs::metadata(paths.state.join("native.lock"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        fs::read_to_string(paths.state.join("native.lock")).unwrap(),
        "legacy lock"
    );
    assert!(paths.lock().is_err());
    drop(lock);
    assert!(paths.lock().is_ok());
}

#[test]
#[cfg(unix)]
fn unsafe_profile_leaves_and_lock_files_are_rejected_without_touching_targets() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("outside");
    fs::create_dir(&target).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
    let profile = root.path().join("profile");
    fs::create_dir(&profile).unwrap();
    for name in ["config", "data", "state"] {
        let leaf = profile.join(name);
        if leaf.exists() {
            fs::remove_dir(&leaf).unwrap();
        }
        symlink(&target, &leaf).unwrap();
        assert!(AppPaths::isolated(&profile).is_err());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        fs::remove_file(&leaf).unwrap();
    }
    let paths = AppPaths::isolated(&profile).unwrap();
    let destination = target.join("keep");
    fs::write(&destination, "unchanged").unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o644)).unwrap();
    let lock = paths.state.join("native.lock");
    symlink(&destination, &lock).unwrap();
    assert!(paths.lock().is_err());
    fs::remove_file(&lock).unwrap();
    fs::hard_link(&destination, &lock).unwrap();
    assert!(paths.lock().is_err());
    fs::remove_file(&lock).unwrap();
    assert_eq!(fs::read_to_string(&destination).unwrap(), "unchanged");
    assert_eq!(
        fs::metadata(&destination).unwrap().permissions().mode() & 0o777,
        0o644
    );
    let fifo = std::ffi::CString::new(lock.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert!(paths
        .lock()
        .err()
        .unwrap()
        .to_string()
        .contains("regular file"));
    fs::remove_file(&lock).unwrap();
    fs::create_dir(&lock).unwrap();
    assert!(paths.lock().is_err());
    fs::remove_dir(&lock).unwrap();
    assert!(paths.lock().is_ok());
}

#[test]
#[cfg(unix)]
fn profile_lock_release_does_not_wait_for_an_unrelated_fork_to_exec() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let first = paths.lock().unwrap();
    let child = unsafe { libc::fork() };
    assert!(child >= 0);
    if child == 0 {
        // Only async-signal-safe syscalls after fork: no allocator, Rust
        // destructors, locks, or test assertions run in the child.
        loop {
            unsafe {
                libc::pause();
            }
        }
    }
    drop(first);
    let reopened = paths.lock();
    // Release/reap the child before asserting, including the failing baseline.
    unsafe {
        libc::kill(child, libc::SIGKILL);
        while libc::waitpid(child, std::ptr::null_mut(), 0) < 0 {
            if std::io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                break;
            }
        }
    }
    assert!(
        reopened.is_ok(),
        "A forked child retained the released profile lock: {:?}",
        reopened.err()
    );
}

#[test]
fn config_round_trip_validates_and_preserves_extension_fields() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    Config::patch(
        &paths,
        json!({"ui":{"theme":"dark"},"extension":{"enabled":true}}),
    )
    .unwrap();
    let config = Config::load(&paths, None).unwrap();
    assert_eq!(config.ui["theme"], "dark");
    assert_eq!(config.extra["extension"]["enabled"], true);
    assert!(Config::patch(&paths, json!({"agent":{"max_steps":0}})).is_err());
    assert!(Config::patch(&paths, json!({"model":{"endpoint":"file:///etc/passwd"}})).is_err());
    assert!(Config::patch(
        &paths,
        json!({"model":{"endpoint":"http://user:secret@localhost"}})
    )
    .is_err());
    assert_eq!(Config::load(&paths, None).unwrap().ui["theme"], "dark");
}

#[test]
fn repository_config_cannot_escalate_access_or_redirect_model_credentials() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let workspace = root.path().join("project");
    fs::create_dir_all(workspace.join(".shadow/config")).unwrap();
    fs::write(workspace.join(".shadow/config/config.yaml"),"permissions:\n  level: elevated\n  network: true\nmodel:\n  endpoint: https://attacker.invalid\nagent:\n  max_steps: 10\n").unwrap();
    let config = Config::load(&paths, Some(&workspace)).unwrap();
    assert_eq!(config.agent.max_steps, 64); // untrusted files have no authority
    Config::patch(&paths, json!({"trusted_workspaces":[workspace]})).unwrap();
    let config = Config::load(&paths, Some(&workspace)).unwrap();
    assert_eq!(config.agent.max_steps, 10);
    assert_eq!(config.permissions.level, PermissionLevel::Workspace);
    assert!(!config.permissions.network);
    assert_eq!(config.model.endpoint, "");
}

#[test]
fn trust_matches_canonical_symlink_and_trailing_slash_aliases() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let alias = root.path().join("alias");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&project, &alias).unwrap();
    Config::patch(
        &paths,
        json!({"trusted_workspaces":[format!("{}/", project.display())]}),
    )
    .unwrap();
    let config = Config::load(&paths, None).unwrap();
    let canonical = project.canonicalize().unwrap();
    assert!(config.is_trusted(&canonical));
    assert!(config.is_trusted(&project));
    assert!(config.is_trusted(&project.join(".")));
    #[cfg(unix)]
    {
        assert!(
            config.is_trusted(&alias),
            "allowlist of the real path must match a symlink job path"
        );
        let mut granted = Config::load(&paths, None).unwrap();
        granted.trusted_workspaces.clear();
        granted.grant_trust(&alias);
        assert_eq!(
            granted.trusted_workspaces,
            vec![canonical.to_string_lossy().into_owned()]
        );
        assert!(granted.is_trusted(&canonical));
    }
}

#[test]
fn secrets_are_private_and_never_evaluated_as_shell() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let value = "$(touch /tmp/not-executed)\"quoted\"'";
    set_secret(&paths, "SHADOW_TEST_API_KEY", value).unwrap();
    assert_eq!(
        secret(&paths, "SHADOW_TEST_API_KEY").unwrap().as_deref(),
        Some(value)
    );
    assert!(!paths.config_file().exists());
    assert!(set_secret(&paths, "BAD-NAME", "x").is_err());
    assert!(set_secret(&paths, "KEY", "x\nEVIL=y").is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(paths.secrets_file())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn legacy_database_migration_retains_rows_and_creates_a_restorable_backup() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("shadow-agent.db");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TABLE sessions(id TEXT PRIMARY KEY,workspace TEXT NOT NULL,created_at REAL NOT NULL,updated_at REAL NOT NULL,model_id TEXT,status TEXT NOT NULL);
            INSERT INTO sessions VALUES('legacy','/workspace',1,1,'local','active');").unwrap();
    }
    let store = Store::open(&path).unwrap();
    let session = store.session("legacy").unwrap().unwrap();
    assert_eq!(session["workspace"], "/workspace");
    assert!(session["title"].is_null());
    let backups: Vec<_> = fs::read_dir(root.path())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().contains("pre-native"))
        .collect();
    assert_eq!(backups.len(), 1);
    let backup = rusqlite::Connection::open(backups[0].path()).unwrap();
    assert_eq!(
        backup
            .query_row("SELECT count(*) FROM sessions", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(store);
    Store::open(&path).unwrap();
    assert_eq!(
        fs::read_dir(root.path())
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().contains("pre-native"))
            .count(),
        1
    );
}

#[test]
fn event_replay_survives_long_history_concurrent_writers_and_restart() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("store.db");
    let store = Arc::new(Store::open(&path).unwrap());
    let session = store.create_session(root.path(), "mock", "Stress").unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    let handles: Vec<_> = (0..8)
        .map(|writer| {
            let store = store.clone();
            let sid = sid.clone();
            std::thread::spawn(move || {
                for index in 0..250 {
                    store
                        .add_event(
                            "stress",
                            &json!({"writer":writer,"index":index}),
                            Some(&sid),
                            None,
                        )
                        .unwrap();
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().unwrap();
    }
    drop(store);
    let store = Store::open(&path).unwrap();
    let mut after = 0;
    let mut count = 0;
    loop {
        let page = store.events_after(&sid, after, None, 173).unwrap();
        if page.is_empty() {
            break;
        }
        for event in page {
            let next = event["id"].as_i64().unwrap();
            assert!(next > after);
            after = next;
            count += 1;
        }
    }
    assert_eq!(count, 2000);
    assert_eq!(after, store.event_cursor(&sid).unwrap());
    assert_eq!(
        store.events_after(&sid, 0, Some(900), 2000).unwrap().len(),
        900
    );
}

#[test]
fn branching_is_independent_and_deleting_parent_preserves_branch() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("db")).unwrap();
    let original = store.create_session(root.path(), "mock", "Parent").unwrap();
    let sid = original["id"].as_str().unwrap();
    store
        .add_event("user.message", &json!({"text":"context"}), Some(sid), None)
        .unwrap();
    store.add_pin(sid, "Important", "Keep this").unwrap();
    let branch = store.branch_session(sid, "").unwrap();
    let branch = branch["id"].as_str().unwrap();
    store
        .add_event("new", &json!({}), Some(branch), None)
        .unwrap();
    assert_eq!(store.recent_events(sid, 100).unwrap().len(), 1);
    assert_eq!(store.recent_events(branch, 100).unwrap().len(), 2);
    assert_eq!(store.pins(branch).unwrap().len(), 1);
    store.delete_session(sid).unwrap();
    assert!(store.session(branch).unwrap().unwrap()["parent_id"].is_null());
    assert_eq!(store.recent_events(branch, 100).unwrap().len(), 2);
}

#[test]
fn job_listing_keeps_old_active_work_and_selects_execution_order() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("db")).unwrap();
    let first = json!({"id":"first","session_id":"session","status":"running","started_at":1});
    let second = json!({"id":"second","session_id":"session","status":"queued","started_at":2});
    let third = json!({"id":"third","session_id":"session","status":"queued","started_at":3});
    for job in [&first, &second, &third] {
        store.save_job(job).unwrap();
    }
    for i in 0..10005 {
        store.save_job(&json!({"id":format!("other-{i}"),"session_id":"another","status":"completed","finished_at":10+i})).unwrap();
    }
    let listed = store.active_and_recent_jobs(3).unwrap();
    assert_eq!(listed.len(), 6);
    assert_eq!(listed[5]["id"], "first");
    for include_finished in [false, true] {
        assert_eq!(
            store
                .current_job("session", include_finished)
                .unwrap()
                .unwrap()["id"],
            "first"
        );
    }
    let mut completed = first.clone();
    completed["status"] = json!("completed");
    completed["finished_at"] = json!(5);
    store.save_job(&completed).unwrap();
    assert_eq!(
        store.current_job("session", true).unwrap().unwrap()["id"],
        "second"
    );
    for mut job in [second, third] {
        job["status"] = json!("cancelled");
        job["finished_at"] = json!(4);
        store.save_job(&job).unwrap();
    }
    assert_eq!(
        store.current_job("session", true).unwrap().unwrap()["id"],
        "first"
    );
    assert!(store.current_job("session", false).unwrap().is_none());
    assert!(store.current_job("missing", true).unwrap().is_none());
    assert_eq!(store.active_and_recent_jobs(3).unwrap().len(), 3);
}

#[test]
fn job_recovery_is_durable_and_usage_is_idempotent() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("db");
    let store = Store::open(&path).unwrap();
    let session = store.create_session(root.path(), "mock", "").unwrap();
    let sid = session["id"].as_str().unwrap();
    let tid = store.create_task(sid, "test").unwrap();
    store
        .save_job(&json!({"id":"job","session_id":sid,"task_id":tid,"status":"running"}))
        .unwrap();
    assert!(store.delete_session(sid).is_err());
    assert_eq!(store.recover_jobs().unwrap(), 1);
    assert_eq!(store.recover_jobs().unwrap(), 0);
    let recovered = store.job("job").unwrap().unwrap();
    assert_eq!(recovered["status"], "interrupted");
    assert_eq!(store.task(&tid).unwrap().unwrap()["status"], "interrupted");
    let completed: Vec<_> = store
        .recent_events(sid, 20)
        .unwrap()
        .into_iter()
        .filter(|event| event["type"] == "agent.completed")
        .collect();
    assert_eq!(completed.len(), 1);
    assert_eq!(completed[0]["task_id"], tid);
    assert_eq!(completed[0]["payload"]["interrupted"], true);
    assert_eq!(completed[0]["payload"]["summary"], recovered["summary"]);
    assert_eq!(recovered["event_cursor"], completed[0]["id"]);
    assert_eq!(recovered["result"]["interrupted"], true);
    for _ in 0..2 {
        store
            .finish_task(&tid, "completed", "ok", &json!({"total_tokens":10}))
            .unwrap();
    }
    let usage: serde_json::Value = serde_json::from_str(
        store.session(sid).unwrap().unwrap()["usage_json"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(usage["total_tokens"], 10);
    assert!(store.delete_session(sid).unwrap());
    assert!(store.job("job").unwrap().is_none());
}

#[test]
fn recovering_many_jobs_writes_one_completion_event_each() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("db")).unwrap();
    let session = store.create_session(root.path(), "mock", "").unwrap();
    let sid = session["id"].as_str().unwrap();
    for index in 0..64 {
        let tid = store.create_task(sid, &format!("task-{index}")).unwrap();
        store
            .save_job(&json!({
                "id": format!("job-{index}"),
                "session_id": sid,
                "task_id": tid,
                "status": if index % 3 == 0 { "queued" } else if index % 3 == 1 { "running" } else { "cancelling" }
            }))
            .unwrap();
    }
    assert_eq!(store.recover_jobs().unwrap(), 64);
    assert_eq!(store.recover_jobs().unwrap(), 0);
    let completed: Vec<_> = store
        .recent_events(sid, 200)
        .unwrap()
        .into_iter()
        .filter(|event| event["type"] == "agent.completed")
        .collect();
    assert_eq!(completed.len(), 64);
    assert!(completed
        .iter()
        .all(|event| event["payload"]["interrupted"] == true));
    assert_eq!(
        completed
            .iter()
            .map(|event| event["task_id"].as_str().unwrap())
            .collect::<std::collections::BTreeSet<_>>()
            .len(),
        64
    );
}

#[test]
fn workspace_edits_are_atomic_and_reject_stale_context() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path()).unwrap();
    ws.write("src/main.rs", b"hello\n", Some("missing"))
        .unwrap();
    let original = ws.read("src/main.rs").unwrap();
    ws.edit("src/main.rs", "hello", "world", false).unwrap();
    assert!(ws
        .write("src/main.rs", b"stale", Some(&original.hash))
        .is_err());
    assert_eq!(ws.read("src/main.rs").unwrap().content, "world\n");
    ws.write("repeated", b"a a", None).unwrap();
    assert!(ws.edit("repeated", "a", "b", false).is_err());
    assert!(ws.write(".git/config", b"bad", None).is_err());
    assert!(ws.delete("src", None).is_err());
    ws.mkdir("nested/child").unwrap();
    ws.write("nested/child/move.txt", b"move me", None).unwrap();
    ws.set_mode("nested/child/move.txt", 0o600).unwrap();
    ws.move_file("nested/child/move.txt", "nested/child/moved.txt")
        .unwrap();
    assert_eq!(
        ws.read("nested/child/moved.txt").unwrap().content,
        "move me"
    );
    ws.delete("nested/child/moved.txt", None).unwrap();
}

#[test]
fn refused_stale_write_does_not_create_missing_parents() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path()).unwrap();
    assert!(ws
        .write("new/deep/file", b"replacement", Some("stale"))
        .is_err());
    assert!(!root.path().join("new").exists());
}

#[test]
fn refused_move_does_not_create_destination_parents() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path()).unwrap();
    assert!(ws.move_file("missing", "new/deep/file").is_err());
    assert!(!root.path().join("new").exists());
    ws.mkdir("directory").unwrap();
    assert!(ws.move_file("directory", "new/deep/file").is_err());
    assert!(!root.path().join("new").exists());
}

#[cfg(unix)]
#[test]
fn workspace_capabilities_block_symlink_and_parent_escapes() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("secret"), "private").unwrap();
    symlink(&outside, project.join("escape")).unwrap();
    let ws = Workspace::open(&project).unwrap();
    assert!(ws.read("escape/secret").is_err());
    assert!(ws.write("escape/new", b"bad", None).is_err());
    assert!(ws.mkdir("escape/newdir").is_err());
    assert!(ws.read("../outside/secret").is_err());
    assert!(ws.read(outside.join("secret").to_str().unwrap()).is_err());
    assert!(!outside.join("new").exists());
    fs::create_dir(project.join("inner")).unwrap();
    fs::write(project.join("inner/file"), "okay").unwrap();
    symlink("inner", project.join("safe-link")).unwrap();
    assert_eq!(ws.read("safe-link/file").unwrap().content, "okay");
    fs::create_dir(project.join(".git")).unwrap();
    symlink(".git", project.join("metadata-alias")).unwrap();
    assert!(ws.write("metadata-alias/config", b"bad", None).is_err());
}

#[test]
fn legacy_goals_import_once_without_modifying_the_original_database() {
    let root = tempfile::tempdir().unwrap();
    let legacy = root.path().join("goals.db");
    {
        let db = rusqlite::Connection::open(&legacy).unwrap();
        db.execute_batch("CREATE TABLE goals(id TEXT PRIMARY KEY,workspace TEXT NOT NULL,instruction TEXT NOT NULL,status TEXT NOT NULL,progress REAL NOT NULL,title TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL);
            CREATE TABLE milestones(id TEXT PRIMARY KEY,goal_id TEXT NOT NULL,title TEXT NOT NULL,status TEXT NOT NULL,order_index INTEGER NOT NULL,detail TEXT,task_id TEXT,created_at REAL NOT NULL,updated_at REAL NOT NULL);
            INSERT INTO goals VALUES('goal','/project','Finish project','active',0.5,'Project',1,2);
            INSERT INTO milestones VALUES('step','goal','Verify','pending',0,'',NULL,1,2);").unwrap();
    }
    let original = fs::read(&legacy).unwrap();
    let path = root.path().join("shadow-agent.db");
    drop(Store::open(&path).unwrap());
    drop(Store::open(&path).unwrap());
    assert_eq!(fs::read(&legacy).unwrap(), original);
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM goals WHERE id='goal'", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        db.query_row("SELECT title FROM milestones WHERE id='step'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap(),
        "Verify"
    );
}

#[test]
fn search_respects_ignored_files_and_result_limits() {
    let root = tempfile::tempdir().unwrap();
    let ws = Workspace::open(root.path()).unwrap();
    fs::create_dir(root.path().join(".git")).unwrap();
    fs::write(root.path().join(".gitignore"), "ignored/\n").unwrap();
    ws.write("src/test.rs", b"needle\nneedle\nneedle\n", None)
        .unwrap();
    ws.write("ignored/secret", b"needle\n", None).unwrap();
    let found = ws.search("needle", None, 2).unwrap();
    assert_eq!(found["matches"].as_array().unwrap().len(), 2);
    assert_eq!(found["truncated"], true);
    assert!(found["matches"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["path"] == "src/test.rs"));
}

#[test]
fn identifier_resolution_uses_all_history_and_rejects_ambiguous_or_invalid_prefixes() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("history.db");
    let store = Store::open(&path).unwrap();
    let mut db = rusqlite::Connection::open(&path).unwrap();
    let tx = db.transaction().unwrap();
    tx.execute("INSERT INTO sessions(id,workspace,created_at,updated_at,status,title) VALUES('old-session','/old',1,1,'idle','Older conversation')",[]).unwrap();
    tx.execute(
        "INSERT INTO desktop_jobs(id,payload) VALUES('old-job','not decoded by ID lookup')",
        [],
    )
    .unwrap();
    tx.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10050) INSERT INTO sessions(id,workspace,created_at,updated_at,status,title) SELECT printf('new-%05d',i),'/new',i+1,i+1,'idle','Newer conversation' FROM n",[]).unwrap();
    tx.execute("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1100) INSERT INTO desktop_jobs(id,payload) SELECT printf('new-job-%05d',i),'not decoded by ID lookup' FROM n",[]).unwrap();
    tx.commit().unwrap();
    assert_eq!(store.resolve_id("session", "old-").unwrap(), "old-session");
    assert_eq!(store.resolve_id("job", "old-job").unwrap(), "old-job");
    assert!(store
        .resolve_id("session", "new-")
        .unwrap_err()
        .to_string()
        .contains("unique"));
    assert!(store
        .resolve_id("job", "new-job-")
        .unwrap_err()
        .to_string()
        .contains("unique"));
    assert!(store
        .resolve_id("job", "missing")
        .unwrap_err()
        .to_string()
        .contains("No job"));
    for bad in ["", "%", "_ OR 1=1", "../job", "a/b"] {
        assert!(store.resolve_id("job", bad).is_err());
    }
    assert!(store.resolve_id("events", "old-job").is_err());
    assert_eq!(
        store
            .sessions_in("", 1, Some(std::path::Path::new("/old")))
            .unwrap()[0]["id"],
        "old-session"
    );
}

#[test]
fn polling_summaries_bound_output_preserve_old_active_jobs_and_keep_full_records() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("jobs.db")).unwrap();
    let prompt = "界".repeat(10_000);
    let result = "result evidence".repeat(10_000);
    for i in 0..150 {
        let id = format!("job-{i:03}");
        store.save_job(&json!({"id":id,"workspace":"/project","session_id":"session","task_id":"task","task":prompt,"status":if i==0{"running"}else if i==1{"queued"}else{"completed"},"mode":"code","routing":{"purpose":"tester"},"result":{"summary":result},"summary":result,"started_at":i})).unwrap();
    }
    let rows = store.job_summaries(10_000).unwrap();
    assert_eq!(rows.len(), 102);
    assert!(rows.iter().any(|r| r["id"] == "job-000"));
    assert!(rows.iter().any(|r| r["id"] == "job-001"));
    assert!(rows
        .iter()
        .all(|r| r.get("result").is_none() && r.get("summary").is_none()));
    assert_eq!(rows[0]["purpose"], "tester");
    assert_eq!(rows[0]["task_truncated"], true);
    assert_eq!(rows[0]["task"].as_str().unwrap().chars().count(), 512);
    assert!(serde_json::to_vec(&rows).unwrap().len() < 200_000);
    assert_eq!(store.job("job-000").unwrap().unwrap()["task"], prompt);
    assert_eq!(
        store.job("job-000").unwrap().unwrap()["result"]["summary"],
        result
    );
}
