//! The update notice against a fake GitHub releases API on loopback
//! (`SHADOWCODE_UPDATE_API`). Every request the app sends is recorded, so
//! this checks the once-a-day schedule, the manual check, the offline and
//! turned-off paths, and that no identifier leaves the machine. Nothing here
//! reaches the real GitHub.
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
    updates,
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

const API_PATH: &str = "/repos/Shadowfetchapps/ShadowCode/releases/latest";

#[derive(Default)]
struct Fake {
    /// Raw request heads, in order.
    requests: Vec<String>,
    /// Status and body of the next answers.
    status: u16,
    body: String,
}

async fn serve(listener: TcpListener, fake: Arc<Mutex<Fake>>) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            return;
        };
        let fake = fake.clone();
        tokio::spawn(async move {
            let mut head = Vec::new();
            let mut buffer = [0u8; 4096];
            while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                match socket.read(&mut buffer).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => head.extend_from_slice(&buffer[..n]),
                }
            }
            let (status, body) = {
                let mut fake = fake.lock().unwrap();
                fake.requests
                    .push(String::from_utf8_lossy(&head).into_owned());
                (fake.status, fake.body.clone())
            };
            let response = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
    }
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

fn set_last_attempt(paths: &AppPaths, seconds_ago: f64) {
    let mut state = updates::load_state(paths);
    state.last_attempt = shadowcode_core::now() - seconds_ago;
    fs::write(
        paths.state.join("update-check.json"),
        serde_json::to_vec(&state).unwrap(),
    )
    .unwrap();
}

fn release_json(tag: &str) -> String {
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/updates/latest-signed.json");
    let mut value: Value = serde_json::from_str(&fs::read_to_string(fixture).unwrap()).unwrap();
    value["tag_name"] = json!(tag);
    value.to_string()
}

#[tokio::test(flavor = "multi_thread")]
async fn daily_and_manual_update_checks_respect_settings_and_send_no_identifiers() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let fake = Arc::new(Mutex::new(Fake {
        status: 200,
        body: release_json("v999.0.0"),
        ..Fake::default()
    }));
    tokio::spawn(serve(listener, fake.clone()));
    let requests = || fake.lock().unwrap().requests.clone();
    // Only this test binary sets these; it has one test.
    std::env::set_var(
        "SHADOWCODE_UPDATE_API",
        format!("http://127.0.0.1:{port}{API_PATH}"),
    );
    // A token in the environment must never be sent.
    std::env::set_var("GITHUB_TOKEN", "ghp_should_never_be_sent");
    for proxy in [
        "http_proxy",
        "https_proxy",
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "ALL_PROXY",
        "all_proxy",
    ] {
        std::env::remove_var(proxy);
    }

    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"cli_agents": {"enabled": false}})).unwrap();
    let service = Service::open(paths.clone(), Some(project)).unwrap();

    // Nothing is fetched until the window asks for its daily check.
    let status = call(&service, "GET", "/api/updates", Value::Null)
        .await
        .unwrap();
    assert!(requests().is_empty());
    assert_eq!(status["available"], false);
    assert_eq!(status["allowed"], true);
    assert_eq!(status["automatic"], true);
    assert!(status["last_checked_at"].is_null());

    let status = call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(requests().len(), 1, "{status}");
    assert_eq!(status["available"], true);
    assert_eq!(status["latest"]["version"], "999.0.0");
    assert_eq!(
        status["latest"]["url"],
        "https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v999.0.0"
    );
    assert!(status["last_checked_at"].as_f64().unwrap() > 0.0);
    assert!(status["error"].is_null());
    assert!(status["next_step"]["text"].is_string());

    // Exactly one fixed request: no query string, version, token or cookie.
    let head = &requests()[0];
    let mut lines = head.split("\r\n").filter(|line| !line.is_empty());
    assert_eq!(lines.next().unwrap(), format!("GET {API_PATH} HTTP/1.1"));
    let mut names = Vec::new();
    for line in lines {
        let (name, value) = line.split_once(':').unwrap();
        let name = name.trim().to_ascii_lowercase();
        let value = value.trim();
        match name.as_str() {
            "host" => assert_eq!(value, format!("127.0.0.1:{port}")),
            "user-agent" => assert_eq!(value, "ShadowCode-update-check"),
            "accept" => assert_eq!(value, "application/vnd.github+json"),
            "x-github-api-version" => assert_eq!(value, "2022-11-28"),
            other => panic!("unexpected header {other}: {value}"),
        }
        assert!(!value.contains(shadowcode_core::VERSION), "{line}");
        names.push(name);
    }
    assert!(names.contains(&"user-agent".to_owned()));
    assert!(!head.contains("ghp_should_never_be_sent"));

    // Once a day: the next window load and a manual check right away do not
    // ask again.
    call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    call(&service, "POST", "/api/updates/check", json!({}))
        .await
        .unwrap();
    assert_eq!(requests().len(), 1);
    // A manual check a minute later does.
    set_last_attempt(&paths, 60.0);
    call(&service, "POST", "/api/updates/check", json!({}))
        .await
        .unwrap();
    assert_eq!(requests().len(), 2);
    call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(requests().len(), 2);
    // 23 hours later the daily check still waits; a day later it runs.
    set_last_attempt(&paths, 23.0 * 3600.0);
    call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(requests().len(), 2);
    set_last_attempt(&paths, 24.0 * 3600.0);
    call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(requests().len(), 3);

    // A failed check is recorded quietly and keeps the last good answer.
    fake.lock().unwrap().status = 500;
    set_last_attempt(&paths, 2.0 * 86400.0);
    let status = call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(requests().len(), 4);
    assert_eq!(status["error"], "GitHub answered with HTTP 500");
    assert_eq!(status["available"], true);
    fake.lock().unwrap().status = 200;

    // Turned off by the user: no daily check, but Check now still works.
    let saved = call(
        &service,
        "PUT",
        "/api/config",
        json!({"values": {"updates": {"check": false}}}),
    )
    .await
    .unwrap();
    assert_eq!(saved["updates"]["check"], false);
    set_last_attempt(&paths, 2.0 * 86400.0);
    let status = call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(requests().len(), 4);
    assert_eq!(status["automatic"], false);
    assert_eq!(status["setting"], false);
    let status = call(&service, "POST", "/api/updates/check", json!({}))
        .await
        .unwrap();
    assert_eq!(requests().len(), 5);
    assert!(status["error"].is_null());

    // Offline mode: nothing at all, and Check now explains why.
    call(
        &service,
        "PUT",
        "/api/config",
        json!({"values": {"updates": {"check": true}, "network": {"mode": "offline"}}}),
    )
    .await
    .unwrap();
    set_last_attempt(&paths, 2.0 * 86400.0);
    let status = call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["offline"], true);
    let error = call(&service, "POST", "/api/updates/check", json!({}))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("Offline mode"), "{error}");
    assert_eq!(requests().len(), 5);
    // "Web tools off" still allows the check (it is not web browsing).
    call(
        &service,
        "PUT",
        "/api/config",
        json!({"values": {"network": {"mode": "web_off"}}}),
    )
    .await
    .unwrap();
    call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(requests().len(), 6);

    // Hiding the notice lasts until a newer version appears.
    let status = call(
        &service,
        "POST",
        "/api/updates/dismiss",
        json!({"version": "999.0.0"}),
    )
    .await
    .unwrap();
    assert_eq!(status["dismissed"], true);
    fake.lock().unwrap().body = release_json("v999.1.0");
    set_last_attempt(&paths, 2.0 * 86400.0);
    let status = call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["latest"]["version"], "999.1.0");
    assert_eq!(status["dismissed"], false);
    assert!(call(
        &service,
        "POST",
        "/api/updates/dismiss",
        json!({"version": "soon"})
    )
    .await
    .is_err());

    // A release that is not newer never shows a notice.
    fake.lock().unwrap().body = release_json("v0.0.1");
    set_last_attempt(&paths, 2.0 * 86400.0);
    let status = call(&service, "GET", "/api/updates?auto=1", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["latest"]["version"], "0.0.1");
    assert_eq!(status["available"], false);
    assert!(status["next_step"].is_null());

    // Settings › About.
    let about = call(&service, "GET", "/api/about", Value::Null)
        .await
        .unwrap();
    assert_eq!(about["name"], "ShadowCode");
    assert_eq!(about["version"], shadowcode_core::VERSION);
    // The test executable lives in target/debug/deps.
    assert_eq!(about["install"]["kind"], "source");
    assert_eq!(about["install"]["label"], "Built from source");
    assert_eq!(about["license"]["spdx"], "Apache-2.0");
    assert!(about["license"]["notice"]
        .as_str()
        .unwrap()
        .contains("originally created by Shadowfetch"));
    assert_eq!(
        about["links"]["release_notes"],
        format!(
            "https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v{}",
            shadowcode_core::VERSION
        )
    );
    assert_eq!(about["updates"]["current"], shadowcode_core::VERSION);
    // About never reaches the network by itself.
    assert_eq!(requests().len(), 8);
}
