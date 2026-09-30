//! Settings › Your data: backups, restore, repair and reset, including
//! damaged and incomplete backups, and the `shadowcode backup|restore|reset|
//! doctor --repair` commands.
use serde_json::{json, Value};
use shadowcode_core::{
    config::{self, Config},
    data::{self, BackupOptions, Manifest, RestoreOptions},
    engine::Engine,
    paths::AppPaths,
    service::{Request, Service},
    store::{Store, SCHEMA_VERSION},
};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn manual() -> BackupOptions<'static> {
    BackupOptions {
        include_secrets: false,
        folder: None,
        reason: "manual",
        allow_raw: false,
    }
}

struct Profile {
    _temp: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    paths: AppPaths,
}

fn profile() -> Profile {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    let paths = AppPaths::isolated(&root.join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({"ui": {"theme": "dark"}, "trusted_workspaces": [project]}),
    )
    .unwrap();
    config::set_secret(&paths, "OPENAI_API_KEY", "first-value").unwrap();
    Profile {
        _temp: temp,
        root,
        project,
        paths,
    }
}

fn titles(paths: &AppPaths) -> Vec<String> {
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    let mut rows: Vec<String> = db
        .prepare("SELECT coalesce(title,'') FROM sessions")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    rows.sort();
    rows
}

fn backups(paths: &AppPaths) -> Vec<data::Listed> {
    data::list_backups(&data::backups_dir(paths))
}

fn rewrite_manifest(folder: &Path, edit: impl FnOnce(&mut Manifest)) {
    let path = folder.join("manifest.json");
    let mut manifest: Manifest = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    edit(&mut manifest);
    fs::write(path, serde_json::to_string_pretty(&manifest).unwrap()).unwrap();
}

fn digest(path: &Path) -> (u64, String) {
    use sha2::{Digest, Sha256};
    let bytes = fs::read(path).unwrap();
    (bytes.len() as u64, format!("{:x}", Sha256::digest(&bytes)))
}

/// Refresh the manifest entry of `relative` after changing the file.
fn reseal(folder: &Path, relative: &str) {
    let (bytes, sha256) = digest(&folder.join(relative));
    rewrite_manifest(folder, |m| {
        let file = m.files.iter_mut().find(|f| f.path == relative).unwrap();
        file.bytes = bytes;
        file.sha256 = sha256;
    });
}

#[cfg(unix)]
fn mode(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[tokio::test(flavor = "multi_thread")]
async fn a_backup_is_a_consistent_private_copy_without_secrets_unless_asked() {
    let p = profile();
    let agents = shadowcode_core::agents::user_dir(&p.paths);
    fs::create_dir_all(&agents).unwrap();
    fs::write(agents.join("helper.md"), "---\nname: helper\n---\nHelp.\n").unwrap();
    let engine = Engine::open(p.paths.clone()).unwrap();
    let store = engine.store();
    store.create_session(&p.project, "mock", "Kept").unwrap();
    // Taken while the engine runs and holds the database open.
    let (folder, manifest) = data::create_backup(&p.paths, manual()).unwrap();
    assert!(folder.starts_with(data::backups_dir(&p.paths)));
    assert!(folder
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("shadowcode-backup-"));
    let listed: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    assert!(listed.contains(&"state/shadow-agent.db"), "{listed:?}");
    assert!(listed.contains(&"config/config.yaml"));
    assert!(listed.contains(&"agents/helper.md"));
    assert!(!listed.iter().any(|f| f.contains("secrets")), "{listed:?}");
    assert!(!manifest.includes_secrets);
    assert_eq!(manifest.schema_version, SCHEMA_VERSION);
    assert_eq!(manifest.app_version, shadowcode_core::VERSION);
    let copy = rusqlite::Connection::open(folder.join("state/shadow-agent.db")).unwrap();
    let version: i64 = copy
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    let count: i64 = copy
        .query_row(
            "SELECT count(*) FROM sessions WHERE title='Kept'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 1);
    drop(copy);
    for file in &manifest.files {
        assert_eq!(digest(&folder.join(&file.path)).1, file.sha256);
    }
    #[cfg(unix)]
    {
        assert_eq!(mode(&folder), 0o700);
        assert_eq!(mode(&folder.join("state/shadow-agent.db")), 0o600);
        assert_eq!(mode(&folder.join("manifest.json")), 0o600);
    }
    let inspection = data::inspect(&p.paths, &folder).unwrap();
    assert!(inspection.restorable, "{:?}", inspection.problems);
    assert_eq!(inspection.summary["conversations"], 1);

    // Secrets only when asked, into a folder the user chose.
    let chosen = p.root.join("usb");
    fs::create_dir_all(&chosen).unwrap();
    let (with_secrets, manifest) = data::create_backup(
        &p.paths,
        BackupOptions {
            include_secrets: true,
            folder: Some(&chosen),
            ..manual()
        },
    )
    .unwrap();
    assert!(with_secrets.starts_with(&chosen));
    assert!(manifest.includes_secrets);
    assert!(fs::read_to_string(with_secrets.join("config/secrets.env"))
        .unwrap()
        .contains("first-value"));
    assert!(data::create_backup(
        &p.paths,
        BackupOptions {
            folder: Some(Path::new("relative/folder")),
            ..manual()
        }
    )
    .is_err());
    // Two backups in the same second get distinct folders; the list is
    // newest first and reads the manifests.
    let (again, _) = data::create_backup(&p.paths, manual()).unwrap();
    assert_ne!(again, folder);
    let list = backups(&p.paths);
    assert_eq!(list.len(), 2);
    assert!(list[0].created_at >= list[1].created_at);
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn incomplete_damaged_and_foreign_backups_are_refused_with_the_reason() {
    let p = profile();
    {
        let store = Store::open(&p.paths.database()).unwrap();
        for i in 0..200 {
            store
                .create_session(&p.project, "mock", &format!("Conversation {i}"))
                .unwrap();
        }
    }
    let fresh = || data::create_backup(&p.paths, manual()).unwrap().0;
    let refused = |folder: &Path, expect: &str| {
        let inspection = data::inspect(&p.paths, folder).unwrap();
        assert!(!inspection.restorable, "{expect}");
        assert!(
            inspection.problems.iter().any(|m| m.contains(expect)),
            "{expect}: {:?}",
            inspection.problems
        );
        let error =
            data::schedule_restore(&p.paths, folder, RestoreOptions::default()).unwrap_err();
        assert!(format!("{error:#}").contains(expect), "{error:#}");
        assert!(data::pending(&p.paths).unwrap().is_none());
    };

    // A file is missing (a partial copy).
    let partial = fresh();
    fs::remove_file(partial.join("config/config.yaml")).unwrap();
    refused(&partial, "config/config.yaml is missing");
    // A file was cut short or changed.
    let truncated = fresh();
    let db = truncated.join("state/shadow-agent.db");
    let bytes = fs::read(&db).unwrap();
    fs::write(&db, &bytes[..bytes.len() / 2]).unwrap();
    refused(&truncated, "changed or is damaged");
    // Damaged database pages with a matching manifest.
    let damaged = fresh();
    let db = damaged.join("state/shadow-agent.db");
    let mut bytes = fs::read(&db).unwrap();
    assert!(bytes.len() > 16 * 4096, "{}", bytes.len());
    for byte in &mut bytes[4096 * 2..4096 * 8] {
        *byte = 0x5a;
    }
    fs::write(&db, &bytes).unwrap();
    reseal(&damaged, "state/shadow-agent.db");
    refused(&damaged, "damaged");
    // A newer database format.
    let newer = fresh();
    let db = newer.join("state/shadow-agent.db");
    rusqlite::Connection::open(&db)
        .unwrap()
        .pragma_update(None, "user_version", SCHEMA_VERSION + 1)
        .unwrap();
    reseal(&newer, "state/shadow-agent.db");
    refused(&newer, "newer ShadowCode");
    // A newer backup format.
    let format = fresh();
    rewrite_manifest(&format, |m| m.format_version = 99);
    refused(&format, "newer ShadowCode");
    // A manifest pointing outside the profile.
    let escaping = fresh();
    fs::write(escaping.join("evil"), "x").unwrap();
    rewrite_manifest(&escaping, |m| {
        m.files.push(data::FileEntry {
            path: "config/../../../evil".into(),
            bytes: 1,
            sha256: String::new(),
        })
    });
    refused(&escaping, "outside the profile");
    // Settings this version cannot load.
    let settings = fresh();
    fs::write(
        settings.join("config/config.yaml"),
        "agent:\n  max_steps: 0\n",
    )
    .unwrap();
    reseal(&settings, "config/config.yaml");
    refused(&settings, "do not load");
    // No database at all.
    let empty = fresh();
    rewrite_manifest(&empty, |m| m.files.retain(|f| !f.path.ends_with(".db")));
    refused(&empty, "no database");

    // Not a backup: errors, not problems.
    let missing = fresh();
    fs::remove_file(missing.join("manifest.json")).unwrap();
    let error = format!("{:#}", data::inspect(&p.paths, &missing).unwrap_err());
    assert!(error.contains("has no manifest.json"), "{error}");
    let garbled = fresh();
    fs::write(garbled.join("manifest.json"), "{not json").unwrap();
    let error = format!("{:#}", data::inspect(&p.paths, &garbled).unwrap_err());
    assert!(error.contains("damaged"), "{error}");
    let text = p.root.join("notes.txt");
    fs::write(&text, "hello").unwrap();
    assert!(data::inspect(&p.paths, &text).is_err());
    assert!(data::inspect(&p.paths, Path::new("relative")).is_err());
    assert!(data::inspect(&p.paths, &p.root.join("nowhere")).is_err());
    // The manifest file itself names its folder.
    let good = fresh();
    assert!(
        data::inspect(&p.paths, &good.join("manifest.json"))
            .unwrap()
            .restorable
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scheduled_restore_finishes_at_the_next_start_after_backing_up_the_current_data() {
    let p = profile();
    let engine = Engine::open(p.paths.clone()).unwrap();
    engine
        .store()
        .create_session(&p.project, "mock", "Before")
        .unwrap();
    let (folder, _) = data::create_backup(
        &p.paths,
        BackupOptions {
            include_secrets: true,
            ..manual()
        },
    )
    .unwrap();
    // Work continues after the backup.
    engine
        .store()
        .create_session(&p.project, "mock", "After")
        .unwrap();
    Config::patch(&p.paths, json!({"ui": {"theme": "light"}})).unwrap();
    config::set_secret(&p.paths, "OPENAI_API_KEY", "second-value").unwrap();

    // Scheduling changes nothing yet, and can be cancelled.
    let pending = data::schedule_restore(&p.paths, &folder, RestoreOptions::default()).unwrap();
    assert_eq!(pending.kind, "restore");
    assert!(!pending.include_secrets);
    assert_eq!(titles(&p.paths), ["After", "Before"]);
    assert!(data::cancel_pending(&p.paths).unwrap());
    assert!(data::pending(&p.paths).unwrap().is_none());
    assert!(!p.paths.state.join("pending-restore").exists());
    data::schedule_restore(&p.paths, &folder, RestoreOptions::default()).unwrap();
    // The backup may go away before the restart; the staged copy is used.
    let moved = p.root.join("moved-backup");
    fs::rename(&folder, &moved).unwrap();
    engine.shutdown().await.unwrap();
    drop(engine);

    let engine = Engine::open(p.paths.clone()).unwrap();
    assert!(data::pending(&p.paths).unwrap().is_none());
    assert!(!p.paths.state.join("pending-restore").exists());
    assert_eq!(titles(&p.paths), ["Before"]);
    assert_eq!(
        Config::load(&p.paths, None).unwrap().ui["theme"],
        "dark",
        "settings restored"
    );
    assert_eq!(
        config::secret(&p.paths, "OPENAI_API_KEY")
            .unwrap()
            .as_deref(),
        Some("second-value"),
        "API keys stay unless asked for"
    );
    let last = data::last_operation(&p.paths).unwrap();
    assert_eq!(last["kind"], "restore");
    assert_eq!(last["ok"], true, "{last}");
    // What was replaced is in a before-restore backup.
    let previous = PathBuf::from(last["backup_of_previous_data"].as_str().unwrap());
    let inspection = data::inspect(&p.paths, &previous).unwrap();
    assert!(inspection.restorable, "{:?}", inspection.problems);
    assert_eq!(inspection.manifest.reason, "before-restore");
    assert_eq!(inspection.summary["conversations"], 2);
    engine.shutdown().await.unwrap();
    drop(engine);

    // Restoring API keys too, from the same backup.
    data::schedule_restore(
        &p.paths,
        &moved,
        RestoreOptions {
            include_secrets: true,
            ..RestoreOptions::default()
        },
    )
    .unwrap();
    let engine = Engine::open(p.paths.clone()).unwrap();
    assert_eq!(
        config::secret(&p.paths, "OPENAI_API_KEY")
            .unwrap()
            .as_deref(),
        Some("first-value")
    );
    let last = data::last_operation(&p.paths).unwrap();
    assert_eq!(last["secrets_restored"], true);
    // The before-restore backup kept the key it replaced.
    let previous = PathBuf::from(last["backup_of_previous_data"].as_str().unwrap());
    assert!(fs::read_to_string(previous.join("config/secrets.env"))
        .unwrap()
        .contains("second-value"));
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_staged_restore_that_changed_is_refused_and_recorded() {
    let p = profile();
    {
        let store = Store::open(&p.paths.database()).unwrap();
        store.create_session(&p.project, "mock", "Current").unwrap();
    }
    let (folder, _) = data::create_backup(&p.paths, manual()).unwrap();
    {
        let store = Store::open(&p.paths.database()).unwrap();
        store.create_session(&p.project, "mock", "Newer").unwrap();
    }
    data::schedule_restore(&p.paths, &folder, RestoreOptions::default()).unwrap();
    fs::write(
        p.paths.state.join("pending-restore/config/config.yaml"),
        "ui: {theme: light}\n",
    )
    .unwrap();
    let engine = Engine::open(p.paths.clone()).unwrap();
    // Nothing was replaced, the marker is gone and the failure is recorded.
    assert_eq!(titles(&p.paths), ["Current", "Newer"]);
    assert!(data::pending(&p.paths).unwrap().is_none());
    let last = data::last_operation(&p.paths).unwrap();
    assert_eq!(last["ok"], false);
    assert!(
        last["error"].as_str().unwrap().contains("changed"),
        "{last}"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_upgrade_copy_restores_and_is_upgraded_again() {
    let p = profile();
    // A 0.28.0 database (format 25), as an automatic pre-upgrade copy is.
    let old = p.root.join("old.sqlite");
    {
        let sql = fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/upgrade/0.28.0/shadow-agent.sql"),
        )
        .unwrap()
        .replace("@ROOT@", &p.root.to_string_lossy());
        let db = rusqlite::Connection::open(&old).unwrap();
        db.pragma_update(None, "synchronous", "OFF").unwrap();
        db.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
            .unwrap();
    }
    let inspection = data::inspect(&p.paths, &old).unwrap();
    assert_eq!(inspection.kind, "database");
    assert!(inspection.restorable, "{:?}", inspection.problems);
    assert_eq!(inspection.manifest.schema_version, 25);
    data::schedule_restore(&p.paths, &old, RestoreOptions::default()).unwrap();
    let engine = Engine::open(p.paths.clone()).unwrap();
    assert!(
        titles(&p.paths).iter().any(|t| t == "Fixture conversation"),
        "{:?}",
        titles(&p.paths)
    );
    let version: i64 = rusqlite::Connection::open(p.paths.database())
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, SCHEMA_VERSION);
    // Settings are untouched by a database-only restore.
    assert_eq!(Config::load(&p.paths, None).unwrap().ui["theme"], "dark");
    engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_moves_everything_aside_and_keeps_backups_worktrees_and_downloads() {
    let p = profile();
    {
        let store = Store::open(&p.paths.database()).unwrap();
        store
            .create_session(&p.project, "mock", "Old life")
            .unwrap();
    }
    data::create_backup(&p.paths, manual()).unwrap();
    for kept in ["local-models", "managed-worktrees"] {
        fs::create_dir_all(p.paths.data.join(kept)).unwrap();
        fs::write(p.paths.data.join(kept).join("keep.bin"), "kept").unwrap();
    }
    fs::create_dir_all(p.paths.data.join("webview")).unwrap();
    fs::write(p.paths.data.join("webview/cache"), "moved").unwrap();
    fs::write(p.paths.state.join("ui.log"), "moved").unwrap();

    data::schedule_reset(&p.paths).unwrap();
    let engine = Engine::open(p.paths.clone()).unwrap();
    // A fresh start.
    assert!(titles(&p.paths).is_empty());
    assert!(!p.paths.secrets_file().exists());
    assert!(!p.paths.data.join("webview").exists());
    assert!(!p.paths.state.join("ui.log").exists());
    assert_eq!(Config::load(&p.paths, None).unwrap().ui["theme"], "light");
    // Kept in place.
    assert_eq!(backups(&p.paths).len(), 1);
    for kept in ["local-models", "managed-worktrees"] {
        assert!(p.paths.data.join(kept).join("keep.bin").is_file());
    }
    // Everything else moved aside, nothing deleted.
    let last = data::last_operation(&p.paths).unwrap();
    assert_eq!(last["kind"], "reset");
    assert_eq!(last["ok"], true);
    let moved: Vec<PathBuf> = last["moved_to"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| PathBuf::from(v.as_str().unwrap()))
        .collect();
    assert_eq!(moved.len(), 3, "{moved:?}");
    let config_aside = moved
        .iter()
        .find(|m| m.to_string_lossy().contains("config.reset-"))
        .unwrap();
    assert!(fs::read_to_string(config_aside.join("secrets.env"))
        .unwrap()
        .contains("first-value"));
    let state_aside = moved
        .iter()
        .find(|m| m.to_string_lossy().contains("state.reset-"))
        .unwrap();
    let old = rusqlite::Connection::open(state_aside.join("shadow-agent.db")).unwrap();
    let title: String = old
        .query_row("SELECT title FROM sessions", [], |r| r.get(0))
        .unwrap();
    assert_eq!(title, "Old life");
    assert!(state_aside.join("ui.log").is_file());
    let data_aside = moved
        .iter()
        .find(|m| m.to_string_lossy().contains("data.reset-"))
        .unwrap();
    assert!(data_aside.join("webview/cache").is_file());
    assert_eq!(data::reset_folders(&p.paths).len(), 3);
    engine.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn a_reset_that_cannot_finish_puts_everything_back() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    // XDG-like: the three folders live in different parents.
    let paths = AppPaths {
        config: root.join("xdg-config/shadow-agent"),
        data: root.join("xdg-data/shadow-agent"),
        state: root.join("xdg-state/shadow-agent"),
        cache: root.join("xdg-cache/shadow-agent"),
    };
    paths.ensure().unwrap();
    Config::patch(&paths, json!({"ui": {"theme": "dark"}})).unwrap();
    {
        let store = Store::open(&paths.database()).unwrap();
        store.create_session(&root, "mock", "Still here").unwrap();
    }
    fs::write(paths.data.join("history.txt"), "data").unwrap();
    data::schedule_reset(&paths).unwrap();
    // The state folder's parent cannot take the aside folder.
    let locked = root.join("xdg-state");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o500)).unwrap();
    let lock = paths.lock().unwrap();
    let error = data::apply_pending(&paths);
    drop(lock);
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    if nix_root() {
        return; // root ignores the read-only parent
    }
    let error = format!("{:#}", error.unwrap_err());
    assert!(error.contains("nothing was moved"), "{error}");
    assert_eq!(Config::load(&paths, None).unwrap().ui["theme"], "dark");
    assert!(paths.data.join("history.txt").is_file());
    assert_eq!(titles(&paths), ["Still here"]);
    assert!(data::reset_folders(&paths).is_empty());
    let last = data::last_operation(&paths).unwrap();
    assert_eq!(last["ok"], false);
}

#[cfg(unix)]
fn nix_root() -> bool {
    unsafe { libc::geteuid() == 0 }
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

#[tokio::test(flavor = "multi_thread")]
async fn the_data_routes_back_up_repair_and_schedule() {
    let p = profile();
    let service = Service::open(p.paths.clone(), Some(p.project.clone())).unwrap();
    service
        .engine
        .store()
        .create_session(&p.project, "mock", "Routed")
        .unwrap();
    let cache = p.paths.state.join("openrouter-models.json");
    fs::write(&cache, "{\"data\":[]}").unwrap();

    let overview = call(&service, "GET", "/api/data", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        overview["database"]["supported_schema_version"],
        SCHEMA_VERSION
    );
    assert_eq!(overview["database"]["schema_version"], SCHEMA_VERSION);
    assert!(overview["pending"].is_null());
    assert_eq!(overview["folders"]["config"], json!(p.paths.config));

    let created = call(&service, "POST", "/api/data/backups", json!({}))
        .await
        .unwrap();
    let folder = created["path"].as_str().unwrap().to_owned();
    assert_eq!(created["manifest"]["includes_secrets"], false);
    let listed = call(&service, "GET", "/api/data/backups", Value::Null)
        .await
        .unwrap();
    assert_eq!(listed["backups"].as_array().unwrap().len(), 1);
    let inspected = call(
        &service,
        "POST",
        "/api/data/backups/inspect",
        json!({"path": folder}),
    )
    .await
    .unwrap();
    assert_eq!(inspected["restorable"], true, "{inspected}");
    assert_eq!(inspected["summary"]["conversations"], 1);

    // Repair: backed up first, healthy, cache moved into that backup.
    let report = call(&service, "POST", "/api/data/repair", json!({}))
        .await
        .unwrap();
    assert_eq!(report["ok"], true, "{report}");
    for id in ["integrity", "references", "indexes", "journal", "caches"] {
        let check = report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == id)
            .unwrap_or_else(|| panic!("{id}: {report}"));
        assert!(
            matches!(check["status"].as_str(), Some("pass" | "warn")),
            "{check}"
        );
    }
    let backup = PathBuf::from(report["backup"].as_str().unwrap());
    assert!(backup.join("state/shadow-agent.db").is_file());
    assert!(!cache.exists());
    assert!(backup.join("caches/state/openrouter-models.json").is_file());
    assert_eq!(data::last_operation(&p.paths).unwrap()["kind"], "repair");
    // The engine keeps working after the repair.
    service
        .engine
        .store()
        .create_session(&p.project, "mock", "After repair")
        .unwrap();

    // Reset needs the explicit word.
    let error = call(&service, "POST", "/api/data/reset", json!({}))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("confirm"), "{error:#}");
    let scheduled = call(
        &service,
        "POST",
        "/api/data/reset",
        json!({"confirm": "reset"}),
    )
    .await
    .unwrap();
    assert_eq!(scheduled["pending"]["kind"], "reset");
    let overview = call(&service, "GET", "/api/data", Value::Null)
        .await
        .unwrap();
    assert_eq!(overview["pending"]["kind"], "reset");
    // Scheduling a restore replaces the scheduled reset; cancel clears it.
    call(
        &service,
        "POST",
        "/api/data/restore",
        json!({"path": folder}),
    )
    .await
    .unwrap();
    assert_eq!(data::pending(&p.paths).unwrap().unwrap().kind, "restore");
    let cancelled = call(&service, "DELETE", "/api/data/pending", Value::Null)
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], true);
    let cancelled = call(&service, "DELETE", "/api/data/pending", Value::Null)
        .await
        .unwrap();
    assert_eq!(cancelled["cancelled"], false);
    // A restore request for a damaged backup is refused.
    fs::remove_file(PathBuf::from(&folder).join("state/shadow-agent.db")).unwrap();
    let error = call(
        &service,
        "POST",
        "/api/data/restore",
        json!({"path": folder}),
    )
    .await
    .unwrap_err();
    assert!(format!("{error:#}").contains("missing"), "{error:#}");
    assert!(call(
        &service,
        "POST",
        "/api/data/restore",
        json!({"path": "relative/path"})
    )
    .await
    .is_err());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn repair_reports_a_damaged_database_without_dropping_data() {
    let p = profile();
    {
        let store = Store::open(&p.paths.database()).unwrap();
        for i in 0..300 {
            store
                .add_event(
                    "fixture",
                    &json!({"i": i, "text": "x".repeat(200)}),
                    None,
                    None,
                )
                .unwrap();
        }
        let db = rusqlite::Connection::open(p.paths.database()).unwrap();
        db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
            .unwrap();
    }
    // Damage pages of the events index in the middle of the file.
    let (root_page, page_size): (i64, i64) = {
        let db = rusqlite::Connection::open(p.paths.database()).unwrap();
        (
            db.query_row(
                "SELECT rootpage FROM sqlite_master WHERE name='events_session_id'",
                [],
                |r| r.get(0),
            )
            .unwrap(),
            db.query_row("PRAGMA page_size", [], |r| r.get(0)).unwrap(),
        )
    };
    let mut bytes = fs::read(p.paths.database()).unwrap();
    let start = ((root_page - 1) * page_size) as usize;
    for byte in &mut bytes[start + 8..start + page_size as usize] {
        *byte = 0xff;
    }
    fs::write(p.paths.database(), &bytes).unwrap();
    let store = Store::open(&p.paths.database()).unwrap();
    let report = data::repair(&p.paths, &store).unwrap();
    assert_eq!(report["ok"], false, "{report}");
    let integrity = &report["checks"][0];
    assert_eq!(integrity["status"], "fail");
    assert!(integrity["detail"]
        .as_str()
        .unwrap()
        .contains("Restore a backup"));
    let indexes = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "indexes")
        .unwrap();
    assert_eq!(indexes["status"], "not_checked");
    // The damaged database was still copied first, as it was.
    let backup = PathBuf::from(report["backup"].as_str().unwrap());
    assert!(backup.join("state/shadow-agent.db").is_file());
    let events: i64 = rusqlite::Connection::open(p.paths.database())
        .unwrap()
        .query_row("SELECT count(*) FROM events NOT INDEXED", [], |r| r.get(0))
        .unwrap();
    assert_eq!(events, 300, "no rows dropped");
}

// The command line: `target/debug/shadowcode` (build it first, as for the
// other process tests).
#[cfg(unix)]
fn shadowcode(profile: &Path, args: &[&str]) -> std::process::Output {
    let binary = std::env::var_os("SHADOW_DESKTOP_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/shadowcode")
        });
    let output = std::process::Command::new(binary)
        .arg("--profile")
        .arg(profile)
        .args(args)
        .current_dir(profile.parent().unwrap())
        .env("HOME", profile.parent().unwrap())
        .output()
        .expect("build target/debug/shadowcode first (cargo build -p shadowcode-desktop)");
    output
}

#[cfg(unix)]
#[test]
fn the_command_line_backs_up_restores_resets_and_repairs() {
    let p = profile();
    let profile_root = p.root.join("profile");
    {
        let store = Store::open(&p.paths.database()).unwrap();
        store
            .create_session(&p.project, "mock", "Line one")
            .unwrap();
    }
    let text = |o: &std::process::Output| {
        format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        )
    };
    let out = shadowcode(&profile_root, &["--json", "backup"]);
    assert!(out.status.success(), "{}", text(&out));
    let backup: Value = serde_json::from_slice(&out.stdout).unwrap();
    let folder = backup["path"].as_str().unwrap().to_owned();
    assert_eq!(backup["manifest"]["includes_secrets"], false);
    let out = shadowcode(
        &profile_root,
        &["backup", "--include-secrets", "-o", "exports"],
    );
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("keep this backup private"),
        "{}",
        text(&out)
    );
    assert_eq!(fs::read_dir(p.root.join("exports")).unwrap().count(), 1);

    {
        let store = Store::open(&p.paths.database()).unwrap();
        store
            .create_session(&p.project, "mock", "Line two")
            .unwrap();
    }
    // Without --yes only the check runs.
    let out = shadowcode(&profile_root, &["restore", &folder]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        text(&out).contains("Run again with --yes"),
        "{}",
        text(&out)
    );
    assert_eq!(titles(&p.paths), ["Line one", "Line two"]);
    // No ShadowCode runs on this profile, so it finishes right away.
    let out = shadowcode(&profile_root, &["restore", &folder, "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Restored"), "{}", text(&out));
    assert_eq!(titles(&p.paths), ["Line one"]);
    // A damaged backup is refused with the reason.
    fs::remove_file(PathBuf::from(&folder).join("config/config.yaml")).unwrap();
    let out = shadowcode(&profile_root, &["restore", &folder, "--yes"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("missing"), "{}", text(&out));

    let out = shadowcode(&profile_root, &["doctor", "--repair"]);
    assert!(text(&out).contains("Database integrity"), "{}", text(&out));
    assert!(text(&out).contains("Native diagnostics"), "{}", text(&out));

    let out = shadowcode(&profile_root, &["reset"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Nothing was changed"), "{}", text(&out));
    assert_eq!(titles(&p.paths), ["Line one"]);
    let out = shadowcode(&profile_root, &["reset", "--yes"]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("Reset done"), "{}", text(&out));
    assert!(!p.paths.database().exists());
    // The data folder held only backups (kept), so it needed no aside folder.
    let asides = data::reset_folders(&p.paths);
    assert_eq!(asides.len(), 2, "{asides:?}");
    assert!(asides.iter().any(|a| a.join("shadow-agent.db").is_file()));
    assert!(asides.iter().any(|a| a.join("secrets.env").is_file()));
}

/// Finish a scheduled restore or reset the way the next engine start does.
fn finish(paths: &AppPaths) -> Value {
    let _lock = paths.lock().unwrap();
    data::apply_pending(paths).unwrap().unwrap()
}

/// A profile with one conversation in its database.
fn profile_with_history() -> Profile {
    let p = profile();
    Store::open(&p.paths.database())
        .unwrap()
        .create_session(&p.project, "mock", "Kept")
        .unwrap();
    p
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_pairing_comes_back_only_when_asked_for_and_switched_off() {
    use shadowcode_core::remote::settings::{self as remote, Device, Settings};
    let p = profile_with_history();
    let device = |name: &str, digest: &str| Device {
        id: name.into(),
        name: name.into(),
        digest: digest.repeat(32),
        created_at: 1.0,
        last_seen: None,
    };
    let paired = Settings {
        enabled: true,
        allow_terminals: true,
        devices: vec![device("Lost phone", "ab")],
        ..Settings::default()
    };
    remote::save(&p.paths, &paired).unwrap();
    let (folder, manifest) = data::create_backup(
        &p.paths,
        BackupOptions {
            include_secrets: true,
            ..manual()
        },
    )
    .unwrap();
    assert!(manifest
        .files
        .iter()
        .any(|f| f.path == "config/remote.json"));
    // The phone was lost and removed; a tablet was paired since.
    let now = Settings {
        enabled: true,
        devices: vec![device("Tablet", "cd")],
        ..Settings::default()
    };
    remote::save(&p.paths, &now).unwrap();

    // Restoring the API keys leaves remote access as it is.
    let pending = data::schedule_restore(
        &p.paths,
        &folder,
        RestoreOptions {
            include_secrets: true,
            ..RestoreOptions::default()
        },
    )
    .unwrap();
    assert!(pending.include_secrets && !pending.include_remote);
    let last = finish(&p.paths);
    assert_eq!(last["remote_restored"], false, "{last}");
    assert_eq!(remote::load(&p.paths).unwrap(), now);

    // Asked for on its own, it comes back switched off: the lost phone is
    // paired again, so the user checks the devices before turning it on.
    let pending = data::schedule_restore(
        &p.paths,
        &folder,
        RestoreOptions {
            include_remote: true,
            ..RestoreOptions::default()
        },
    )
    .unwrap();
    assert!(pending.include_remote && !pending.include_secrets);
    let last = finish(&p.paths);
    assert_eq!(last["remote_restored"], true, "{last}");
    let restored = remote::load(&p.paths).unwrap();
    assert!(!restored.enabled, "remote access stays off until turned on");
    assert_eq!(restored.devices, paired.devices);
    assert!(restored.allow_terminals);
    #[cfg(unix)]
    assert_eq!(mode(&remote::path(&p.paths)), 0o600);
    assert_eq!(
        config::secret(&p.paths, "OPENAI_API_KEY")
            .unwrap()
            .as_deref(),
        Some("first-value")
    );
    // What it replaced is in the before-restore backup.
    let previous = PathBuf::from(last["backup_of_previous_data"].as_str().unwrap());
    let kept: Settings =
        serde_json::from_slice(&fs::read(previous.join("config/remote.json")).unwrap()).unwrap();
    assert_eq!(kept, now);
}

#[cfg(unix)]
#[test]
fn a_backup_into_a_chosen_folder_leaves_that_folder_as_it_is() {
    use std::os::unix::fs::PermissionsExt;
    let p = profile_with_history();
    // A folder other accounts may read.
    let shared = p.root.join("shared");
    fs::create_dir_all(&shared).unwrap();
    fs::set_permissions(&shared, fs::Permissions::from_mode(0o755)).unwrap();
    let chosen = |folder: &Path| {
        data::create_backup(
            &p.paths,
            BackupOptions {
                folder: Some(folder),
                ..manual()
            },
        )
        .unwrap()
        .0
    };
    let folder = chosen(&shared);
    assert_eq!(
        mode(&shared),
        0o755,
        "the chosen folder keeps its permissions"
    );
    assert_eq!(mode(&folder), 0o700);
    assert_eq!(mode(&folder.join("state")), 0o700);
    assert!(data::inspect(&p.paths, &folder).unwrap().restorable);
    // A folder reached through a symlink, such as a disk mounted elsewhere.
    let disk = p.root.join("disk/backups");
    fs::create_dir_all(&disk).unwrap();
    let link = p.root.join("Backups");
    std::os::unix::fs::symlink(&disk, &link).unwrap();
    let folder = chosen(&link);
    assert!(disk
        .join(folder.file_name().unwrap())
        .join("manifest.json")
        .is_file());
    assert!(fs::symlink_metadata(&link)
        .unwrap()
        .file_type()
        .is_symlink());
    // A missing folder is created.
    let missing = p.root.join("new/place");
    chosen(&missing);
    assert_eq!(fs::read_dir(&missing).unwrap().count(), 1);
}

/// The stand-in keyring (a file; debug builds only) is set for the whole
/// process: no other test in this file uses the keyring.
#[tokio::test(flavor = "multi_thread")]
async fn a_backup_with_api_keys_holds_the_keys_moved_to_the_keyring() {
    use shadowcode_core::keyring;
    let p = profile_with_history();
    std::env::set_var("SHADOWCODE_TEST_KEYRING", p.root.join("vault.json"));
    let name = "BACKUP_TEST_API_KEY";
    let first = format!("{}-{}", "kept-in-keyring", "first");
    config::set_secret(&p.paths, name, &first).unwrap();
    keyring::move_in(&p.paths, name).unwrap();
    assert_eq!(config::file_secret(&p.paths, name).unwrap(), None);
    let with_keys = BackupOptions {
        include_secrets: true,
        ..manual()
    };
    let (folder, manifest) = data::create_backup(&p.paths, with_keys).unwrap();
    assert!(manifest.includes_secrets);
    assert!(manifest.left_out.is_empty(), "{:?}", manifest.left_out);
    let text = fs::read_to_string(folder.join("config/secrets.env")).unwrap();
    assert!(text.contains(&first), "the keyring's key: {text}");
    assert!(text.contains("first-value"), "the file's key: {text}");
    #[cfg(unix)]
    assert_eq!(mode(&folder.join("config/secrets.env")), 0o600);
    assert!(data::inspect(&p.paths, &folder).unwrap().restorable);
    let keys = RestoreOptions {
        include_secrets: true,
        ..RestoreOptions::default()
    };

    // After a reset (which moves keyring.json aside) the key comes back.
    data::schedule_reset(&p.paths).unwrap();
    finish(&p.paths);
    assert_eq!(config::secret(&p.paths, name).unwrap(), None);
    data::schedule_restore(&p.paths, &folder, keys).unwrap();
    finish(&p.paths);
    assert_eq!(
        config::secret(&p.paths, name).unwrap().as_deref(),
        Some(first.as_str())
    );

    // On the same computer, a restore brings back the backup's key, not the
    // keyring's newer one.
    keyring::move_in(&p.paths, name).unwrap();
    config::set_secret(
        &p.paths,
        name,
        &format!("{}-{}", "kept-in-keyring", "second"),
    )
    .unwrap();
    data::schedule_restore(&p.paths, &folder, keys).unwrap();
    finish(&p.paths);
    assert_eq!(
        config::secret(&p.paths, name).unwrap().as_deref(),
        Some(first.as_str())
    );
    assert!(!keyring::listed(&p.paths).contains(name));

    // A key the keyring does not give is named, and a backup with no key
    // does not claim to hold any.
    let q = profile();
    config::set_secret(&q.paths, "OPENAI_API_KEY", "").unwrap();
    fs::write(
        q.paths.config.join("keyring.json"),
        r#"{"names":["GONE_API_KEY"]}"#,
    )
    .unwrap();
    let (folder, manifest) = data::create_backup(&q.paths, with_keys).unwrap();
    assert!(!manifest.includes_secrets);
    assert!(!folder.join("config/secrets.env").exists());
    assert!(
        manifest.left_out.iter().any(|n| n.contains("GONE_API_KEY")),
        "{:?}",
        manifest.left_out
    );
    assert!(!data::list_backups(&data::backups_dir(&q.paths))[0].includes_secrets);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_large_agents_folder_never_blocks_a_backup_or_a_restore() {
    let p = profile_with_history();
    let agents = shadowcode_core::agents::user_dir(&p.paths);
    // Agent definitions kept in a Git repository.
    let objects = agents.join(".git/objects/ab");
    fs::create_dir_all(&objects).unwrap();
    for i in 0..4200 {
        fs::write(objects.join(format!("{i:05}")), "x").unwrap();
    }
    fs::write(agents.join("helper.md"), "---\nname: helper\n---\nHelp.\n").unwrap();
    fs::write(agents.join("model.bin"), vec![0u8; 4 * 1024 * 1024 + 1]).unwrap();
    let (folder, manifest) = data::create_backup(&p.paths, manual()).unwrap();
    let listed: Vec<&str> = manifest.files.iter().map(|f| f.path.as_str()).collect();
    assert!(listed.contains(&"agents/helper.md"), "{listed:?}");
    assert!(
        !listed
            .iter()
            .any(|f| f.contains(".git") || f.ends_with("model.bin")),
        "{listed:?}"
    );
    assert_eq!(manifest.left_out.len(), 1, "{:?}", manifest.left_out);
    assert!(
        manifest.left_out[0].contains("agents/model.bin")
            && manifest.left_out[0].contains("larger than 4 MB"),
        "{:?}",
        manifest.left_out
    );
    // Past the file limit, the rest is counted and the backup still works.
    let notes = agents.join("notes");
    fs::create_dir_all(&notes).unwrap();
    for i in 0..4200 {
        fs::write(notes.join(format!("{i:05}.txt")), "x").unwrap();
    }
    let (_, manifest) = data::create_backup(&p.paths, manual()).unwrap();
    assert_eq!(
        manifest.files.len(),
        4096 + 1,
        "the database and 4096 files"
    );
    assert!(
        manifest
            .left_out
            .iter()
            .any(|n| n.contains("past the limit of 4096 files")),
        "{:?}",
        manifest.left_out
    );
    // The backup made before a restore works too.
    data::schedule_restore(&p.paths, &folder, RestoreOptions::default()).unwrap();
    let last = finish(&p.paths);
    assert_eq!(last["ok"], true, "{last}");
}

#[cfg(unix)]
#[test]
fn a_reset_keeps_the_lock_and_backups_when_the_xdg_folders_are_one() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    // XDG_CONFIG_HOME, XDG_DATA_HOME and XDG_STATE_HOME all name one folder.
    let one = root.join("xdg/shadow-agent");
    let paths = AppPaths {
        config: one.clone(),
        data: one.clone(),
        state: one.clone(),
        cache: root.join("cache/shadow-agent"),
    };
    paths.ensure().unwrap();
    Config::patch(&paths, json!({"ui": {"theme": "dark"}})).unwrap();
    {
        let store = Store::open(&paths.database()).unwrap();
        store.create_session(&root, "mock", "Old life").unwrap();
    }
    data::create_backup(&paths, manual()).unwrap();
    fs::create_dir_all(one.join("managed-worktrees/task")).unwrap();
    data::schedule_reset(&paths).unwrap();
    let lock = paths.lock().unwrap();
    let last = data::apply_pending(&paths).unwrap().unwrap();
    assert_eq!(last["ok"], true, "{last}");
    // The running engine's lock stays where it is and still holds, so no
    // second engine can open the profile.
    assert!(one.join("native.lock").is_file());
    let second = paths.lock().err().expect("the profile is still locked");
    assert!(format!("{second:#}").contains("already running"));
    drop(lock);
    // Backups and worktrees stay; settings and history moved aside once.
    assert_eq!(data::list_backups(&data::backups_dir(&paths)).len(), 1);
    assert!(one.join("managed-worktrees/task").is_dir());
    assert!(!paths.database().exists());
    assert!(!paths.config_file().exists());
    let asides = data::reset_folders(&paths);
    assert_eq!(asides.len(), 1, "{asides:?}");
    assert_eq!(last["moved_to"].as_array().unwrap().len(), 1, "{last}");
    assert!(asides[0].join("shadow-agent.db").is_file());
    assert!(asides[0].join("config.yaml").is_file());

    // The data folder inside the config folder: it is not moved with it.
    let config = root.join("nested/shadow-agent");
    let paths = AppPaths {
        data: config.join("shadow-agent"),
        state: root.join("state/shadow-agent"),
        cache: root.join("cache/shadow-agent"),
        config,
    };
    paths.ensure().unwrap();
    Config::patch(&paths, json!({"ui": {"theme": "dark"}})).unwrap();
    data::create_backup(&paths, manual()).unwrap();
    data::schedule_reset(&paths).unwrap();
    let last = finish(&paths);
    assert_eq!(last["ok"], true, "{last}");
    assert!(!paths.config_file().exists());
    assert_eq!(data::list_backups(&data::backups_dir(&paths)).len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_restart_advice_names_the_process_that_holds_the_profile() {
    let p = profile();
    let service = Service::open(p.paths.clone(), Some(p.project.clone())).unwrap();
    // Unknown owner: every kind of ShadowCode process is named.
    let overview = call(&service, "GET", "/api/data", Value::Null)
        .await
        .unwrap();
    assert!(overview["engine"]["mode"].is_null(), "{overview}");
    assert!(overview["engine"]["restart"]
        .as_str()
        .unwrap()
        .contains("`shadowcode acp`"));
    // An editor's `shadowcode acp` owns the engine: quitting a window
    // attached to it would not let the reset run.
    let server = shadowcode_core::control::Server::start_with_mode(service.clone(), "acp").unwrap();
    let overview = call(&service, "GET", "/api/data", Value::Null)
        .await
        .unwrap();
    assert_eq!(overview["engine"]["mode"], "acp");
    assert_eq!(overview["engine"]["pid"], std::process::id());
    let restart = overview["engine"]["restart"].as_str().unwrap();
    assert!(
        restart.contains("editor") && restart.contains("`shadowcode acp`"),
        "{restart}"
    );
    let scheduled = call(
        &service,
        "POST",
        "/api/data/reset",
        json!({"confirm": "reset"}),
    )
    .await
    .unwrap();
    let message = scheduled["message"].as_str().unwrap();
    assert!(
        message.contains("next time ShadowCode starts") && message.contains("editor"),
        "{message}"
    );
    assert!(data::cancel_pending(&p.paths).unwrap());
    assert_eq!(
        data::restart_hint(Some("desktop"), 1),
        "Quit ShadowCode and open it again."
    );
    server.close();
    server.wait_closed().await;
    service.engine.shutdown().await.unwrap();
}
