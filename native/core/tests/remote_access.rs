//! Remote access over real HTTP: pairing, token checks, the failed-attempt
//! limit, same-origin rules, static files, the route policy, secret
//! redaction, the event stream, and phone notifications against a local
//! fake ntfy server. Nothing leaves this computer.
#![cfg(unix)]
use futures_util::StreamExt;
use serde_json::{json, Value};
use shadowcode_core::{
    paths::AppPaths,
    remote::{assets, auth, Manager},
    service::{Request, Service},
};
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex, Once},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_util::sync::CancellationToken;

struct Fixture {
    _dir: tempfile::TempDir,
    service: Service,
    manager: Arc<Manager>,
    address: SocketAddr,
    base: String,
    workspace: std::path::PathBuf,
}

/// The web interface files every test server serves (one per test binary).
fn ui_files() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("shadowcode-remote-ui-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("assets")).unwrap();
        std::fs::write(
            dir.join("index.html"),
            "<!doctype html><title>ShadowCode</title>",
        )
        .unwrap();
        std::fs::write(dir.join("assets/app-abc.js"), "console.log('ui')").unwrap();
        std::fs::write(dir.join("manifest.webmanifest"), "{}").unwrap();
        // A file next to the UI folder that must never be served.
        std::fs::write(
            dir.parent()
                .unwrap()
                .join(format!("shadowcode-remote-secret-{}", std::process::id())),
            "private",
        )
        .unwrap();
        assets::set_bundled(Arc::new(assets::DirAssets::new(&dir).unwrap()));
    });
}

async fn fixture() -> Fixture {
    ui_files();
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(&dir.path().join("profile")).unwrap();
    let workspace = dir.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    std::fs::write(workspace.join(".env"), "TOKEN=hidden").unwrap();
    std::fs::write(workspace.join("notes.txt"), "hello").unwrap();
    let service = Service::open(paths, Some(workspace.clone())).unwrap();
    let manager = service.remote().clone();
    let address = manager
        .start(&service, Some("127.0.0.1:0".parse().unwrap()))
        .unwrap();
    Fixture {
        base: format!("http://{address}"),
        _dir: dir,
        service,
        manager,
        address,
        workspace,
    }
}

impl Fixture {
    /// Pair a device the way the web interface does, returning its token.
    async fn pair(&self) -> String {
        let link = self.manager.pair(None).unwrap()["link"]
            .as_str()
            .unwrap()
            .to_owned();
        let code = link.split("#pair=").nth(1).unwrap().to_owned();
        let response = reqwest::Client::new()
            .post(format!("{}/_remote/pair", self.base))
            .json(&json!({"code": code, "name": "Test phone"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["device"]["name"], "Test phone");
        body["token"].as_str().unwrap().to_owned()
    }
    async fn api(
        &self,
        token: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let client = reqwest::Client::new();
        let mut request = client
            .request(method.parse().unwrap(), format!("{}{path}", self.base))
            .bearer_auth(token)
            .header("x-shadow-view", "test-view-0001");
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request.send().await.unwrap();
        let status = response.status().as_u16();
        (status, response.json().await.unwrap_or(Value::Null))
    }
}

/// One raw HTTP/1.1 request (reqwest normalizes paths, so traversal
/// attempts are written by hand).
async fn raw(address: SocketAddr, request: &str) -> String {
    let mut stream = TcpStream::connect(address).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut out = Vec::new();
    let _ = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut out)).await;
    String::from_utf8_lossy(&out).into_owned()
}

#[tokio::test]
async fn tokens_are_required_and_checked() {
    let f = fixture().await;
    let client = reqwest::Client::new();
    // Missing token.
    let response = client
        .get(format!("{}/api/version", f.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    assert!(response.headers().contains_key("www-authenticate"));
    // Wrong token of the right shape.
    let wrong = auth::new_token().unwrap();
    let (status, _) = f.api(&wrong, "GET", "/api/version", None).await;
    assert_eq!(status, 401);
    // The right token.
    let token = f.pair().await;
    let (status, body) = f.api(&token, "GET", "/api/version", None).await;
    assert_eq!(status, 200);
    assert_eq!(body["name"], "ShadowCode");
    let (status, session) = f.api(&token, "GET", "/_remote/session", None).await;
    assert_eq!(status, 200);
    assert_eq!(session["allow_terminals"], false);
    // Tokens are stored as digests only, in a private file.
    let saved =
        std::fs::read_to_string(f.service.engine.paths().config.join("remote.json")).unwrap();
    assert!(!saved.contains(&token));
    assert!(saved.contains(&auth::hex(&auth::digest(&token))));
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(f.service.engine.paths().config.join("remote.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    // A pairing link works once.
    let link = f.manager.pair(None).unwrap()["link"]
        .as_str()
        .unwrap()
        .to_owned();
    let code = link.split("#pair=").nth(1).unwrap();
    for expected in [200, 401] {
        let response = client
            .post(format!("{}/_remote/pair", f.base))
            .json(&json!({"code": code}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
    }
    // Revoking the device ends its access.
    let device = f.manager.status()["devices"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    f.manager.revoke(Some(&device)).unwrap();
    let (status, _) = f.api(&token, "GET", "/api/version", None).await;
    assert_eq!(status, 401);
    f.manager.stop();
}

#[tokio::test]
async fn repeated_failures_are_rate_limited() {
    let f = fixture().await;
    let token = f.pair().await;
    let wrong = auth::new_token().unwrap();
    let mut statuses = Vec::new();
    for _ in 0..auth::MAX_FAILURES + 1 {
        statuses.push(f.api(&wrong, "GET", "/api/version", None).await.0);
    }
    assert!(statuses[..auth::MAX_FAILURES as usize]
        .iter()
        .all(|s| *s == 401));
    assert_eq!(*statuses.last().unwrap(), 429);
    // While blocked, even the right token is not checked.
    assert_eq!(f.api(&token, "GET", "/api/version", None).await.0, 429);
    // Pairing attempts count too.
    let response = reqwest::Client::new()
        .post(format!("{}/_remote/pair", f.base))
        .json(&json!({"code": "guess"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 429);
    f.manager.stop();
}

#[tokio::test]
async fn static_files_refuse_traversal() {
    let f = fixture().await;
    let index = reqwest::get(format!("{}/", f.base)).await.unwrap();
    assert_eq!(index.status(), 200);
    let csp = index.headers()["content-security-policy"]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(csp.contains("frame-ancestors 'none'") && csp.contains("connect-src 'self'"));
    assert_eq!(index.headers()["x-content-type-options"], "nosniff");
    assert!(index.text().await.unwrap().contains("ShadowCode"));
    let script = reqwest::get(format!("{}/assets/app-abc.js", f.base))
        .await
        .unwrap();
    assert_eq!(script.status(), 200);
    assert!(script.headers()["cache-control"]
        .to_str()
        .unwrap()
        .contains("immutable"));
    let secret = format!("shadowcode-remote-secret-{}", std::process::id());
    for path in [
        format!("/../{secret}"),
        format!("/assets/../../{secret}"),
        format!("/%2e%2e/{secret}"),
        "/.env".to_owned(),
        "/assets/..%2f..%2findex.html".to_owned(),
    ] {
        let response = raw(
            f.address,
            &format!(
                "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                f.address
            ),
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
        assert!(!response.contains("private"));
    }
    f.manager.stop();
}

#[tokio::test]
async fn cross_origin_requests_are_refused() {
    let f = fixture().await;
    let token = f.pair().await;
    let client = reqwest::Client::new();
    let preflight = client
        .request(reqwest::Method::OPTIONS, format!("{}/api/version", f.base))
        .header("origin", "http://evil.example")
        .header("access-control-request-method", "GET")
        .send()
        .await
        .unwrap();
    assert_eq!(preflight.status(), 403);
    assert!(!preflight
        .headers()
        .contains_key("access-control-allow-origin"));
    let foreign = client
        .get(format!("{}/api/version", f.base))
        .bearer_auth(&token)
        .header("origin", "http://evil.example")
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), 403);
    assert!(!foreign
        .headers()
        .contains_key("access-control-allow-origin"));
    let same = client
        .get(format!("{}/api/version", f.base))
        .bearer_auth(&token)
        .header("origin", f.base.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(same.status(), 200);
    // Bodies must be JSON (a form post cannot reach the API).
    let form = client
        .post(format!("{}/api/sessions", f.base))
        .bearer_auth(&token)
        .header("content-type", "application/x-www-form-urlencoded")
        .body("workspace=/tmp")
        .send()
        .await
        .unwrap();
    assert_eq!(form.status(), 415);
    f.manager.stop();
}

#[tokio::test]
async fn terminals_are_blocked_until_allowed() {
    let f = fixture().await;
    let token = f.pair().await;
    let (status, body) = f.api(&token, "GET", "/api/terminals", None).await;
    assert_eq!(status, 403);
    assert!(body["error"]
        .as_str()
        .unwrap()
        .contains("Terminals are turned off"));
    let (status, _) = f
        .api(
            &token,
            "POST",
            "/api/workspace/exec",
            Some(json!({"command":"id"})),
        )
        .await;
    assert_eq!(status, 403);
    let (status, _) = f.api(&token, "GET", "/api/remote", None).await;
    assert_eq!(status, 403, "remote clients cannot manage remote access");
    // The desktop turns terminals on for remote access.
    f.service
        .dispatch(Request {
            method: "PUT".into(),
            path: "/api/remote".into(),
            body: json!({"allow_terminals": true}),
        })
        .await
        .unwrap();
    let (status, body) = f.api(&token, "GET", "/api/terminals", None).await;
    assert_eq!(status, 200, "{body}");
    f.manager.stop();
}

#[tokio::test]
async fn background_commands_require_remote_terminal_permission() {
    let f = fixture().await;
    // Trust the project so only the remote permission can prevent execution.
    shadowcode_core::config::Config::patch(
        f.service.engine.paths(),
        json!({"trusted_workspaces": [f.workspace]}),
    )
    .unwrap();
    let token = f.pair().await;
    let command =
        json!({"name":"remote policy probe", "command":"printf allowed > remote-marker.txt"});
    let (status, body) = f
        .api(&token, "POST", "/api/background", Some(command.clone()))
        .await;
    assert_eq!(status, 403, "{body}");
    assert!(!f.workspace.join("remote-marker.txt").exists());
    let tasks = f
        .service
        .dispatch(Request {
            method: "GET".into(),
            path: "/api/background".into(),
            body: Value::Null,
        })
        .await
        .unwrap();
    assert_eq!(
        tasks["tasks"].as_array().unwrap().len(),
        0,
        "Denied requests must not register a process"
    );

    f.service
        .dispatch(Request {
            method: "PUT".into(),
            path: "/api/remote".into(),
            body: json!({"allow_terminals": true}),
        })
        .await
        .unwrap();
    let (status, body) = f
        .api(&token, "POST", "/api/background", Some(command))
        .await;
    assert_eq!(status, 200, "{body}");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if std::fs::read_to_string(f.workspace.join("remote-marker.txt"))
                .ok()
                .as_deref()
                == Some("allowed")
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("Allowed remote command did not run");
    f.manager.stop();
}

#[tokio::test]
async fn secrets_are_not_shown_remotely() {
    let f = fixture().await;
    let token = f.pair().await;
    let (status, body) = f
        .api(
            &token,
            "POST",
            "/api/projects/trust",
            Some(json!({"path": f.workspace})),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = f
        .api(&token, "GET", "/api/workspace/file?path=.env", None)
        .await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = f
        .api(&token, "GET", "/api/workspace/file?path=notes.txt", None)
        .await;
    assert_eq!((status, body["content"].as_str()), (200, Some("hello")));
    // The profile's own folder cannot be opened as a project.
    let config = f.service.engine.paths().config.clone();
    let (status, _) = f
        .api(
            &token,
            "POST",
            "/api/projects",
            Some(json!({"path": config})),
        )
        .await;
    assert_eq!(status, 403);
    // Recognizable keys in responses are replaced, and the placeholder is
    // never accepted back.
    let key = format!("{}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz012345");
    std::fs::write(f.workspace.join("deploy.txt"), format!("push with {key}\n")).unwrap();
    let (status, body) = f
        .api(&token, "GET", "/api/workspace/file?path=deploy.txt", None)
        .await;
    assert_eq!(status, 200);
    let content = body["content"].as_str().unwrap();
    assert!(!content.contains(&key) && content.contains("[redacted secret]"));
    let (status, _) = f
        .api(
            &token,
            "PUT",
            "/api/workspace/instructions",
            Some(json!({"content": content})),
        )
        .await;
    assert_eq!(status, 403);
    // Remote navigation never moves the desktop's own project.
    assert_ne!(f.service.workspace().unwrap(), config);
    f.manager.stop();
}

#[tokio::test]
async fn event_stream_delivers_wakeups() {
    let f = fixture().await;
    let token = f.pair().await;
    // Streams need the token too.
    let refused = reqwest::get(format!("{}/_remote/stream", f.base))
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);
    let response = reqwest::Client::new()
        .get(format!("{}/_remote/stream", f.base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let mut chunks = response.bytes_stream();
    let mut text = String::new();
    // A new stream starts with one untyped wake-up (re-read everything).
    while !text.contains("shadowcode:events") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), chunks.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    f.service
        .engine
        .notifier()
        .send(json!({"type":"approval.requested","session_id":"s-1","payload":{"command":"secret-command"}}))
        .unwrap();
    f.service
        .engine
        .notifier()
        .send(json!({"type":"terminal.output","terminal_id":"a".repeat(32)}))
        .unwrap();
    while !text.contains("approval.requested") {
        let chunk = tokio::time::timeout(Duration::from_secs(5), chunks.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        text.push_str(&String::from_utf8_lossy(&chunk));
    }
    assert!(text.contains(r#"data: {"session_id":"s-1","type":"approval.requested"}"#));
    assert!(!text.contains("secret-command"), "payloads never travel");
    assert!(!text.contains("shadowcode:terminal"), "terminals are off");
    f.manager.stop();
}

/// A one-request-at-a-time HTTP server that records what it receives.
async fn fake_ntfy() -> (String, Arc<Mutex<Vec<(String, Value)>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = stream.read(&mut buf).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n]);
                let text = String::from_utf8_lossy(&data);
                if let Some((head, body)) = text.split_once("\r\n\r\n") {
                    let length = head
                        .lines()
                        .find_map(|l| {
                            l.to_ascii_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                        })
                        .unwrap_or(0);
                    if body.len() >= length {
                        let value: Value =
                            serde_json::from_str(&body[..length]).unwrap_or(Value::Null);
                        log.lock().unwrap().push((head.to_owned(), value));
                        break;
                    }
                }
            }
            let _ = stream
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\nconnection: close\r\n\r\n{}")
                .await;
        }
    });
    (format!("http://{address}"), seen)
}

#[tokio::test]
async fn ntfy_messages_follow_the_shared_decision() {
    let f = fixture().await;
    let (server, seen) = fake_ntfy().await;
    let cancel = CancellationToken::new();
    f.manager.activate(&f.service, cancel.clone());
    // Nothing is sent before a server and topic are configured.
    f.service
        .engine
        .notifier()
        .send(json!({"type":"agent.completed","session_id":"s","task_id":"t","payload":{"success":false,"summary":"boom"}}))
        .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(seen.lock().unwrap().is_empty());

    f.manager
        .set_ntfy(&json!({"server": server, "topic": "shadow-test", "token": "tk_test_value", "events": {"finished": false}}))
        .unwrap();
    assert_eq!(f.manager.status()["ntfy"]["token_saved"], true);
    assert!(!f.manager.status().to_string().contains("tk_test_value"));
    f.manager.test_ntfy().await.unwrap();
    // A finished task is switched off; a failure and an approval are sent.
    for event in [
        json!({"type":"agent.completed","session_id":"s","task_id":"t1","payload":{"success":true,"summary":"done"}}),
        json!({"type":"agent.completed","session_id":"s","task_id":"t2","payload":{"success":false,"summary":"boom"}}),
        json!({"type":"approval.requested","session_id":"s","payload":{"command":"rm -rf build","tool":"run_command"}}),
    ] {
        f.service.engine.notifier().send(event).unwrap();
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while seen.lock().unwrap().len() < 3 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "{seen:?}");
    let (head, test) = &seen[0];
    assert!(head.starts_with("POST / HTTP/1.1"));
    assert!(head
        .to_ascii_lowercase()
        .contains("authorization: bearer tk_test_value"));
    assert_eq!(test["topic"], "shadow-test");
    assert_eq!(test["message"], "Phone notifications are working.");
    let mut rest: Vec<&Value> = seen[1..].iter().map(|(_, v)| v).collect();
    rest.sort_by_key(|v| v["title"].as_str().unwrap_or("").to_owned());
    assert!(rest[0]["title"]
        .as_str()
        .unwrap()
        .contains("approval needed"));
    assert_eq!(rest[0]["message"], "A task is waiting for your decision.");
    assert!(
        !rest[0].to_string().contains("rm -rf"),
        "details are off by default"
    );
    assert!(rest[1]["title"].as_str().unwrap().contains("task failed"));
    // The link goes back to the conversation through the running server.
    assert_eq!(rest[1]["click"], format!("{}/#session=s", f.base).as_str());
    cancel.cancel();
    f.manager.stop();
}

/// Moving the server from this computer only to every address on the same
/// port restarts it there, instead of failing because the old server still
/// held the port. The runtime's only worker is kept busy during the change,
/// so the old server cannot close its socket on its own first.
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn changing_the_address_restarts_on_the_same_port() {
    ui_files();
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(&dir.path().join("profile")).unwrap();
    let workspace = dir.path().join("project");
    std::fs::create_dir_all(&workspace).unwrap();
    let service = Service::open(paths, Some(workspace)).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let status = service
        .dispatch(Request {
            method: "PUT".into(),
            path: "/api/remote".into(),
            body: json!({"enabled": true, "address": "127.0.0.1", "port": port}),
        })
        .await
        .unwrap();
    assert_eq!(status["error"], Value::Null, "{status}");
    assert_eq!(
        service.remote().address(),
        Some(SocketAddr::from(([127, 0, 0, 1], port)))
    );
    let (started, busy) = std::sync::mpsc::channel();
    let hold = tokio::spawn(async move {
        started.send(()).unwrap();
        std::thread::sleep(Duration::from_millis(700));
    });
    busy.recv().unwrap();
    let (manager, owner) = (service.remote().clone(), service.clone());
    let status = tokio::task::spawn_blocking(move || {
        manager.configure(&owner, &json!({"address": "0.0.0.0"}))
    })
    .await
    .unwrap()
    .unwrap();
    hold.await.unwrap();
    assert_eq!(status["error"], Value::Null, "{status}");
    assert_eq!(
        service.remote().address(),
        Some(SocketAddr::from(([0, 0, 0, 0], port)))
    );
    service.remote().stop();
}
