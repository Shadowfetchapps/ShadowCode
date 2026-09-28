//! OpenRouter against a fake API (a small Python server on loopback, pointed
//! to with `SHADOWCODE_OPENROUTER_BASE`). It serves `/models`, `/key` (bearer
//! checked) and streaming `/chat/completions` with a canned tool call, and
//! records every request. Nothing here reaches the real OpenRouter.
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{
    fs,
    io::{BufRead, BufReader},
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
KEY = "sk-or-good"

def log(value):
    with open(os.path.join(HERE, "requests.jsonl"), "a") as f:
        f.write(json.dumps(value) + "\n")

MODELS = {"data": [
    {"id": "acme/coder", "name": "Acme: Coder", "context_length": 65536,
     "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
     "pricing": {"prompt": "0.000001", "completion": "0.000002"},
     "supported_parameters": ["tools", "tool_choice"]},
    {"id": "acme/chat:free", "name": "Acme: Chat (free)", "context_length": 8192,
     "architecture": {"input_modalities": ["text", "image"], "output_modalities": ["text"]},
     "pricing": {"prompt": "0", "completion": "0"}, "supported_parameters": ["max_tokens"]},
    {"id": "acme/painter", "name": "Painter", "architecture": {"output_modalities": ["image"]}},
]}

def sse(handler, chunks):
    handler.send_response(200)
    handler.send_header("Content-Type", "text/event-stream")
    handler.end_headers()
    for chunk in chunks:
        handler.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
    handler.wfile.write(b"data: [DONE]\n\n")

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def log_message(self, *a):
        pass
    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def auth(self):
        return self.headers.get("Authorization", "") == "Bearer " + KEY
    def do_GET(self):
        log({"method": "GET", "path": self.path, "auth": self.auth()})
        if self.path == "/api/v1/models":
            return self.reply(200, MODELS)
        if self.path == "/api/v1/key":
            if not self.auth():
                return self.reply(401, {"error": {"message": "No auth credentials found"}})
            return self.reply(200, {"data": {"label": "sk-or-v1-abc...xyz", "usage": 1.25,
                                             "limit": 10, "limit_remaining": 8.75, "is_free_tier": False}})
        if self.path == "/api/v1/credits":
            if not self.auth():
                return self.reply(401, {"error": {"message": "No auth credentials found"}})
            return self.reply(200, {"data": {"total_credits": 20, "total_usage": 13.5}})
        self.reply(404, {})
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        messages = body.get("messages", [])
        log({"method": "POST", "path": self.path, "auth": self.auth(), "model": body.get("model"),
             "tools": [t["function"]["name"] for t in body.get("tools", [])],
             "title": self.headers.get("X-Title"), "referer": self.headers.get("HTTP-Referer"),
             "image": "image_url" in json.dumps(messages), "usage": body.get("usage")})
        if not self.auth():
            return self.reply(401, {"error": {"message": "No auth credentials found"}})
        if self.path != "/api/v1/chat/completions":
            return self.reply(404, {})
        if os.path.exists(os.path.join(HERE, "broke")):
            return self.reply(402, {"error": {"code": 402, "message":
                "This request requires more credits, or fewer max_tokens. You requested up to 32000 tokens, but can only afford 1133."}})
        done = any(m.get("role") == "tool" for m in messages)
        if body.get("tools") and not done:
            return sse(self, [
                {"choices": [{"delta": {"role": "assistant", "content": None, "tool_calls": [
                    {"index": 0, "id": "call_1", "type": "function", "function": {
                        "name": "write_file",
                        "arguments": json.dumps({"path": "hello.txt", "content": "hi from openrouter\n"})}}]},
                    "finish_reason": None}]},
                {"choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                 "usage": {"prompt_tokens": 40, "completion_tokens": 9, "cost": 0.0001,
                           "prompt_tokens_details": {"cached_tokens": 32}}}])
        return sse(self, [
            {"choices": [{"delta": {"role": "assistant", "content": "Wrote hello.txt."}, "finish_reason": None}]},
            {"choices": [{"delta": {}, "finish_reason": "stop"}],
             "usage": {"prompt_tokens": 55, "completion_tokens": 5}}])

server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
print(server.server_address[1], flush=True)
server.serve_forever()
"#;

struct Fake {
    child: Child,
    base: String,
    dir: tempfile::TempDir,
}
impl Drop for Fake {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Fake {
    fn start() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("fake_openrouter.py");
        fs::write(&script, FAKE).unwrap();
        let mut child = Command::new("python3")
            .arg(&script)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let base = format!("http://127.0.0.1:{}/api/v1", line.trim());
        Self { child, base, dir }
    }
    fn requests(&self) -> Vec<Value> {
        fs::read_to_string(self.dir.path().join("requests.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
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

async fn wait_job(service: &Service, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let job = call(service, "GET", &format!("/api/jobs/{id}"), Value::Null)
            .await
            .unwrap();
        if matches!(
            job["status"].as_str(),
            Some("completed" | "failed" | "cancelled")
        ) {
            return job;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "{job}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn setup(root: &Path) -> (AppPaths, Service) {
    let project = root.join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "trusted_workspaces": [project.clone()],
            "permissions": {"mode": "allow_edits"},
            // Never probe the vendor CLIs installed on the test machine.
            "cli_agents": {"enabled": false}
        }),
    )
    .unwrap();
    let service = Service::open(paths.clone(), Some(project)).unwrap();
    (paths, service)
}

#[tokio::test]
async fn key_models_picker_and_a_task_run_on_the_native_loop() {
    let fake = Fake::start();
    // Only this test binary sets the variable; it has one test.
    std::env::set_var("SHADOWCODE_OPENROUTER_BASE", &fake.base);
    std::env::remove_var("OPENROUTER_API_KEY");
    let root = tempfile::tempdir().unwrap();
    let (paths, service) = setup(root.path());
    let project = root.path().join("project");

    // No key: status says so, rows ask for a key and cannot run.
    let status = call(&service, "GET", "/api/openrouter", Value::Null)
        .await
        .unwrap();
    assert_eq!(status["key_set"], false);
    let picker = call(&service, "GET", "/api/picker", Value::Null)
        .await
        .unwrap();
    let rows: Vec<&Value> = picker["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["group"] == "api")
        .collect();
    assert!(
        rows.is_empty(),
        "no key: the picker offers to add one: {rows:?}"
    );
    assert!(
        fake.requests()
            .iter()
            .all(|r| r["path"] != "/api/v1/models"),
        "the model list is not fetched before a key exists"
    );
    let refused = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"hi","model":"api:openrouter:acme/coder"}),
    )
    .await;
    assert!(
        format!("{:#}", refused.unwrap_err()).contains("Add an OpenRouter API key"),
        "a row without a key never starts"
    );

    // A rejected key is not stored.
    let bad = call(
        &service,
        "POST",
        "/api/openrouter/key",
        json!({"api_key":"sk-or-wrong"}),
    )
    .await;
    assert!(format!("{:#}", bad.unwrap_err()).contains("rejected this key (401)"));
    assert!(
        !paths.secrets_file().exists()
            || !fs::read_to_string(paths.secrets_file())
                .unwrap()
                .contains("sk-or-wrong")
    );

    // A good key is checked, stored privately, and never echoed.
    let saved = call(
        &service,
        "POST",
        "/api/openrouter/key",
        json!({"api_key":"sk-or-good"}),
    )
    .await
    .unwrap();
    assert_eq!(saved["key_set"], true);
    assert_eq!(saved["key"]["usage"], 1.25);
    assert_eq!(saved["key"]["limit_remaining"], 8.75);
    // The account balance comes from `GET /credits`, next to the key limit.
    assert_eq!(saved["key"]["credits_remaining"], 6.5);
    assert_eq!(saved["models"], 2);
    assert_eq!(saved["tool_models"], 1);
    assert!(!saved.to_string().contains("sk-or-good"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(paths.secrets_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0, "secrets file is private");
    }
    let picker = call(&service, "GET", "/api/picker", Value::Null)
        .await
        .unwrap();
    let coder = picker["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "api:openrouter:acme/coder")
        .unwrap()
        .clone();
    assert_eq!(coder["availability"], "ready");
    assert_eq!(coder["inference"], "cloud");
    assert_eq!(
        coder["usage"]["label"],
        "API key · $1.00/M in · $2.00/M out"
    );
    let api_rows: Vec<&Value> = picker["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["group"] == "api")
        .collect();
    assert_eq!(api_rows.len(), 2, "image-only models are left out");
    assert_eq!(api_rows[1]["tools"], false);
    assert_eq!(api_rows[1]["vision"], true);

    // A task runs on ShadowCode's own loop: tools, edits, summary.
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Create hello.txt","model":"api:openrouter:acme/coder"}),
    )
    .await
    .unwrap();
    let done = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert_eq!(
        fs::read_to_string(project.join("hello.txt")).unwrap(),
        "hi from openrouter\n"
    );
    let chats: Vec<Value> = fake
        .requests()
        .into_iter()
        .filter(|r| r["path"] == "/api/v1/chat/completions")
        .collect();
    assert_eq!(chats.len(), 2, "{chats:?}");
    assert!(chats
        .iter()
        .all(|r| r["auth"] == true && r["model"] == "acme/coder"));
    assert!(chats[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "write_file"));
    assert_eq!(chats[0]["title"], "ShadowCode");
    assert_eq!(
        chats[0]["usage"],
        json!({"include": true}),
        "cost is requested"
    );

    // Per-task usage: the first turn's cost is OpenRouter's; the second turn
    // reported none, so it is priced from the model list and marked estimated.
    let usage = &done["usage"];
    assert_eq!(usage["prompt_tokens"], 95);
    assert_eq!(usage["completion_tokens"], 14);
    assert_eq!(usage["cached_tokens"], 32);
    assert_eq!(usage["turns"], 2);
    assert_eq!(usage["source"], "provider");
    let expected = 0.0001 + 55.0 * 0.000001 + 5.0 * 0.000002;
    assert!(
        (usage["cost_usd"].as_f64().unwrap() - expected).abs() < 1e-12,
        "{usage}"
    );
    assert_eq!(usage["cost_estimated"], true);
    let session = call(
        &service,
        "GET",
        &format!("/api/sessions/{}", done["session_id"].as_str().unwrap()),
        json!(null),
    )
    .await
    .unwrap();
    assert_eq!(session["usage"]["cached_tokens"], 32);
    assert!((session["usage"]["cost_usd"].as_f64().unwrap() - expected).abs() < 1e-12);

    // A chat-only model gets no tool schemas.
    let chat = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"Say hi","model":"api:openrouter:acme/chat:free","mode":"ask"}),
    )
    .await
    .unwrap();
    let done = wait_job(&service, chat["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    let last = fake
        .requests()
        .into_iter()
        .rfind(|r| r["path"] == "/api/v1/chat/completions")
        .unwrap();
    assert_eq!(last["model"], "acme/chat:free");
    assert_eq!(last["tools"], json!([]));

    // Vision: an image reaches a vision model and is refused for a text-only one.
    fs::write(
        project.join("red.png"),
        [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00,
            0x00, 0x90, 0x77, 0x53, 0xde, 0x00, 0x00, 0x00, 0x0c, 0x49, 0x44, 0x41, 0x54, 0x08,
            0xd7, 0x63, 0xf8, 0xcf, 0xc0, 0x00, 0x00, 0x03, 0x01, 0x01, 0x00, 0x18, 0xdd, 0x8d,
            0xb0, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ],
    )
    .unwrap();
    let seen = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"What colour is this?","model":"api:openrouter:acme/chat:free","images":["red.png"],"mode":"ask"}),
    )
    .await
    .unwrap();
    let done = wait_job(&service, seen["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    let last = fake
        .requests()
        .into_iter()
        .rfind(|r| r["path"] == "/api/v1/chat/completions")
        .unwrap();
    assert_eq!(last["image"], true, "the image is sent to a vision model");
    let blind = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"What colour is this?","model":"api:openrouter:acme/coder","images":["red.png"]}),
    )
    .await;
    let refused = match blind {
        Err(error) => format!("{error:#}"),
        Ok(job) => {
            let done = wait_job(&service, job["id"].as_str().unwrap()).await;
            assert_eq!(done["status"], "failed", "{done}");
            done.to_string()
        }
    };
    assert!(refused.to_lowercase().contains("image"), "{refused}");

    // An account out of credit: OpenRouter's own explanation reaches the
    // task instead of a bare status code.
    fs::write(fake.dir.path().join("broke"), "").unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"hi","model":"api:openrouter:acme/coder"}),
    )
    .await
    .unwrap();
    let broke = wait_job(&service, job["id"].as_str().unwrap()).await;
    fs::remove_file(fake.dir.path().join("broke")).unwrap();
    assert_eq!(broke["status"], "failed", "{broke}");
    let text = broke.to_string();
    assert!(text.contains("HTTP 402"), "{text}");
    assert!(text.contains("can only afford 1133"), "{text}");

    // Offline mode: rows are off and nothing is sent.
    let before = fake.requests().len();
    Config::patch(&paths, json!({"network":{"mode":"offline"}})).unwrap();
    let picker = call(&service, "GET", "/api/picker", Value::Null)
        .await
        .unwrap();
    assert!(picker["targets"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|t| t["group"] == "api")
        .all(|t| t["availability"] == "unavailable"));
    let offline = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"task":"hi","model":"api:openrouter:acme/coder"}),
    )
    .await;
    assert!(offline.is_err());
    assert_eq!(fake.requests().len(), before, "offline sends nothing");

    // Removing the key clears it.
    Config::patch(&paths, json!({"network":{"mode":"online"}})).unwrap();
    let removed = call(
        &service,
        "POST",
        "/api/openrouter/key",
        json!({"api_key":""}),
    )
    .await
    .unwrap();
    assert_eq!(removed["key_set"], false);
    assert!(!fs::read_to_string(paths.secrets_file())
        .unwrap_or_default()
        .contains("sk-or-good"));
}
