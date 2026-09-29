//! Every released profile format opens in this version with nothing lost.
//!
//! `tests/fixtures/upgrade/<version>/` holds a profile written by that
//! release's own engine (see `scripts/generate-upgrade-fixtures.mjs`): its
//! database as SQL text, `config.yaml`, `secrets.env` and other small files,
//! plus `manifest.json` naming the records it created. Each one is rebuilt in
//! a temporary profile and opened through `Service::open`, the way the
//! desktop and CLI open a profile after an upgrade.
use serde_json::{json, Value};
use shadowcode_core::{
    paths::AppPaths,
    service::{Request, Service},
    store::{Store, SCHEMA_VERSION},
};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/upgrade")
}

fn versions() -> Vec<String> {
    let mut found: Vec<String> = fs::read_dir(fixtures())
        .unwrap()
        .flatten()
        .filter(|e| e.path().join("manifest.json").is_file())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort_by_key(|v| {
        v.split('.')
            .map(|p| p.parse::<u32>().unwrap())
            .collect::<Vec<_>>()
    });
    found
}

struct Profile {
    _root: tempfile::TempDir,
    root: PathBuf,
    project: PathBuf,
    paths: AppPaths,
    manifest: Value,
}

fn copy_tree(from: &Path, to: &Path, root: &Path) {
    let Ok(entries) = fs::read_dir(from) else {
        return;
    };
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            fs::create_dir_all(&target).unwrap();
            copy_tree(&entry.path(), &target, root);
        } else {
            let text = fs::read_to_string(entry.path()).unwrap();
            fs::write(&target, text.replace("@ROOT@", &root.to_string_lossy())).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }
}

/// The fixture profiles' `secrets.env`: placeholders, never credentials.
const FIXTURE_SECRETS: &str = "OPENROUTER_API_KEY=\"fixture-openrouter-placeholder\"\nSHADOWCODE_FIXTURE_KEY=\"fixture-value-not-a-credential\"\n";

/// A private file (as ShadowCode writes them), unless the fixture has one.
fn write_private(path: &Path, text: &str) {
    if path.exists() {
        return;
    }
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

/// Rebuild a released profile in a temporary folder.
fn profile(version: &str) -> Profile {
    let dir = fixtures().join(version);
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("README.md"), "# Fixture project\n").unwrap();
    fs::write(project.join("notes.txt"), "fixture notes\n").unwrap();
    let paths = AppPaths::isolated(&root.join("profile")).unwrap();
    for (label, target) in [
        ("config", &paths.config),
        ("data", &paths.data),
        ("state", &paths.state),
    ] {
        copy_tree(&dir.join("profile").join(label), target, &root);
    }
    // Every released profile also had these two files, which the repository
    // cannot hold (`.gitignore` and scripts/check-secrets.mjs keep any
    // `secrets.env` and `last-workspace.txt` out of Git): write them here.
    write_private(&paths.secrets_file(), FIXTURE_SECRETS);
    write_private(
        &paths.state.join("last-workspace.txt"),
        &format!("{}\n", project.display()),
    );
    let sql = fs::read_to_string(dir.join("shadow-agent.sql"))
        .unwrap()
        .replace("@ROOT@", &root.to_string_lossy());
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    // A throwaway file: one transaction, no fsync per row.
    db.pragma_update(None, "synchronous", "OFF").unwrap();
    db.execute_batch(&format!("BEGIN;\n{sql}\nCOMMIT;"))
        .unwrap();
    // Released versions kept the database in write-ahead-log mode.
    db.pragma_update(None, "journal_mode", "WAL").unwrap();
    drop(db);
    let manifest: Value =
        serde_json::from_str(&fs::read_to_string(dir.join("manifest.json")).unwrap()).unwrap();
    Profile {
        _root: temp,
        root,
        project,
        paths,
        manifest,
    }
}

fn counts(path: &Path) -> BTreeMap<String, i64> {
    let db = rusqlite::Connection::open(path).unwrap();
    let tables: Vec<String> = db
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    tables
        .into_iter()
        .map(|table| {
            let count = db
                .query_row(&format!("SELECT count(*) FROM \"{table}\""), [], |r| {
                    r.get(0)
                })
                .unwrap();
            (table, count)
        })
        .collect()
}

fn user_version(path: &Path) -> i64 {
    rusqlite::Connection::open(path)
        .unwrap()
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap()
}

fn upgrade_copies(paths: &AppPaths) -> Vec<PathBuf> {
    fs::read_dir(&paths.state)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            name.starts_with("shadow-agent.pre-native-") && name.ends_with(".sqlite")
        })
        .collect()
}

async fn call(service: &Service, method: &str, path: &str, body: Value) -> Value {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
        .unwrap_or_else(|error| panic!("{method} {path}: {error:#}"))
}

fn ids(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|row| row["id"].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn a_fixture_exists_for_every_release_and_schema_version() {
    let found = versions();
    let script = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/generate-upgrade-fixtures.mjs"),
    )
    .unwrap();
    let list = script
        .split_once("export const RELEASES = [")
        .unwrap()
        .1
        .split_once("];")
        .unwrap()
        .0;
    let releases: Vec<String> = list
        .split('\'')
        .skip(1)
        .step_by(2)
        .map(|tag| tag.trim_start_matches('v').to_owned())
        .collect();
    assert!(releases.len() >= 11, "{releases:?}");
    for release in &releases {
        assert!(
            found.contains(release),
            "no upgrade fixture for {release}: run node scripts/generate-upgrade-fixtures.mjs v{release}"
        );
    }
    let schemas: Vec<i64> = found
        .iter()
        .map(|v| {
            let manifest: Value = serde_json::from_str(
                &fs::read_to_string(fixtures().join(v).join("manifest.json")).unwrap(),
            )
            .unwrap();
            manifest["schema_version"].as_i64().unwrap()
        })
        .collect();
    for schema in 25..=27 {
        assert!(schemas.contains(&schema), "no fixture at schema {schema}");
    }
    assert!(schemas.iter().all(|s| *s <= SCHEMA_VERSION));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_released_profile_opens_with_all_its_data() {
    let versions = versions();
    assert!(!versions.is_empty());
    for version in versions {
        check_release(&version).await;
    }
}

async fn check_release(version: &str) {
    let fixture = profile(version);
    let paths = fixture.paths.clone();
    let manifest = &fixture.manifest;
    let from_schema = manifest["schema_version"].as_i64().unwrap();
    assert_eq!(user_version(&paths.database()), from_schema, "{version}");
    let before = counts(&paths.database());
    for (table, count) in manifest["row_counts"].as_object().unwrap() {
        assert_eq!(
            before.get(table).copied(),
            count.as_i64(),
            "{version}: the fixture database was rebuilt incompletely ({table})"
        );
    }
    let secrets_before = fs::read(paths.secrets_file()).unwrap();
    eprintln!("upgrade fixture {version}: database format {from_schema} -> {SCHEMA_VERSION}");

    let service = Service::open(paths.clone(), Some(fixture.project.clone()))
        .unwrap_or_else(|error| panic!("{version} did not open: {error:#}"));

    // Migrated in place, after a copy of the old database was kept.
    assert_eq!(user_version(&paths.database()), SCHEMA_VERSION, "{version}");
    let copies = upgrade_copies(&paths);
    if from_schema < SCHEMA_VERSION {
        assert_eq!(copies.len(), 1, "{version}: one pre-upgrade copy");
        assert_eq!(user_version(&copies[0]), from_schema, "{version}");
        assert_eq!(
            counts(&copies[0]),
            before,
            "{version}: the pre-upgrade copy holds everything"
        );
    } else {
        assert!(copies.is_empty(), "{version}: nothing to migrate, no copy");
    }
    // Nothing lost: every table keeps at least its rows; the records people
    // see keep exactly theirs.
    let after = counts(&paths.database());
    for (table, count) in &before {
        assert!(
            after.get(table).copied().unwrap_or(0) >= *count,
            "{version}: rows lost from {table}: {count} -> {:?}",
            after.get(table)
        );
    }
    for table in [
        "sessions",
        "tasks",
        "desktop_jobs",
        "goals",
        "milestones",
        "pins",
        "task_notes",
        "usage_snapshots",
        "automations",
        "automation_runs",
        "editor_drafts",
    ] {
        if let Some(count) = before.get(table) {
            assert_eq!(after.get(table), Some(count), "{version}: {table}");
        }
    }

    // Conversations, their transcripts, pins and notes.
    let session = manifest["session_id"].as_str().unwrap();
    let listed = call(
        &service,
        "GET",
        &format!(
            "/api/sessions?limit=100&include_compare=true&workspace={}",
            fixture.project.display()
        ),
        Value::Null,
    )
    .await;
    let listed = ids(&listed["sessions"]);
    for key in ["session_id", "second_session_id", "branch_session_id"] {
        if let Some(id) = manifest[key].as_str() {
            assert!(listed.iter().any(|l| l == id), "{version}: {key} listed");
        }
    }
    let conversation = call(
        &service,
        "GET",
        &format!("/api/sessions/{session}?summary=true"),
        Value::Null,
    )
    .await;
    assert_eq!(conversation["title"], "Fixture conversation", "{version}");
    let events = call(
        &service,
        "GET",
        &format!("/api/sessions/{session}/events?limit=1000"),
        Value::Null,
    )
    .await;
    let events = events["events"].as_array().unwrap();
    assert!(
        events.iter().any(|e| e["type"] == "agent.completed"),
        "{version}: transcript kept"
    );
    assert!(
        events
            .iter()
            .any(|e| e["type"] == "tool.completed" && e["payload"]["tool"] == "write_file"),
        "{version}: tool steps kept"
    );
    let pins = call(
        &service,
        "GET",
        &format!("/api/sessions/{session}/pins"),
        Value::Null,
    )
    .await;
    assert!(
        pins["pins"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["body"] == "Remember the fixture"),
        "{version}: pin kept"
    );
    if let Some(note) = manifest.get("task_note") {
        let read = call(
            &service,
            "POST",
            "/api/memory",
            json!({"action": "read", "scope": "task", "task_id": note["task_id"], "workspace": fixture.project}),
        )
        .await;
        assert!(
            read.to_string().contains("Fixture task note"),
            "{version}: task note kept: {read}"
        );
    }

    // Jobs and their results.
    for job in manifest["jobs"].as_array().unwrap() {
        let id = job["id"].as_str().unwrap();
        let saved = call(&service, "GET", &format!("/api/jobs/{id}"), Value::Null).await;
        assert_eq!(saved["status"], job["status"], "{version}: job {id}");
        assert_eq!(saved["task_id"], job["task_id"], "{version}: job {id}");
    }
    let summaries = call(&service, "GET", "/api/jobs?view=summary", Value::Null).await;
    assert!(
        summaries["jobs"].as_array().unwrap().len() >= 2,
        "{version}"
    );

    // Goals.
    let goals = call(&service, "GET", "/api/goals?all=true", Value::Null).await;
    let goal_ids = ids(&goals["goals"]);
    for key in ["goal_id", "run_goal_id"] {
        if let Some(id) = manifest[key].as_str() {
            assert!(goal_ids.iter().any(|g| g == id), "{version}: {key}");
            let goal = call(&service, "GET", &format!("/api/goals/{id}"), Value::Null).await;
            assert_eq!(goal["id"], id, "{version}");
            assert!(
                !goal["milestones"].as_array().unwrap().is_empty(),
                "{version}: milestones of {key}"
            );
        }
    }

    // Automations and their history.
    if let Some(id) = manifest["automation_id"].as_str() {
        let all = call(&service, "GET", "/api/automations?all=true", Value::Null).await;
        let listed = ids(&all["automations"]);
        assert!(listed.iter().any(|a| a == id), "{version}");
        if let Some(paused) = manifest["paused_automation_id"].as_str() {
            assert!(listed.iter().any(|a| a == paused), "{version}");
        }
        let runs = call(
            &service,
            "GET",
            &format!("/api/automations/{id}/runs"),
            Value::Null,
        )
        .await;
        assert!(
            runs["runs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == manifest["automation_run_id"]),
            "{version}: automation run kept: {runs}"
        );
    }

    // Compare records.
    if let Some(id) = manifest["compare_id"].as_str() {
        let record = call(&service, "GET", &format!("/api/compare/{id}"), Value::Null).await;
        assert_eq!(record["id"], id, "{version}");
        assert_eq!(record["lanes"].as_array().unwrap().len(), 2, "{version}");
        let board = call(
            &service,
            "GET",
            &format!(
                "/api/compare/scoreboard?workspace={}",
                fixture.project.display()
            ),
            Value::Null,
        )
        .await;
        assert!(
            board["rows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["model"] == "local:gguf:fixture-a" && r["wins"] == 1),
            "{version}: compare scoreboard kept: {board}"
        );
    }

    // Unsaved editor drafts.
    if manifest.get("editor_draft").is_some() {
        let drafts = call(
            &service,
            "GET",
            &format!(
                "/api/workspace/editor-drafts?workspace={}",
                fixture.project.display()
            ),
            Value::Null,
        )
        .await;
        assert!(
            drafts["drafts"]
                .as_array()
                .unwrap()
                .iter()
                .any(|d| d["path"] == "README.md"
                    && d["draft"].as_str().unwrap().contains("Unsaved line")),
            "{version}: editor draft kept: {drafts}"
        );
    }

    // Subscription usage.
    let usage = service.engine.store().usage_snapshots().unwrap();
    assert!(
        usage
            .iter()
            .any(|u| u.vendor == "codex" && u.account == "fixture@example.invalid"),
        "{version}: usage kept"
    );

    // Settings load, keep keys this version does not know, and save.
    let config = call(&service, "GET", "/api/config", Value::Null).await;
    assert_eq!(config["model"]["name"], "fixture-coder", "{version}");
    assert_eq!(config["ui"]["theme"], "dark", "{version}");
    call(
        &service,
        "PUT",
        "/api/config",
        json!({"values": {"ui": {"notify": true}}}),
    )
    .await;
    let saved = fs::read_to_string(paths.config_file()).unwrap();
    assert!(
        saved.contains("a_setting_from_the_future"),
        "{version}: unknown keys are kept"
    );
    assert!(
        saved.contains(&*fixture.project.to_string_lossy()),
        "{version}: trust kept"
    );
    assert_eq!(
        fs::read(paths.secrets_file()).unwrap(),
        secrets_before,
        "{version}: secrets.env untouched"
    );

    // Your data sees the pre-upgrade copy.
    let data = call(&service, "GET", "/api/data", Value::Null).await;
    assert_eq!(
        data["upgrade_copies"].as_array().unwrap().len(),
        copies.len(),
        "{version}"
    );

    service.engine.shutdown().await.unwrap();
    drop(service);
    // A second start migrates nothing and copies nothing again.
    let store = Store::open(&paths.database()).unwrap();
    drop(store);
    assert_eq!(upgrade_copies(&paths).len(), copies.len(), "{version}");
    assert!(fixture.root.exists());
}

/// Downgrade protection: a database from a newer version is refused before
/// anything is written, and the refusal names the version to use.
#[test]
fn a_newer_database_is_refused_without_changing_anything() {
    let latest = versions().pop().unwrap();
    let fixture = profile(&latest);
    let paths = fixture.paths.clone();
    // Merge the log so the file alone is the database, then pretend a
    // future version wrote it.
    {
        let db = rusqlite::Connection::open(paths.database()).unwrap();
        db.pragma_update(None, "journal_mode", "DELETE").unwrap();
        db.execute(
            "INSERT OR REPLACE INTO native_meta(key,value) VALUES('app_version','9.9.9')",
            [],
        )
        .unwrap();
        db.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
    }
    let listing = |dir: &Path| -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    let bytes = fs::read(paths.database()).unwrap();
    let files = listing(&paths.state);
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(paths.database()).unwrap().permissions().mode()
    };

    let error = format!("{:#}", Store::open(&paths.database()).err().unwrap());
    assert!(error.contains("ShadowCode 9.9.9"), "{error}");
    assert!(
        error.contains(&format!("database format {}", SCHEMA_VERSION + 1)),
        "{error}"
    );
    assert!(error.contains("Nothing was changed"), "{error}");
    let error = format!(
        "{:#}",
        Service::open(paths.clone(), Some(fixture.project.clone()))
            .err()
            .unwrap()
    );
    assert!(
        error.contains("newer") || error.contains("9.9.9"),
        "{error}"
    );

    assert_eq!(fs::read(paths.database()).unwrap(), bytes, "database bytes");
    // Only the profile lock may appear (the engine takes it before it
    // looks at the database); no log, no copy, no backup.
    let mut expected = files.clone();
    if !expected.iter().any(|f| f == "native.lock") {
        expected.push("native.lock".into());
        expected.sort();
    }
    let now = listing(&paths.state);
    assert!(now == files || now == expected, "{files:?} -> {now:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(paths.database()).unwrap().permissions().mode(),
            mode
        );
    }
    // The same database opens again once this version is replaced by one
    // that knows the format: here, by putting the version back.
    {
        let db = rusqlite::Connection::open(paths.database()).unwrap();
        db.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
    }
    Store::open(&paths.database()).unwrap();
}
