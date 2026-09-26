//! Model-quality engine work against fake model servers on loopback:
//! retries (429 with Retry-After, overload, mid-stream drops), model-written
//! compaction summaries that later turns replay, per-turn usage and cost,
//! prompt-cache markers and cached-token accounting, and tool description
//! tiers. Nothing here reaches a real provider.
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    autonomy,
    config::{Config, ModelConfig},
    context,
    engine::{Engine, Job, StartRequest},
    models::{ModelClient, StreamDecoder},
    paths::AppPaths,
    tools::DescriptionTier,
};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

/// What the raw fake server does with one request.
enum Reply {
    /// 200 with a complete JSON completion.
    Json(Value),
    /// An error status with extra headers.
    Status(u16, Vec<(&'static str, &'static str)>),
    /// 200 SSE frames, then the connection closes without a finish marker.
    SseDrop(Vec<Value>),
}

struct Raw {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    worker: tokio::task::JoinHandle<()>,
}
impl Drop for Raw {
    fn drop(&mut self) {
        self.worker.abort();
    }
}

async fn read_body(socket: &mut tokio::net::TcpStream) -> Option<Value> {
    let mut wire = Vec::new();
    let mut buffer = [0; 8192];
    loop {
        let count = socket.read(&mut buffer).await.ok()?;
        if count == 0 {
            return None;
        }
        wire.extend_from_slice(&buffer[..count]);
        if let Some(end) = wire.windows(4).position(|w| w == b"\r\n\r\n") {
            let headers = String::from_utf8_lossy(&wire[..end]).to_lowercase();
            let len = headers
                .lines()
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .and_then(|v| v.trim().parse::<usize>().ok())
                })
                .unwrap_or(0);
            if wire.len() >= end + 4 + len {
                return serde_json::from_slice(&wire[end + 4..end + 4 + len]).ok();
            }
        }
    }
}

async fn raw_server(handler: impl Fn(usize, &Value) -> Reply + Send + 'static) -> Raw {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let captured = requests.clone();
    let worker = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let Some(body) = read_body(&mut socket).await else {
                continue;
            };
            let index = {
                let mut seen = captured.lock().unwrap();
                seen.push(body.clone());
                seen.len() - 1
            };
            let wire = match handler(index, &body) {
                Reply::Json(value) => {
                    let text = value.to_string();
                    format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}", text.len())
                }
                Reply::Status(code, headers) => {
                    let text = json!({"error":{"message":"fake failure"}}).to_string();
                    let extra: String = headers
                        .iter()
                        .map(|(k, v)| format!("{k}: {v}\r\n"))
                        .collect();
                    format!("HTTP/1.1 {code} Fake\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}Connection: close\r\n\r\n{text}", text.len())
                }
                Reply::SseDrop(frames) => {
                    let mut wire = "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n".to_owned();
                    for frame in frames {
                        wire.push_str(&format!("data: {frame}\n\n"));
                    }
                    wire
                }
            };
            let _ = socket.write_all(wire.as_bytes()).await;
            let _ = socket.shutdown().await;
        }
    });
    Raw {
        endpoint,
        requests,
        worker,
    }
}

fn response(text: &str, calls: Value) -> Value {
    let reason = if calls.as_array().is_some_and(|v| !v.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
}

fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}

fn setup(endpoint: &str, context_limit: usize, agent: Value) -> (tempfile::TempDir, Engine) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("project")).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":endpoint,"name":"fixture","context_limit":context_limit},
            "trusted_workspaces":[root.path().join("project")],
            "permissions":{"approve_shell":false,"mode":"allow_edits"},
            "cli_agents":{"enabled":false},
            "agent":agent
        }),
    )
    .unwrap();
    let engine = Engine::open(paths).unwrap();
    (root, engine)
}

fn request(root: &Path, task: &str, session_id: Option<String>) -> StartRequest {
    StartRequest {
        workspace: root.join("project"),
        task: task.into(),
        session_id,
        model: None,
        mode: "code".into(),
        queue: false,
        images: Vec::new(),
        web: false,
    }
}

async fn wait(engine: &Engine, id: &str) -> Job {
    tokio::time::timeout(Duration::from_secs(20), engine.wait(id))
        .await
        .unwrap()
        .unwrap()
}

fn events(engine: &Engine, job: &Job, kind: &str) -> Vec<Value> {
    engine
        .store()
        .recent_events(&job.session_id, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == kind && e["task_id"] == job.task_id.as_str())
        .map(|e| e["payload"].clone())
        .collect()
}

#[tokio::test]
async fn rate_limit_is_retried_after_the_providers_retry_after() {
    let server = raw_server(|index, _| {
        if index == 0 {
            Reply::Status(429, vec![("Retry-After", "1")])
        } else {
            Reply::Json(response("Done after waiting.", json!([])))
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint, 16384, json!({"retry_backoff_sec":0.01}));
    let started = Instant::now();
    let job = engine
        .start(request(root.path(), "hello", None))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert!(
        started.elapsed() >= Duration::from_millis(950),
        "Retry-After honoured"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let retries = events(&engine, &done, "model.retry");
    assert_eq!(retries.len(), 1, "{retries:?}");
    assert_eq!(retries[0]["reason"], "rate_limited");
    assert_eq!(retries[0]["status"], 429);
    assert_eq!(retries[0]["retry_after"], true);
    assert_eq!(retries[0]["delay_ms"], 1000);
    assert_eq!(retries[0]["attempt"], 1);
    assert_eq!(retries[0]["max_attempts"], 3);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_mid_stream_drop_is_retried_and_the_partial_reply_discarded() {
    let server = raw_server(|index, _| {
        if index == 0 {
            Reply::SseDrop(vec![
                json!({"choices":[{"delta":{"role":"assistant","content":"Partial ans"},"finish_reason":null}]}),
                json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"write_file","arguments":"{\"path\":\"half"}}]},"finish_reason":null}]}),
            ])
        } else {
            Reply::Json(response("Complete answer.", json!([])))
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint, 16384, json!({"retry_backoff_sec":0.01}));
    let job = engine
        .start(request(root.path(), "hello", None))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert_eq!(done.summary, "Complete answer.");
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let retries = events(&engine, &done, "model.retry");
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0]["reason"], "disconnected");
    let discarded = retries[0]["discard_message_id"].as_str().unwrap();
    let partial = events(&engine, &done, "model.stream");
    assert!(partial
        .iter()
        .any(|e| e["message_id"] == discarded && e["text"] == "Partial ans"));
    assert!(events(&engine, &done, "model.stream_end")
        .iter()
        .any(|e| e["message_id"] == discarded && e["complete"] == false));
    // The partial tool call never ran and the tape has only the final reply.
    assert!(!root.path().join("project/half").exists());
    assert!(events(&engine, &done, "tool.started").is_empty());
    let tape = engine.store().messages(&done.id).unwrap();
    assert!(!json!(tape).to_string().contains("Partial ans"));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_retry_never_repeats_a_tool_that_already_ran() {
    let server = raw_server(|index, _| match index {
        0 => Reply::Json(response(
            "",
            json!([tool(
                "w1",
                "write_file",
                json!({"path":"once.txt","content":"one\n"})
            )]),
        )),
        1 => Reply::Status(503, vec![]),
        _ => Reply::Json(response("Wrote once.txt.", json!([]))),
    })
    .await;
    let (root, engine) = setup(&server.endpoint, 16384, json!({"retry_backoff_sec":0.01}));
    let job = engine
        .start(request(root.path(), "Create once.txt", None))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1]["messages"], requests[2]["messages"],
        "the same request is re-sent"
    );
    let writes: Vec<_> = events(&engine, &done, "tool.completed")
        .into_iter()
        .filter(|e| e["tool"] == "write_file")
        .collect();
    assert_eq!(writes.len(), 1, "the tool ran once");
    let retries = events(&engine, &done, "model.retry");
    assert_eq!(retries.len(), 1);
    assert_eq!(retries[0]["reason"], "overloaded");
    assert_eq!(retries[0]["discard_message_id"], Value::Null);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn permanent_errors_fail_at_once_and_retries_stop_at_the_limit() {
    let server = raw_server(|_, _| Reply::Status(400, vec![])).await;
    let (root, engine) = setup(&server.endpoint, 16384, json!({"retry_backoff_sec":0.01}));
    let job = engine
        .start(request(root.path(), "hello", None))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "failed");
    assert!(done.summary.contains("HTTP 400"), "{}", done.summary);
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    assert!(events(&engine, &done, "model.retry").is_empty());
    engine.shutdown().await.unwrap();

    let server = raw_server(|_, _| Reply::Status(429, vec![])).await;
    let (root, engine) = setup(
        &server.endpoint,
        16384,
        json!({"retry_backoff_sec":0.01,"model_retries":2}),
    );
    let job = engine
        .start(request(root.path(), "hello", None))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "failed");
    assert!(done.summary.contains("rate limit"), "{}", done.summary);
    assert_eq!(
        server.requests.lock().unwrap().len(),
        3,
        "1 try + 2 retries"
    );
    assert_eq!(events(&engine, &done, "model.retry").len(), 2);
    engine.shutdown().await.unwrap();
}

fn is_summary_request(body: &Value) -> bool {
    body.get("tools").is_none()
        && body["messages"][0]["content"]
            .as_str()
            .is_some_and(|s| s.contains("compaction summaries"))
}

/// A long earlier conversation for the session, so the first turn must compact.
fn seed(engine: &Engine, root: &Path) -> String {
    let session = engine
        .store()
        .create_session(&root.join("project"), "fixture", "Long history")
        .unwrap();
    let sid = session["id"].as_str().unwrap().to_owned();
    let mut messages = Vec::new();
    for i in 0..14 {
        messages.push(
            json!({"role":"user","content":format!("Old request {i}: {}", "context ".repeat(400))}),
        );
        messages.push(json!({"role":"assistant","content":format!("Old answer {i}: {}", "detail ".repeat(400))}));
    }
    engine
        .store()
        .set_session_meta(&sid, "message_seed", &json!(messages).to_string())
        .unwrap();
    sid
}

#[tokio::test]
async fn compaction_uses_a_model_summary_that_later_turns_replay() {
    let server = support::server(|_, body| {
        if is_summary_request(body) {
            assert!(body["max_tokens"].as_u64().unwrap() <= 1024);
            let prompt = body["messages"][1]["content"].as_str().unwrap();
            assert!(prompt.contains("Old request 0"));
            return (
                json!({"choices":[{"message":{"role":"assistant","content":"SUMMARY: user asked for 14 old things; all answered."},"finish_reason":"stop"}],"usage":{"prompt_tokens":900,"completion_tokens":40}}),
                Duration::ZERO,
            );
        }
        (response("Answered.", json!([])), Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint, 16384, json!({}));
    let sid = seed(&engine, root.path());
    let job = engine
        .start(request(root.path(), "What next?", Some(sid.clone())))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let compacted = events(&engine, &done, "context.compacted");
    assert_eq!(compacted.len(), 1, "{compacted:?}");
    let event = &compacted[0];
    assert_eq!(event["method"], "model_summary", "{event}");
    assert!(event["summary"].as_str().unwrap().starts_with("SUMMARY:"));
    assert!(
        event["before_estimated_tokens"].as_u64().unwrap()
            > event["after_estimated_tokens"].as_u64().unwrap()
    );
    assert_eq!(event["summary_model"], "fixture");
    {
        let requests = server.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert!(is_summary_request(&requests[0]));
        let note = &requests[1]["messages"][1];
        assert_eq!(note["role"], "system");
        assert!(note["content"]
            .as_str()
            .unwrap()
            .contains("SUMMARY: user asked"));
        assert!(
            note.get("_shadow_summary").is_none(),
            "internal keys stay local"
        );
        context::validate_pairs(requests[1]["messages"].as_array().unwrap()).unwrap();
    }
    // The summary request's tokens count toward the job.
    let usage = events(&engine, &done, "usage.updated");
    assert_eq!(usage[0]["purpose"], "compaction");
    assert_eq!(usage[0]["turn"]["prompt_tokens"], 900);
    assert_eq!(done.usage.prompt_tokens, 920);
    assert_eq!(done.usage.turns, 2);
    assert_eq!(done.usage.cost_usd, Some(0.0), "local models cost nothing");
    assert_eq!(done.usage.source, "local");

    // The next turn in the session starts from the saved summary.
    let next = engine
        .start(request(root.path(), "And after that?", Some(sid.clone())))
        .await
        .unwrap();
    let next = wait(&engine, &next.id).await;
    assert_eq!(next.status, "completed", "{}", next.summary);
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3, "no second summary was needed");
    assert!(requests[2]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("SUMMARY: user asked"));
    // Session usage adds both jobs up.
    let session = engine.store().session(&sid).unwrap().unwrap();
    assert_eq!(session["usage"]["turns"], 3);
    assert_eq!(session["usage"]["prompt_tokens"], 940);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_failed_summary_falls_back_to_the_digest_note() {
    let server = support::server(|_, body| {
        if is_summary_request(body) {
            return (json!({"choices":[]}), Duration::ZERO);
        }
        (response("Answered.", json!([])), Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint, 16384, json!({}));
    let sid = seed(&engine, root.path());
    let job = engine
        .start(request(root.path(), "What next?", Some(sid)))
        .await
        .unwrap();
    let done = wait(&engine, &job.id).await;
    assert_eq!(done.status, "completed", "{}", done.summary);
    let event = &events(&engine, &done, "context.compacted")[0];
    assert_eq!(event["method"], "bounded_history");
    assert!(event["fallback_reason"]
        .as_str()
        .unwrap()
        .contains("no completion choices"));
    let requests = server.requests.lock().unwrap().clone();
    let note = requests[1]["messages"][1]["content"].as_str().unwrap();
    assert!(note.contains("earlier messages were omitted"), "{note}");
    engine.shutdown().await.unwrap();
}

fn model(endpoint: &str, context_limit: usize) -> ModelConfig {
    ModelConfig {
        default: "fixture".into(),
        name: "fixture".into(),
        provider: "local".into(),
        endpoint: endpoint.into(),
        api_key_env: "OPENAI_API_KEY".into(),
        keep_alive: "5m".into(),
        context_limit,
    }
}

#[tokio::test]
async fn tiny_or_disabled_contexts_keep_the_digest_without_a_request() {
    let server = support::server(|_, _| (response("unused", json!([])), Duration::ZERO)).await;
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    paths.ensure().unwrap();
    for (limit, enabled, reason) in [
        (4096, true, "context_too_small"),
        (16384, false, "disabled"),
    ] {
        let mut config = Config {
            model: model(&server.endpoint, limit),
            ..Default::default()
        };
        config.agent.summary_compaction = enabled;
        let client = ModelClient::new(config.model.clone(), &paths).unwrap();
        let mut messages = vec![json!({"role":"system","content":"system"})];
        for i in 0..80 {
            messages.push(
                json!({"role":"user","content":format!("request {i}: {}", "context ".repeat(100))}),
            );
            messages.push(json!({"role":"assistant","content":format!("answer {i}: {}", "detail ".repeat(100))}));
        }
        // Exercise real token pressure, not an arbitrary message-count trigger.
        assert!(shadowcode_core::context::estimate_tokens(&json!(messages)) > limit);
        let outcome = shadowcode_core::compaction::compact(
            &client,
            &mut messages,
            &[],
            &config,
            &CancellationToken::new(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(outcome.event["method"], "bounded_history");
        assert_eq!(outcome.event["fallback_reason"], reason);
        assert!(outcome.usage.is_none());
    }
    assert!(server.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn tool_descriptions_are_full_for_large_contexts_and_short_for_small_ones() {
    for (limit, full) in [(8192, false), (65536, true)] {
        let server = support::server(|_, _| (response("Hi.", json!([])), Duration::ZERO)).await;
        let (root, engine) = setup(&server.endpoint, limit, json!({}));
        let job = engine
            .start(request(root.path(), "hi", None))
            .await
            .unwrap();
        let done = wait(&engine, &job.id).await;
        assert_eq!(done.status, "completed", "{}", done.summary);
        let requests = server.requests.lock().unwrap().clone();
        let tools = requests[0]["tools"].as_array().unwrap();
        let longest = tools
            .iter()
            .filter(|t| {
                t["function"]["name"] != "web_fetch" && t["function"]["name"] != "web_search"
            })
            .map(|t| t["function"]["description"].as_str().unwrap().len())
            .max()
            .unwrap();
        assert_eq!(longest > 64, full, "limit {limit}: longest {longest}");
        let read = tools
            .iter()
            .find(|t| t["function"]["name"] == "read_file")
            .unwrap();
        assert_eq!(
            read["function"]["description"]
                .as_str()
                .unwrap()
                .contains("do not assume omitted lines"),
            full
        );
        engine.shutdown().await.unwrap();
    }
    let profile = |provider: &str, limit| {
        autonomy::description_tier(&autonomy::capability_profile_for(provider, "m", limit))
    };
    assert_eq!(profile("llamacpp", 4096), DescriptionTier::Short);
    assert_eq!(profile("ollama", 16384), DescriptionTier::Short);
    assert_eq!(profile("ollama", 32768), DescriptionTier::Full);
    assert_eq!(profile("openrouter", 16384), DescriptionTier::Full);
    assert_eq!(profile("openrouter", 8192), DescriptionTier::Short);
}

#[test]
fn stream_usage_reads_cached_tokens_and_provider_cost() {
    let mut decoder = StreamDecoder::new(false);
    decoder
        .push(b"data: {\"choices\":[{\"delta\":{\"content\":\"hi\"},\"finish_reason\":\"stop\"}]}\n\n")
        .unwrap();
    decoder
        .push(b"data: {\"choices\":[],\"usage\":{\"prompt_tokens\":1200,\"completion_tokens\":8,\"cost\":0.0042,\"prompt_tokens_details\":{\"cached_tokens\":1024,\"cache_write_tokens\":0}}}\n\ndata: [DONE]\n\n")
        .unwrap();
    let response = decoder.finish().unwrap();
    assert_eq!(response.usage.prompt_tokens, 1200);
    assert_eq!(response.usage.cached_tokens, 1024);
    assert_eq!(response.usage.cost_usd, Some(0.0042));
    let mut deepseek = StreamDecoder::new(false);
    deepseek
        .push(b"data: {\"choices\":[{\"delta\":{\"content\":\"x\"},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":50,\"completion_tokens\":1,\"prompt_cache_hit_tokens\":32}}\n\n")
        .unwrap();
    let response = deepseek.finish().unwrap();
    assert_eq!(response.usage.cached_tokens, 32);
    assert_eq!(response.usage.cost_usd, None);
    // A stream that stops without a finish marker is a retryable disconnect.
    let mut cut = StreamDecoder::new(false);
    cut.push(b"data: {\"choices\":[{\"delta\":{\"content\":\"par\"}}]}\n\n")
        .unwrap();
    let error = cut.finish().unwrap_err();
    assert_eq!(
        shadowcode_core::retry::classify(&error).unwrap().kind,
        "disconnected"
    );
    // An in-stream provider error is classified by its code.
    let mut overloaded = StreamDecoder::new(false);
    let error = overloaded
        .push(b"data: {\"error\":{\"code\":529,\"message\":\"Overloaded\"}}\n\n")
        .unwrap_err();
    assert_eq!(
        shadowcode_core::retry::classify(&error).unwrap().kind,
        "overloaded"
    );
}

#[test]
fn openrouter_claude_requests_mark_cache_breakpoints_and_ask_for_cost() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    paths.ensure().unwrap();
    let config = ModelConfig {
        default: "api:openrouter:anthropic/claude-sonnet-4.5".into(),
        name: "anthropic/claude-sonnet-4.5".into(),
        provider: "openrouter".into(),
        endpoint: "https://openrouter.ai/api/v1".into(),
        api_key_env: "OPENROUTER_API_KEY".into(),
        keep_alive: "5m".into(),
        context_limit: 200_000,
    };
    let client = ModelClient::new(config, &paths).unwrap();
    let messages = vec![
        json!({"role":"system","content":"You are ShadowCode."}),
        json!({"role":"user","content":"Fix it","_shadow_images":[]}),
    ];
    let tools = shadowcode_core::tools::schemas();
    let body = client.request_body(&messages, &tools, 1000);
    assert_eq!(body["usage"], json!({"include":true}));
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({"type":"ephemeral"})
    );
    assert_eq!(body["messages"][1]["content"][0]["text"], "Fix it");
    assert!(body["messages"][1].get("_shadow_images").is_none());
    assert_eq!(body["tools"].as_array().unwrap().len(), tools.len());
    // Other OpenAI-style providers get neither.
    let plain = ModelClient::new(model("http://127.0.0.1:9/v1", 16384), &paths).unwrap();
    let body = plain.request_body(&messages, &[], 1000);
    assert!(body.get("usage").is_none());
    assert_eq!(body["messages"][0]["content"], "You are ShadowCode.");
}
