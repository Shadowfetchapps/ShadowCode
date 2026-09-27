//! Real OS-process qualification: engine death, persistence, and no silent replay.
//! These spawn `target/debug/shadowcode`, not an in-process Server.
#![cfg(unix)]
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config, control::Endpoint, paths::AppPaths, service::Request, store::Store,
};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

fn binary() -> PathBuf {
    std::env::var_os("SHADOW_DESKTOP_BINARY")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/shadowcode")
        })
}

fn request(method: &str, path: &str, body: Value) -> Request {
    Request {
        method: method.into(),
        path: path.into(),
        body,
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_serve(profile: &Path, workspace: &Path) -> ChildGuard {
    let child = Command::new(binary())
        .args([
            "--profile",
            profile.to_str().unwrap(),
            "--workspace",
            workspace.to_str().unwrap(),
            "--json",
            "serve",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn shadowcode serve");
    ChildGuard(child)
}

fn dead(pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}")).exists()
}

fn rss_kb(pid: u32) -> u64 {
    fs::read_to_string(format!("/proc/{pid}/status"))
        .ok()
        .and_then(|text| {
            text.lines()
                .find(|line| line.starts_with("VmRSS:"))
                .and_then(|line| line.split_whitespace().nth(1).and_then(|v| v.parse().ok()))
        })
        .unwrap_or(0)
}

fn fd_count(pid: u32) -> usize {
    fs::read_dir(format!("/proc/{pid}/fd"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

async fn wait_client(
    client: &shadowcode_core::control::Client,
    timeout: Duration,
) -> anyhow::Result<()> {
    client.wait_available(timeout).await
}

#[tokio::test]
async fn real_engine_sigkill_reattaches_without_replay_or_duplicate_events() {
    assert!(
        binary().is_file(),
        "missing {}; build shadowcode first",
        binary().display()
    );
    let model = support::server(|_, body| {
        let had_tool = body["messages"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|m| m["role"] == "tool");
        let message = if had_tool {
            json!({"role":"assistant","content":"Read the fixture."})
        } else {
            json!({"role":"assistant","content":"Inspecting.","tool_calls":[{"id":"r1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]})
        };
        let reason = if message.get("tool_calls").is_some() {
            "tool_calls"
        } else {
            "stop"
        };
        (
            json!({"choices":[{"message":message,"finish_reason":reason}]}),
            Duration::ZERO,
        )
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    let profile = root.path().join("profile");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("README.md"), "fixture-read-value\n").unwrap();
    let paths = AppPaths::isolated(&profile).unwrap();
    Config::patch(
        &paths,
        json!({
            "trusted_workspaces":[workspace],
            "model":{"provider":"local","name":"fixture","default":"fixture","endpoint":model.endpoint,"context_limit":16384},
            "onboarding":{"completed":true,"workspace":workspace},
            "permissions":{"approve_shell":false},
            "ui":{"notify":false}
        }),
    )
    .unwrap();

    let mut owner = spawn_serve(&profile, &workspace);
    let client = Endpoint::for_paths(&paths)
        .unwrap()
        .client(workspace.clone(), None);
    wait_client(&client, Duration::from_secs(15)).await.unwrap();
    let view = client.open_view().await.unwrap();
    let mut events = view.subscribe();
    let started = view
        .dispatch(request(
            "POST",
            "/api/jobs",
            json!({"task":"READ the fixture once"}),
        ))
        .await
        .unwrap();
    let session_id = started["session_id"].as_str().unwrap().to_owned();
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let event = events.recv().await.unwrap();
            if event["type"] == "agent.completed" {
                break;
            }
        }
    })
    .await
    .expect("task should finish before engine kill");
    let before = view
        .dispatch(request(
            "GET",
            &format!("/api/events?session_id={session_id}"),
            Value::Null,
        ))
        .await
        .unwrap();
    let before_ids: Vec<i64> = before["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["id"].as_i64())
        .collect();
    assert!(!before_ids.is_empty(), "task produced no events");
    let pid = owner.0.id();
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline && !dead(pid) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(dead(pid), "engine PID {pid} still alive after SIGKILL");

    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let event = events.recv().await.unwrap();
            if event["type"] == "view.disconnected" {
                break;
            }
        }
    })
    .await
    .ok();

    let mut owner = spawn_serve(&profile, &workspace);
    wait_client(&client, Duration::from_secs(20)).await.unwrap();
    let attached = view.reattach().await.unwrap();
    assert_eq!(attached["reattached"], true);
    assert_eq!(attached["jobs_started"], 0);
    assert_eq!(attached["tools_replayed"], 0);
    assert!(
        !model.requests.lock().unwrap().is_empty(),
        "fixture model must have received the original task"
    );
    let after = view
        .dispatch(request(
            "GET",
            &format!("/api/events?session_id={session_id}"),
            Value::Null,
        ))
        .await
        .unwrap();
    let after_ids: Vec<i64> = after["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|event| event["id"].as_i64())
        .collect();
    for id in &before_ids {
        assert!(after_ids.contains(id), "lost event {id}");
    }
    let unique: std::collections::HashSet<_> = after_ids.iter().collect();
    assert_eq!(unique.len(), after_ids.len(), "duplicate event ids");
    let jobs = view
        .dispatch(request("GET", "/api/jobs", Value::Null))
        .await
        .unwrap();
    let running = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|job| job["status"] == "running" || job["status"] == "queued")
        .count();
    assert_eq!(running, 0, "reattach must not start or resume jobs");
    let health = view
        .dispatch(request("GET", "/api/health", Value::Null))
        .await
        .unwrap();
    assert_eq!(health["ok"], true);
    owner.0.kill().ok();
    let _ = owner.0.wait();
}

/// A concurrent, recording crash fixture. The first response authorizes one
/// write; every later response is held while the listener continues recording
/// requests. Waiting for the second complete request makes the kill boundary
/// unambiguous: the old engine is already waiting for a reply it never receives.
struct CrashModel {
    endpoint: String,
    requests: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
    second_entered: tokio::sync::oneshot::Receiver<()>,
    cancel: tokio_util::sync::CancellationToken,
    worker: Option<tokio::task::JoinHandle<anyhow::Result<()>>>,
}
impl CrashModel {
    async fn start() -> Self {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let captured = requests.clone();
        let (entered, second_entered) = tokio::sync::oneshot::channel();
        let entered = std::sync::Arc::new(std::sync::Mutex::new(Some(entered)));
        let cancel = tokio_util::sync::CancellationToken::new();
        let stopped = cancel.clone();
        let worker = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    biased;
                    _ = stopped.cancelled() => {
                        connections.abort_all();
                        while connections.join_next().await.is_some() {}
                        return Ok(());
                    }
                    done = connections.join_next(), if !connections.is_empty() => {
                        done.expect("nonempty connection set")??;
                    }
                    accepted = listener.accept() => {
                        let (mut socket, _) = accepted?;
                        anyhow::ensure!(connections.len() < 8, "Crash fixture exceeded its concurrent connection bound");
                        let captured = captured.clone();
                        let entered = entered.clone();
                        let stopped = stopped.clone();
                        connections.spawn(async move {
                            let body = tokio::time::timeout(Duration::from_secs(10), async {
                                let mut wire = Vec::new();
                                let mut buffer = [0; 8192];
                                loop {
                                    let count = socket.read(&mut buffer).await?;
                                    anyhow::ensure!(count > 0, "Crash fixture request ended before its body");
                                    wire.extend_from_slice(&buffer[..count]);
                                    anyhow::ensure!(wire.len() < 16_000_000, "Crash fixture request exceeded its byte bound");
                                    if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
                                        let headers = String::from_utf8_lossy(&wire[..end]).to_lowercase();
                                        let len = headers.lines().find_map(|line| line.strip_prefix("content-length:")
                                            .and_then(|value| value.trim().parse::<usize>().ok()))
                                            .ok_or_else(|| anyhow::anyhow!("Crash fixture needs Content-Length"))?;
                                        anyhow::ensure!(len < 16_000_000, "Crash fixture declared body exceeded its byte bound");
                                        if wire.len() >= end + 4 + len {
                                            return Ok::<Value, anyhow::Error>(serde_json::from_slice(&wire[end + 4..end + 4 + len])?);
                                        }
                                    }
                                }
                            }).await??;
                            let index = {
                                let mut requests = captured.lock().unwrap();
                                anyhow::ensure!(requests.len() < 8, "Crash fixture exceeded its retained request bound");
                                let index = requests.len();
                                requests.push(body);
                                index
                            };
                            if index == 0 {
                                let text = json!({"choices":[{"message":{"role":"assistant","content":"Writing.","tool_calls":[{"id":"w1","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"crash.txt\",\"content\":\"once\\n\",\"expected_hash\":\"missing\"}"}}]},"finish_reason":"tool_calls"}]}).to_string();
                                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len()).as_bytes()).await?;
                            } else {
                                if index == 1 {
                                    if let Some(entered) = entered.lock().unwrap().take() { let _ = entered.send(()); }
                                }
                                // Do not complete this response, and do not stop
                                // accepting other connections while it is held.
                                stopped.cancelled().await;
                            }
                            Ok::<(), anyhow::Error>(())
                        });
                    }
                }
            }
        });
        Self {
            endpoint,
            requests,
            second_entered,
            cancel,
            worker: Some(worker),
        }
    }
    async fn shutdown(&mut self) {
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            worker.await.unwrap().unwrap();
        }
    }
}
impl Drop for CrashModel {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(worker) = self.worker.take() {
            worker.abort();
        }
    }
}

#[tokio::test]
async fn crash_fixture_observes_replay_while_the_original_response_is_held() {
    let mut model = CrashModel::start().await;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let url = format!("{}/chat/completions", model.endpoint);
    let first = client
        .post(&url)
        .json(&json!({"messages":[]}))
        .send()
        .await
        .unwrap();
    assert!(first.status().is_success());
    let reply: Value = first.json().await.unwrap();
    assert_eq!(reply["choices"][0]["message"]["tool_calls"][0]["id"], "w1");
    let second_client = client.clone();
    let second_url = url.clone();
    let second = tokio::spawn(async move {
        second_client
            .post(second_url)
            .json(&json!({"messages":[{"role":"tool","tool_call_id":"w1"}]}))
            .send()
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), &mut model.second_entered)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(model.requests.lock().unwrap().len(), 2);
    assert!(!second.is_finished(), "Original request must still be held");
    let third = tokio::spawn(async move {
        client
            .post(url)
            .json(&json!({"messages":[{"role":"user","content":"deliberate replay sentinel"}]}))
            .send()
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while model.requests.lock().unwrap().len() < 3 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("A held response must not hide a new request");
    assert_eq!(
        model.requests.lock().unwrap()[2]["messages"][0]["content"],
        "deliberate replay sentinel"
    );
    assert!(!second.is_finished() && !third.is_finished());
    second.abort();
    third.abort();
    let _ = second.await;
    let _ = third.await;
    model.shutdown().await;
}

#[tokio::test]
async fn real_engine_kill_during_active_task_marks_interrupted_and_does_not_replay_write() {
    assert!(binary().is_file());
    let mut model = CrashModel::start().await;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    let profile = root.path().join("profile");
    fs::create_dir_all(&workspace).unwrap();
    fs::write(workspace.join("README.md"), "ok\n").unwrap();
    let paths = AppPaths::isolated(&profile).unwrap();
    Config::patch(
        &paths,
        json!({
            "trusted_workspaces":[workspace],
            "model":{"provider":"local","name":"fixture","default":"fixture","endpoint":model.endpoint,"context_limit":16384},
            "onboarding":{"completed":true},
            "permissions":{"approve_shell":false},
            "ui":{"notify":false}
        }),
    )
    .unwrap();
    let mut owner = spawn_serve(&profile, &workspace);
    let client = Endpoint::for_paths(&paths)
        .unwrap()
        .client(workspace.clone(), None);
    wait_client(&client, Duration::from_secs(15)).await.unwrap();
    let view = client.open_view().await.unwrap();
    view.dispatch(request(
        "POST",
        "/api/jobs",
        json!({"task":"WRITE crash.txt then hang"}),
    ))
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(15), async {
        loop {
            if workspace.join("crash.txt").is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        (&mut model.second_entered)
            .await
            .expect("Crash fixture ended before the held request");
    })
    .await
    .expect("write_file and the original engine's held follow-up request must precede SIGKILL");
    let first = fs::read_to_string(workspace.join("crash.txt")).unwrap();
    let requests_before_kill = {
        let requests = model.requests.lock().unwrap();
        assert_eq!(
            requests.len(),
            2,
            "Unexpected original engine requests: {requests:?}"
        );
        assert!(
            requests[1]["messages"]
                .as_array()
                .unwrap()
                .iter()
                .any(|message| message["role"] == "tool" && message["tool_call_id"] == "w1"),
            "Held request must follow the observed write tool: {:?}",
            requests[1]
        );
        requests.len()
    };
    let killed_pid = owner.0.id();
    owner.0.kill().unwrap();
    owner.0.wait().unwrap();
    assert!(dead(killed_pid), "Old engine must be reaped before restart");

    let mut owner = spawn_serve(&profile, &workspace);
    wait_client(&client, Duration::from_secs(20)).await.unwrap();
    let jobs = client
        .dispatch(request("GET", "/api/jobs", Value::Null))
        .await
        .unwrap();
    let statuses: Vec<String> = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|job| job["status"].as_str().unwrap_or("").to_owned())
        .collect();
    assert!(
        statuses
            .iter()
            .all(|s| !matches!(s.as_str(), "running" | "queued")),
        "recovered jobs must not still be running: {statuses:?}"
    );
    assert!(
        statuses.iter().any(|s| s == "interrupted"),
        "expected interrupted job, got {statuses:?}"
    );
    assert_eq!(
        fs::read_to_string(workspace.join("crash.txt")).unwrap(),
        first
    );
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert_eq!(
        model.requests.lock().unwrap().len(),
        requests_before_kill,
        "engine restart must not send another model request while the original response stays held"
    );
    model.shutdown().await;
    let store = Store::open(&paths.database()).unwrap();
    let stats = store.local_stats().unwrap();
    assert_eq!(stats["telemetry"], false);
    owner.0.kill().ok();
    let _ = owner.0.wait();
}

#[tokio::test]
async fn real_engine_reattach_cycles_keep_history_and_resources_bounded() {
    assert!(binary().is_file());
    let model = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"role":"assistant","content":"ok"},"finish_reason":"stop"}]}),
            Duration::ZERO,
        )
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    let profile = root.path().join("profile");
    fs::create_dir_all(&workspace).unwrap();
    let paths = AppPaths::isolated(&profile).unwrap();
    Config::patch(
        &paths,
        json!({
            "trusted_workspaces":[workspace],
            "model":{"provider":"local","name":"fixture","default":"fixture","endpoint":model.endpoint,"context_limit":4096},
            "onboarding":{"completed":true},
            "ui":{"notify":false}
        }),
    )
    .unwrap();
    let mut samples = Vec::new();
    let mut first_session = String::new();
    let mut first_ids = Vec::new();
    for cycle in 0..3 {
        let mut owner = spawn_serve(&profile, &workspace);
        let client = Endpoint::for_paths(&paths)
            .unwrap()
            .client(workspace.clone(), None);
        wait_client(&client, Duration::from_secs(15)).await.unwrap();
        let view = client.open_view().await.unwrap();
        let started = view
            .dispatch(request(
                "POST",
                "/api/jobs",
                json!({"task": format!("cycle {cycle}")}),
            ))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        if first_session.is_empty() {
            first_session = started["session_id"].as_str().unwrap().to_owned();
            first_ids = view
                .dispatch(request(
                    "GET",
                    &format!("/api/events?session_id={first_session}"),
                    Value::Null,
                ))
                .await
                .unwrap()["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|e| e["id"].as_i64())
                .collect();
        } else {
            let page = view
                .dispatch(request(
                    "GET",
                    &format!("/api/events?session_id={first_session}"),
                    Value::Null,
                ))
                .await
                .unwrap();
            let ids: Vec<i64> = page["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|e| e["id"].as_i64())
                .collect();
            for id in &first_ids {
                assert!(ids.contains(id), "cycle {cycle} lost event {id}");
            }
        }
        let pid = owner.0.id();
        samples.push((rss_kb(pid), fd_count(pid)));
        view.close().await.unwrap();
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
    }
    let first = samples[0].1;
    let last = samples[2].1;
    assert!(
        last <= first + 32,
        "FD growth across reattach cycles: {samples:?}"
    );
}

#[test]
fn store_query_plans_use_existing_session_indexes() {
    let root = tempfile::tempdir().unwrap();
    let store = Store::open(&root.path().join("db")).unwrap();
    let session = store.create_session(root.path(), "mock", "plans").unwrap();
    let sid = session["id"].as_str().unwrap();
    for i in 0..2_000 {
        store
            .add_event("model.delta", &json!({"i": i}), Some(sid), None)
            .unwrap();
    }
    let db_path = store.path.clone();
    drop(store);
    let plan = |sql: &str| {
        let out = Command::new("sqlite3")
            .args([
                db_path.to_str().unwrap(),
                &format!("EXPLAIN QUERY PLAN {sql}"),
            ])
            .output()
            .expect("sqlite3 CLI for query plans");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let recent = plan(&format!(
        "SELECT * FROM events WHERE session_id='{sid}' AND id<=999999 ORDER BY id DESC LIMIT 20"
    ));
    let after = plan(&format!(
        "SELECT * FROM events WHERE session_id='{sid}' AND id>0 AND id<=999999 ORDER BY id LIMIT 200"
    ));
    eprintln!("query_plan recent={recent}");
    eprintln!("query_plan after={after}");
    assert!(
        recent.to_ascii_lowercase().contains("events_session_id")
            || recent.to_ascii_lowercase().contains("using index")
            || recent.to_ascii_lowercase().contains("search"),
        "recent_events should use (session_id,id): {recent}"
    );
    assert!(
        after.to_ascii_lowercase().contains("events_session_id")
            || after.to_ascii_lowercase().contains("using index")
            || after.to_ascii_lowercase().contains("search"),
        "events_after should use (session_id,id): {after}"
    );
}
