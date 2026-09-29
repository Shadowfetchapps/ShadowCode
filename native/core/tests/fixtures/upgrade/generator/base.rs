//! Upgrade-fixture generator (not compiled in this tree).
//!
//! `scripts/generate-upgrade-fixtures.mjs` copies this file, plus the
//! snippets for features a release had, into an exported copy of that
//! release's source and runs it there. The profile is therefore written by
//! the released engine itself: its own schema, migrations, JSON payload
//! shapes and config serialization. The result is dumped as text into
//! `native/core/tests/fixtures/upgrade/<version>/`, which
//! `native/core/tests/upgrade_fixtures.rs` opens with the current version.
//!
//! Only API that every release since 0.28.0 has is used here: `Service`
//! and its JSON routes, `Config::patch`, `set_secret` and a few `Store`
//! calls. Everything that can fail on an older release is recorded as
//! skipped instead of aborting.
use serde_json::{json, Map, Value};
use shadowcode_core::{
    config::{self, Config},
    paths::AppPaths,
    service::{Request, Service},
    store::UsageRow,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

pub struct Ctx {
    pub service: Service,
    pub paths: AppPaths,
    pub project: PathBuf,
    pub session_id: String,
    pub job_ids: Vec<String>,
    pub manifest: Map<String, Value>,
    pub skipped: Vec<String>,
}

impl Ctx {
    pub async fn call(&self, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
        self.service
            .dispatch(Request {
                method: method.into(),
                path: path.into(),
                body,
            })
            .await
    }
    pub fn record(&mut self, key: &str, value: Value) {
        self.manifest.insert(key.into(), value);
    }
    pub fn skip(&mut self, what: &str, error: impl std::fmt::Display) {
        eprintln!("fixture: skipped {what}: {error}");
        self.skipped.push(format!("{what}: {error}"));
    }
    /// Start a task and wait until it stops.
    pub async fn run_task(&mut self, task: &str, session: Option<&str>) -> anyhow::Result<Value> {
        let mut body = json!({"workspace": self.project, "task": task});
        if let Some(session) = session {
            body["session_id"] = json!(session);
        }
        let job = self.call("POST", "/api/jobs", body).await?;
        let id = job["id"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("no job id in {job}"))?
            .to_owned();
        let job = self.wait_job(&id).await?;
        self.job_ids.push(id);
        Ok(job)
    }
    pub async fn wait_job(&self, id: &str) -> anyhow::Result<Value> {
        let deadline = Instant::now() + Duration::from_secs(90);
        loop {
            let job = self.call("GET", &format!("/api/jobs/{id}"), Value::Null).await?;
            let status = job["status"].as_str().unwrap_or("");
            if !matches!(
                status,
                "" | "queued" | "starting" | "running" | "waiting" | "waiting_approval" | "paused"
            ) {
                return Ok(job);
            }
            anyhow::ensure!(Instant::now() < deadline, "job {id} still {status}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// A tiny OpenAI-compatible endpoint: a task first writes `notes.txt`, then
/// answers; requests without tools (titles, summaries) get a short answer.
async fn model_server() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let counter = Arc::new(Mutex::new(0usize));
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let counter = counter.clone();
            tokio::spawn(async move {
                let mut wire = Vec::new();
                let mut buffer = [0; 8192];
                let body = loop {
                    let count = socket.read(&mut buffer).await.unwrap_or(0);
                    if count == 0 {
                        return;
                    }
                    wire.extend_from_slice(&buffer[..count]);
                    if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&wire[..end]).to_lowercase();
                        let len = headers
                            .lines()
                            .find_map(|line| {
                                line.strip_prefix("content-length:")
                                    .and_then(|v| v.trim().parse::<usize>().ok())
                            })
                            .unwrap_or(0);
                        if wire.len() >= end + 4 + len {
                            break serde_json::from_slice::<Value>(&wire[end + 4..end + 4 + len])
                                .unwrap_or(Value::Null);
                        }
                    }
                };
                let n = {
                    let mut n = counter.lock().unwrap();
                    *n += 1;
                    *n
                };
                let usage = json!({"prompt_tokens": 120 + n, "completion_tokens": 30, "total_tokens": 150 + n});
                let has_tools = body["tools"].as_array().is_some_and(|t| !t.is_empty());
                let last_role = body["messages"]
                    .as_array()
                    .and_then(|m| m.last())
                    .map(|m| m["role"].as_str().unwrap_or("").to_owned())
                    .unwrap_or_default();
                let message = if !has_tools {
                    json!({"role": "assistant", "content": "Fixture notes"})
                } else if last_role == "tool" {
                    json!({"role": "assistant", "content": "Wrote notes.txt with the fixture notes."})
                } else {
                    json!({"role": "assistant", "content": "", "tool_calls": [{
                        "id": format!("call-{n}"), "type": "function",
                        "function": {"name": "write_file", "arguments": json!({
                            "path": "notes.txt",
                            "content": format!("fixture notes {n}\n"),
                        }).to_string()}
                    }]})
                };
                let reason = if message.get("tool_calls").is_some() {
                    "tool_calls"
                } else {
                    "stop"
                };
                let text = json!({"id": format!("fixture-{n}"), "object": "chat.completion",
                    "choices": [{"index": 0, "message": message, "finish_reason": reason}],
                    "usage": usage})
                .to_string();
                let _ = socket
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                            text.len()
                        )
                        .as_bytes(),
                    )
                    .await;
            });
        }
    });
    endpoint
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "generates an upgrade fixture; run by scripts/generate-upgrade-fixtures.mjs"]
async fn generate_upgrade_fixture() {
    let out = PathBuf::from(std::env::var("SHADOWCODE_FIXTURE_OUT").expect("SHADOWCODE_FIXTURE_OUT"));
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("README.md"), "# Fixture project\n").unwrap();
    let paths = AppPaths::isolated(&root.join("profile")).unwrap();
    let endpoint = model_server().await;

    // Start from this release's own example configuration when it loads,
    // then change what a user would change in Settings.
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config.example.yaml");
    if let Ok(text) = fs::read_to_string(&example) {
        fs::write(paths.config_file(), text).unwrap();
        if Config::load(&paths, None).is_err() {
            eprintln!("fixture: the example configuration does not load; starting empty");
            fs::remove_file(paths.config_file()).unwrap();
        }
    }
    Config::patch(
        &paths,
        json!({
            "model": {"provider": "local", "endpoint": endpoint, "name": "fixture-coder",
                      "context_limit": 16384, "api_key_env": "SHADOWCODE_FIXTURE_KEY"},
            "trusted_workspaces": [project],
            "permissions": {"approve_shell": false, "mode": "allow_edits"},
            "agent": {"max_steps": 12},
            "ui": {"theme": "dark", "notify": false},
            "onboarding": {"completed": true, "workspace": project},
            "a_setting_from_the_future": {"kept": true},
        }),
    )
    .unwrap();
    config::set_secret(&paths, "SHADOWCODE_FIXTURE_KEY", "fixture-value-not-a-credential").unwrap();
    config::set_secret(&paths, "OPENROUTER_API_KEY", "fixture-openrouter-placeholder").unwrap();

    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let mut ctx = Ctx {
        service,
        paths: paths.clone(),
        project: project.clone(),
        session_id: String::new(),
        job_ids: Vec::new(),
        manifest: Map::new(),
        skipped: Vec::new(),
    };
    ctx.record("version", json!(shadowcode_core::VERSION));

    // A conversation with two turns that edit a file.
    let opened = ctx
        .call("POST", "/api/projects/trust", json!({"path": project}))
        .await
        .unwrap();
    ctx.session_id = opened["session_id"].as_str().unwrap().to_owned();
    let sid = ctx.session_id.clone();
    let first = ctx.run_task("Write the fixture notes", Some(&sid)).await.unwrap();
    let second = ctx.run_task("Update the fixture notes", Some(&sid)).await.unwrap();
    ctx.record("session_id", json!(sid));
    ctx.record(
        "jobs",
        json!([
            {"id": first["id"], "status": first["status"], "task_id": first["task_id"]},
            {"id": second["id"], "status": second["status"], "task_id": second["task_id"]},
        ]),
    );
    let _ = ctx
        .call("PATCH", &format!("/api/sessions/{sid}"), json!({"title": "Fixture conversation"}))
        .await;
    match ctx
        .call(
            "POST",
            &format!("/api/sessions/{sid}/pins"),
            json!({"label": "Pinned", "body": "Remember the fixture"}),
        )
        .await
    {
        Ok(pin) => ctx.record("pin_id", pin["id"].clone()),
        Err(error) => ctx.skip("pin", error),
    }
    match ctx
        .call(
            "POST",
            "/api/memory",
            json!({"action": "append", "scope": "project", "note": "Fixture project note"}),
        )
        .await
    {
        Ok(_) => ctx.record("project_note", json!("Fixture project note")),
        Err(error) => ctx.skip("project note", error),
    }
    if let Some(task_id) = first["task_id"].as_str() {
        match ctx
            .call(
                "POST",
                "/api/memory",
                json!({"action": "append", "scope": "task", "task_id": task_id, "note": "Fixture task note"}),
            )
            .await
        {
            Ok(_) => ctx.record("task_note", json!({"task_id": task_id, "note": "Fixture task note"})),
            Err(error) => ctx.skip("task note", error),
        }
    }
    match ctx
        .call("POST", &format!("/api/sessions/{sid}/branch"), json!({"title": "Fixture branch"}))
        .await
    {
        Ok(branch) => ctx.record("branch_session_id", branch["id"].clone()),
        Err(error) => ctx.skip("branch", error),
    }

    // A second conversation.
    match ctx
        .call("POST", "/api/sessions", json!({"workspace": project, "title": "Second conversation"}))
        .await
    {
        Ok(session) => {
            let second_sid = session["id"].as_str().unwrap_or_default().to_owned();
            match ctx.run_task("Write the fixture notes again", Some(&second_sid)).await {
                Ok(_) => ctx.record("second_session_id", json!(second_sid)),
                Err(error) => ctx.skip("second conversation task", error),
            }
        }
        Err(error) => ctx.skip("second conversation", error),
    }

    // A goal with its default milestones, and one that ran.
    match ctx
        .call(
            "POST",
            "/api/goals",
            json!({"workspace": project, "instruction": "Keep the fixture notes current"}),
        )
        .await
    {
        Ok(goal) => ctx.record("goal_id", goal["id"].clone()),
        Err(error) => ctx.skip("goal", error),
    }
    match ctx
        .call(
            "POST",
            "/api/goals",
            json!({"workspace": project, "instruction": "Write the notes as a goal",
                   "milestones": [{"title": "Write the notes", "mode": "code"}], "run": true}),
        )
        .await
    {
        Ok(goal) => {
            let id = goal["id"].as_str().or(goal["goal_id"].as_str()).unwrap_or_default().to_owned();
            let deadline = Instant::now() + Duration::from_secs(60);
            while Instant::now() < deadline {
                match ctx.call("GET", &format!("/api/goals/{id}"), Value::Null).await {
                    Ok(g) if g["status"] != "running" && g["status"] != "active" => break,
                    Ok(_) => tokio::time::sleep(Duration::from_millis(200)).await,
                    Err(_) => break,
                }
            }
            ctx.record("run_goal_id", json!(id));
        }
        Err(error) => ctx.skip("goal run", error),
    }

    // Subscription usage as the Accounts page stores it.
    let store = ctx.service.engine.store();
    match store.upsert_usage_snapshot(&UsageRow {
        vendor: "codex".into(),
        account: "fixture@example.invalid".into(),
        pool: "primary".into(),
        fetched_at: 1_759_000_000.0,
        payload: json!({"rateLimits": {"primary": {"usedPercent": 42, "windowDurationMins": 300, "resetsAt": 1759018000}}}),
    }) {
        Ok(()) => ctx.record("usage_snapshot", json!({"vendor": "codex", "account": "fixture@example.invalid", "pool": "primary"})),
        Err(error) => ctx.skip("usage snapshot", error),
    }

    version_specific(&mut ctx).await;

    // Close the engine the way the app does, then dump the profile.
    let Ctx {
        service,
        mut manifest,
        skipped,
        ..
    } = ctx;
    service.engine.shutdown().await.unwrap();
    drop(service);
    manifest.insert("skipped".into(), json!(skipped));
    dump_profile(&paths, &root, &out, manifest);
}

/// Replace the generation directory with `@ROOT@` so the test can put the
/// profile anywhere.
fn portable(text: &str, root: &Path) -> String {
    text.replace(&*root.to_string_lossy(), "@ROOT@")
}

fn sql_literal(value: rusqlite::types::ValueRef<'_>) -> String {
    use rusqlite::types::ValueRef;
    match value {
        ValueRef::Null => "NULL".into(),
        ValueRef::Integer(i) => i.to_string(),
        ValueRef::Real(f) => {
            let text = format!("{f:?}");
            if text.contains(['.', 'e', 'E']) || text.contains("inf") || text.contains("NaN") {
                text
            } else {
                format!("{text}.0")
            }
        }
        ValueRef::Text(t) => format!("'{}'", String::from_utf8_lossy(t).replace('\'', "''")),
        ValueRef::Blob(b) => {
            let hex: String = b.iter().map(|byte| format!("{byte:02x}")).collect();
            format!("X'{hex}'")
        }
    }
}

fn dump_profile(paths: &AppPaths, root: &Path, out: &Path, mut manifest: Map<String, Value>) {
    let _ = fs::remove_dir_all(out);
    fs::create_dir_all(out).unwrap();
    let db = rusqlite::Connection::open(paths.database()).unwrap();
    db.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).unwrap();
    let version: i64 = db.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    let mut sql = String::from("-- Generated by scripts/generate-upgrade-fixtures.mjs; do not edit.\n");
    let objects: Vec<(String, String, String)> = db
        .prepare("SELECT type,name,sql FROM sqlite_master WHERE sql IS NOT NULL AND name NOT LIKE 'sqlite_%' ORDER BY rowid")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let mut counts = Map::new();
    for (kind, _, create) in &objects {
        if kind == "table" {
            sql.push_str(&format!("{create};\n"));
        }
    }
    let mut tables: Vec<String> = objects
        .iter()
        .filter(|(kind, _, _)| kind == "table")
        .map(|(_, name, _)| name.clone())
        .collect();
    if db
        .query_row("SELECT count(*) FROM sqlite_master WHERE name='sqlite_sequence'", [], |r| r.get::<_, i64>(0))
        .unwrap()
        > 0
    {
        tables.push("sqlite_sequence".into());
    }
    for table in &tables {
        let mut statement = db.prepare(&format!("SELECT * FROM \"{table}\" ORDER BY rowid")).unwrap();
        let columns = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        let mut count = 0;
        while let Some(row) = rows.next().unwrap() {
            let values: Vec<String> = (0..columns)
                .map(|i| sql_literal(row.get_ref(i).unwrap()))
                .collect();
            sql.push_str(&format!("INSERT INTO \"{table}\" VALUES({});\n", values.join(",")));
            count += 1;
        }
        if table != "sqlite_sequence" {
            counts.insert(table.clone(), json!(count));
        }
    }
    for (kind, _, create) in &objects {
        if kind != "table" {
            sql.push_str(&format!("{create};\n"));
        }
    }
    sql.push_str(&format!("PRAGMA user_version={version};\n"));
    fs::write(out.join("shadow-agent.sql"), portable(&sql, root)).unwrap();
    manifest.insert("schema_version".into(), json!(version));
    manifest.insert("row_counts".into(), Value::Object(counts));

    // Every other small text file the release left in the profile.
    let mut files = Vec::new();
    for (label, dir) in [("config", &paths.config), ("data", &paths.data), ("state", &paths.state)] {
        for entry in walk(dir) {
            let relative = entry.strip_prefix(dir).unwrap().to_string_lossy().into_owned();
            let name = entry.file_name().unwrap().to_string_lossy().into_owned();
            if name == "native.lock" || name.starts_with("shadow-agent.db") || name.contains("pre-native") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&entry) else { continue };
            if text.len() > 256 * 1024 {
                continue;
            }
            let target = out.join("profile").join(label).join(&relative);
            fs::create_dir_all(target.parent().unwrap()).unwrap();
            fs::write(&target, portable(&text, root)).unwrap();
            files.push(format!("{label}/{relative}"));
        }
    }
    files.sort();
    manifest.insert("files".into(), json!(files));
    fs::write(
        out.join("manifest.json"),
        portable(&serde_json::to_string_pretty(&Value::Object(manifest)).unwrap(), root) + "\n",
    )
    .unwrap();
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else { return found };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            found.extend(walk(&path));
        } else if kind.is_file() {
            found.push(path);
        }
    }
    found
}
