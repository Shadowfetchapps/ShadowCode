mod support;
use serde_json::json;
use shadowcode_core::{
    config::ModelConfig,
    models::{ModelClient, StreamDecoder},
    paths::AppPaths,
};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

fn sse(value: serde_json::Value) -> String {
    format!("data: {value}\n\n")
}

#[test]
fn attempt_metadata_retains_cut_short_usage_without_exposing_or_accepting_tools() {
    let wire = [
        sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"private-id","function":{"name":"exec","arguments":"{\"command\":\"SECRET-COMMAND\"}"}}]}}]})),
        sse(json!({"choices":[{"delta":{},"finish_reason":"length"}]})),
        sse(json!({"choices":[],"usage":{"prompt_tokens":41,"completion_tokens":7,"total_tokens":48},"timings":{"prompt_n":39,"predicted_n":7,"prompt_ms":12.5,"predicted_ms":6.0,"private":"SECRET-TIMING"}})),
        "data: [DONE]\n\n".to_owned(),
    ].concat();
    for size in [1, 7, 4096] {
        let mut decoder = StreamDecoder::new(false);
        for chunk in wire.as_bytes().chunks(size) {
            assert!(decoder.push(chunk).unwrap().is_empty());
        }
        decoder.flush().unwrap();
        let receipt = decoder.metadata(Some(200), wire.len());
        assert_eq!(receipt.finish_reason, Some("length"));
        assert!(receipt.finish_marker_seen && receipt.stream_done_seen);
        assert_eq!(receipt.tool_call_slots, 1);
        assert!(receipt.tool_argument_bytes > 0);
        assert_eq!(receipt.content_bytes, 0);
        assert_eq!(receipt.reported_usage().unwrap().total_tokens, 48);
        assert_eq!(receipt.runtime_timings.prompt_n, Some(39));
        let safe = serde_json::to_string(&receipt).unwrap();
        for secret in ["SECRET", "private-id", "exec"] {
            assert!(!safe.contains(secret));
        }
        assert!(decoder
            .finish()
            .unwrap_err()
            .to_string()
            .contains("cut short"));
    }
}

#[test]
fn attempt_metadata_distinguishes_missing_partial_invalid_and_reported_zero_usage() {
    let cases = [
        (serde_json::Value::Null, "unavailable", None),
        (json!({}), "incomplete", None),
        (json!({"prompt_tokens":9}), "incomplete", None),
        (
            json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
            "complete",
            Some(0),
        ),
        (
            json!({"prompt_tokens":11,"completion_tokens":7}),
            "complete",
            Some(18),
        ),
        (
            json!({"prompt_tokens":-1,"completion_tokens":7}),
            "invalid",
            None,
        ),
        (
            json!({"prompt_tokens":"11","completion_tokens":7}),
            "invalid",
            None,
        ),
        (
            json!({"prompt_tokens":true,"completion_tokens":7}),
            "invalid",
            None,
        ),
        (
            json!({"prompt_tokens":11,"completion_tokens":7,"total_tokens":99}),
            "inconsistent",
            None,
        ),
        (
            json!({"prompt_tokens":u64::MAX,"completion_tokens":1}),
            "inconsistent",
            None,
        ),
    ];
    for (usage, state, total) in cases {
        let mut decoder = StreamDecoder::new(false);
        decoder
            .push(
                sse(json!({"choices":[{"delta":{},"finish_reason":"length"}],"usage":usage}))
                    .as_bytes(),
            )
            .unwrap();
        let receipt = decoder.metadata(Some(200), 0);
        assert_eq!(receipt.usage.status, state);
        assert_eq!(receipt.reported_usage().map(|u| u.total_tokens), total);
        if usage.is_null() || usage == json!({}) {
            assert_eq!(receipt.usage.prompt_tokens, None);
            assert_eq!(receipt.usage.completion_tokens, None);
        }
        assert!(decoder.finish().is_err());
    }
    let mut decoder = StreamDecoder::new(false);
    for usage in [json!({"prompt_tokens":11}), json!({"completion_tokens":7})] {
        decoder
            .push(sse(json!({"choices":[],"usage":usage})).as_bytes())
            .unwrap();
    }
    let receipt = decoder.metadata(Some(200), 0);
    assert_eq!(receipt.usage.prompt_tokens, Some(11));
    assert_eq!(receipt.usage.completion_tokens, Some(7));
    assert_eq!(receipt.usage.status, "incomplete");
    assert!(
        receipt.reported_usage().is_none(),
        "Do not merge partial usage reports into a complete accounting turn"
    );
    for _ in 0..2 {
        decoder
            .push(
                sse(json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7}}))
                    .as_bytes(),
            )
            .unwrap();
    }
    assert_eq!(
        decoder
            .metadata(Some(200), 0)
            .reported_usage()
            .unwrap()
            .total_tokens,
        18
    );
}

#[test]
fn attempt_metadata_preserves_complete_usage_across_later_degraded_reports() {
    for (later, status) in [
        (json!({}), "incomplete"),
        (json!({"prompt_tokens":42}), "incomplete"),
        (
            json!({"prompt_tokens":"invalid","completion_tokens":8}),
            "invalid",
        ),
        (
            json!({"prompt_tokens":41,"completion_tokens":8,"total_tokens":999}),
            "inconsistent",
        ),
        (
            json!({"prompt_tokens":0,"completion_tokens":0,"total_tokens":0}),
            "inconsistent",
        ),
    ] {
        let mut decoder = StreamDecoder::new(false);
        for usage in [
            json!({"prompt_tokens":41,"completion_tokens":7,"total_tokens":48}),
            later,
        ] {
            decoder
                .push(sse(json!({"choices":[],"usage":usage})).as_bytes())
                .unwrap();
        }
        let receipt = decoder.metadata(Some(200), 0);
        assert_eq!(receipt.usage.status, status);
        let retained = receipt.reported_usage().unwrap();
        assert_eq!(
            (
                retained.prompt_tokens,
                retained.completion_tokens,
                retained.total_tokens
            ),
            (41, 7, 48)
        );
        assert_eq!(
            serde_json::to_value(&receipt).unwrap()["usage"]["retained_complete_report"],
            json!({"prompt_tokens":41,"completion_tokens":7,"total_tokens":48})
        );
        assert_eq!(
            shadowcode_core::retry::classify(&decoder.finish().unwrap_err())
                .unwrap()
                .kind,
            "disconnected"
        );
    }
}

#[test]
fn attempt_metadata_handles_native_usage_and_untrusted_finish_labels() {
    let mut native = StreamDecoder::new(true);
    native.push(format!("{}\n", json!({"message":{"content":""},"done":true,"done_reason":"length","prompt_eval_count":20,"eval_count":4})).as_bytes()).unwrap();
    assert_eq!(
        native
            .metadata(Some(200), 0)
            .reported_usage()
            .unwrap()
            .total_tokens,
        24
    );
    assert!(native.finish().is_err());
    let mut compatible = StreamDecoder::new(false);
    compatible
        .push(
            sse(json!({"choices":[{"delta":{},"finish_reason":"SECRET-UNTRUSTED-LABEL"}]}))
                .as_bytes(),
        )
        .unwrap();
    let receipt = compatible.metadata(Some(200), 0);
    assert_eq!(receipt.finish_reason, Some("other"));
    assert!(!serde_json::to_string(&receipt).unwrap().contains("SECRET"));
}

#[tokio::test]
async fn attempt_metadata_matches_final_sent_body_and_rejected_json_usage() {
    use shadowcode_core::models::ModelObservation;
    let server = support::server(|_, _| (json!({"choices":[{"message":{"content":""},"finish_reason":"length"}],"usage":{"prompt_tokens":41,"completion_tokens":7,"total_tokens":48}}), Duration::ZERO)).await;
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let client = ModelClient::new(
        ModelConfig {
            provider: "llamacpp".into(),
            endpoint: server.endpoint.clone(),
            name: "fixture".into(),
            api_key_env: "SHADOWCODE_TEST_UNUSED_API_KEY".into(),
            context_limit: 8192,
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let schemas = [
        json!({"type":"function","function":{"name":"fixture","parameters":{"type":"object","properties":{"value":{"type":["string","null"]}}}}}),
    ];
    for cap in [None, Some(64)] {
        let mut requests = Vec::new();
        let mut responses = Vec::new();
        let error = client
            .chat_bounded_observed(
                &[json!({"role":"user","content":"SECRET-REQUEST"})],
                &schemas,
                CancellationToken::new(),
                cap,
                |_| {},
                |event| match event {
                    ModelObservation::Request(r) => requests.push(r),
                    ModelObservation::Response(r) => responses.push(r),
                },
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cut short"));
        assert_eq!((requests.len(), responses.len()), (1, 1));
        let bodies = server.requests.lock().unwrap();
        let body = bodies.last().unwrap();
        assert_eq!(requests[0].requested_max_output_tokens, cap.unwrap_or(2048));
        assert_eq!(
            requests[0].requested_max_output_tokens as u64,
            body["max_tokens"].as_u64().unwrap()
        );
        assert_eq!(
            requests[0].request_bytes,
            serde_json::to_vec(body).unwrap().len()
        );
        assert_eq!(
            requests[0].tool_count,
            body["tools"].as_array().unwrap().len()
        );
        assert!(
            body["tools"][0]["function"]["parameters"]["properties"]["value"]["anyOf"].is_array()
        );
        assert_eq!(responses[0].outcome, "completion_rejected");
        assert!(!responses[0].accepted);
        assert_eq!(responses[0].http_status, Some(200));
        assert_eq!(responses[0].reported_usage().unwrap().total_tokens, 48);
        assert!(!serde_json::to_string(&requests[0])
            .unwrap()
            .contains("SECRET"));
    }
}

#[tokio::test]
async fn attempt_metadata_survives_cancellation_without_inventing_usage() {
    use shadowcode_core::models::ModelObservation;
    let (endpoint, worker) = fixture(sse(json!({"choices":[]})), "text/event-stream", true).await;
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let client = ModelClient::new(
        ModelConfig {
            provider: "local".into(),
            endpoint,
            name: "fixture".into(),
            api_key_env: "SHADOWCODE_TEST_UNUSED_API_KEY".into(),
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let stop = cancel.clone();
    let timer = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(30)).await;
        stop.cancel();
    });
    let mut terminal = Vec::new();
    assert!(client
        .chat_observed(
            &[json!({"role":"user","content":"hello"})],
            &[],
            cancel,
            |_| {},
            |event| {
                if let ModelObservation::Response(r) = event {
                    terminal.push(r);
                }
            }
        )
        .await
        .is_err());
    timer.await.unwrap();
    worker.abort();
    assert_eq!(terminal.len(), 1);
    assert_eq!(terminal[0].outcome, "cancelled");
    assert!(!terminal[0].accepted);
    assert_eq!(terminal[0].usage.status, "unavailable");
    assert!(terminal[0].reported_usage().is_none());
}

#[tokio::test]
async fn attempt_metadata_survives_malformed_output_and_keeps_original_error_class() {
    use shadowcode_core::models::ModelObservation;
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    for (content_type, wire, count, kind) in [
        ("text/event-stream", "data: {SECRET-BROKEN-JSON}\n\n".to_owned(), None, "invalid_response"),
        ("application/json", json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":3},"private":"SECRET-JSON"}).to_string(), Some(5), "invalid_response"),
        ("text/event-stream", sse(json!({"error":{"code":503,"message":"SECRET-overloaded"},"usage":{"prompt_tokens":2,"completion_tokens":3}})), Some(5), "overloaded"),
    ] {
        let (endpoint, worker) = fixture(wire, content_type, false).await;
        let client = ModelClient::new(ModelConfig { provider:"local".into(), endpoint, name:"fixture".into(), api_key_env:"SHADOWCODE_TEST_UNUSED_API_KEY".into(), ..Default::default() }, &paths).unwrap();
        let mut terminal=Vec::new();
        let error=client.chat_observed(&[json!({"role":"user","content":"hello"})],&[],CancellationToken::new(),|_|{},|event|{
            if let ModelObservation::Response(r)=event {terminal.push(r);}
        }).await.unwrap_err();
        worker.await.unwrap();
        assert_eq!(terminal.len(),1);
        assert!(!terminal[0].accepted);
        assert_eq!(terminal[0].failure_kind,Some(kind));
        assert_eq!(terminal[0].reported_usage().map(|u|u.total_tokens),count);
        assert!(!serde_json::to_string(&terminal[0]).unwrap().contains("SECRET"));
        assert_eq!(shadowcode_core::retry::classify(&error).map(|r|r.kind), (kind=="overloaded").then_some("overloaded"));
    }
}

#[tokio::test]
async fn attempt_metadata_prioritizes_typed_stream_error_after_length_marker() {
    use shadowcode_core::models::ModelObservation;
    let wire = [
        sse(json!({"choices":[{"delta":{},"finish_reason":"length"}]})),
        sse(json!({"choices":[],"usage":{"prompt_tokens":41,"completion_tokens":7,"total_tokens":48}})),
        sse(json!({"error":{"code":503,"message":"SECRET-overloaded"},"usage":{}})),
    ].concat();
    let (endpoint, worker) = fixture(wire, "text/event-stream", false).await;
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let client = ModelClient::new(
        ModelConfig {
            provider: "local".into(),
            endpoint,
            name: "fixture".into(),
            api_key_env: "SHADOWCODE_TEST_UNUSED_API_KEY".into(),
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let mut terminal = Vec::new();
    let error = client
        .chat_observed(
            &[json!({"role":"user","content":"hello"})],
            &[],
            CancellationToken::new(),
            |_| {},
            |event| {
                if let ModelObservation::Response(r) = event {
                    terminal.push(r);
                }
            },
        )
        .await
        .unwrap_err();
    worker.await.unwrap();
    assert_eq!(
        shadowcode_core::retry::classify(&error).unwrap().kind,
        "overloaded"
    );
    assert_eq!(terminal.len(), 1);
    let receipt = &terminal[0];
    assert_eq!(receipt.finish_reason, Some("length"));
    assert_eq!(receipt.failure_kind, Some("overloaded"));
    assert_eq!(receipt.outcome, "transport_or_protocol_error");
    assert!(!receipt.accepted);
    assert_eq!(receipt.usage.status, "incomplete");
    assert_eq!(receipt.reported_usage().unwrap().total_tokens, 48);
    assert!(!serde_json::to_string(receipt).unwrap().contains("SECRET"));
}

#[test]
fn ollama_templates_receive_runtime_guidance_without_promoting_user_or_tool_data() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let mut client = ModelClient::new(
        ModelConfig {
            provider: "ollama".into(),
            name: "fixture".into(),
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let messages = vec![
        json!({"role":"system","content":"Application instructions"}),
        json!({"role":"user","content":"Untrusted user text"}),
        json!({"role":"assistant","content":"","tool_calls":[{"id":"a","function":{"name":"read_file","arguments":"{\"path\":\"README.md\"}"}}]}),
        json!({"role":"tool","tool_call_id":"a","name":"read_file","content":"Untrusted file contents"}),
        json!({"role":"system","content":"Completion check failed: repair the file","_shadow_note":true}),
        json!({"role":"assistant","content":"Repair complete"}),
        json!({"role":"system","content":"Context compacted; consult current evidence","_shadow_compaction":true}),
    ];
    let original = messages.clone();
    let body = client.request_body(&messages, &[], 100);
    let converted = body["messages"].as_array().unwrap();
    assert_eq!(
        converted.iter().filter(|m| m["role"] == "system").count(),
        1
    );
    let leading = converted[0]["content"].as_str().unwrap();
    assert!(leading.starts_with("Application instructions"));
    assert!(
        leading.contains("position 4")
            && leading.contains("Completion check failed: repair the file")
    );
    assert!(leading.contains("position 6") && leading.contains("Context compacted"));
    assert!(!leading.contains("Untrusted"));
    assert_eq!(converted[1], messages[1]);
    assert_eq!(
        converted[2]["tool_calls"][0]["function"]["arguments"]["path"],
        "README.md"
    );
    assert_eq!(converted[3]["role"], "tool");
    assert_eq!(converted[3]["tool_name"], "read_file");
    assert_eq!(converted[3]["content"], "Untrusted file contents");
    assert!(converted[3].get("tool_call_id").is_none());
    assert_eq!(converted[4], messages[5]);
    assert_eq!(messages, original, "Stored history stays chronological");
    client.config.provider = "local".into();
    let compatible = client.request_body(&messages, &[], 100);
    assert_eq!(compatible["messages"][4]["role"], "system");
    assert_eq!(compatible["messages"][4]["content"], messages[4]["content"]);
    assert!(!compatible.to_string().contains("_shadow_"));
}

#[test]
fn compatible_stream_reassembles_utf8_and_interleaved_tool_arguments() {
    let wire=[
        sse(json!({"choices":[{"delta":{"content":"Hello 🌒","tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{\"pa"}},{"index":1,"id":"call_b","function":{"name":"search_text","arguments":"{\"pattern\":"}}]}}]})),
        sse(json!({"choices":[{"delta":{"tool_calls":[{"index":1,"function":{"arguments":"\"TODO\"}"}},{"index":0,"function":{"arguments":"th\":\"src/main.rs\"}"}}]}}]})),
        sse(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})),
        sse(json!({"choices":[],"usage":{"prompt_tokens":11,"completion_tokens":7}})),
        "data: [DONE]\n\n".into(),
    ].concat();
    // Every byte boundary is a legal network chunk boundary.
    for chunk_size in [1, 2, 3, 7, 31, 4096] {
        let mut decoder = StreamDecoder::new(false);
        let mut visible = String::new();
        for chunk in wire.as_bytes().chunks(chunk_size) {
            for delta in decoder.push(chunk).unwrap() {
                visible.push_str(&delta);
            }
        }
        decoder.flush().unwrap();
        let result = decoder.finish().unwrap();
        assert_eq!(visible, "Hello 🌒");
        assert_eq!(result.text, visible);
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].id, "call_a");
        assert_eq!(result.tool_calls[0].arguments["path"], "src/main.rs");
        assert_eq!(result.tool_calls[1].arguments["pattern"], "TODO");
        assert_eq!(result.usage.total_tokens, 18);
    }
}

#[test]
fn compatible_stream_continues_unindexed_argument_deltas() {
    let wire = [
        sse(json!({"choices":[{"delta":{"tool_calls":[{"id":"call_a","function":{"name":"read_file","arguments":"{\"pa"}}]}}]})),
        sse(json!({"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"th\":\"README.md\"}"}}]}}]})),
        sse(json!({"choices":[{"delta":{"tool_calls":[{"id":"call_b","function":{"name":"search_text","arguments":"{\"pattern\":\"TODO\"}"}}]}}]})),
        sse(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})),
        "data: [DONE]\n\n".into(),
    ]
    .concat();
    for chunk_size in [1, 5, 4096] {
        let mut decoder = StreamDecoder::new(false);
        for chunk in wire.as_bytes().chunks(chunk_size) {
            decoder.push(chunk).unwrap();
        }
        decoder.flush().unwrap();
        let result = decoder.finish().unwrap();
        assert_eq!(result.tool_calls.len(), 2);
        assert_eq!(result.tool_calls[0].id, "call_a");
        assert_eq!(result.tool_calls[0].arguments["path"], "README.md");
        assert_eq!(result.tool_calls[1].id, "call_b");
        assert_eq!(result.tool_calls[1].arguments["pattern"], "TODO");
    }
}

#[test]
fn compatible_stream_ignores_repeated_call_identity() {
    let wire = [
        sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]})),
        sse(json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_a","function":{"name":"read_file","arguments":"\"README.md\"}"}}]}}]})),
        sse(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})),
        "data: [DONE]\n\n".into(),
    ]
    .concat();
    let mut decoder = StreamDecoder::new(false);
    decoder.push(wire.as_bytes()).unwrap();
    decoder.flush().unwrap();
    let result = decoder.finish().unwrap();
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0].id, "call_a");
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(result.tool_calls[0].arguments["path"], "README.md");
}

#[test]
fn incomplete_invalid_and_cut_short_calls_are_never_accepted() {
    let start = sse(
        json!({"choices":[{"delta":{"tool_calls":[{"index":0,"id":"x","function":{"name":"exec","arguments":"{\"command\":"}}]}}]}),
    );
    let mut decoder = StreamDecoder::new(false);
    decoder.push(start.as_bytes()).unwrap();
    assert!(decoder.finish().is_err());
    for reason in ["length", "content_filter", "tool_calls"] {
        let mut decoder = StreamDecoder::new(false);
        decoder.push(start.as_bytes()).unwrap();
        decoder
            .push(sse(json!({"choices":[{"delta":{},"finish_reason":reason}]})).as_bytes())
            .unwrap();
        assert!(decoder.finish().is_err());
    }
    let mut decoder = StreamDecoder::new(false);
    assert!(decoder.push(b"data: {invalid}\n\n").is_err());
    let mut decoder = StreamDecoder::new(true);
    assert!(decoder.push(&vec![b'a'; 1_000_001]).is_err());
}

#[test]
fn ollama_native_stream_retains_calls_and_usage_but_not_private_thinking() {
    let wire=[json!({"message":{"content":"Inspecting","thinking":"private scratch"},"done":false}),
        json!({"message":{"content":"","tool_calls":[{"function":{"name":"read_file","arguments":{"path":"README.md"}}}]},"done":false}),
        json!({"message":{"content":""},"done":true,"done_reason":"stop","prompt_eval_count":20,"eval_count":4})]
        .iter().map(|v|format!("{v}\n")).collect::<String>();
    let mut decoder = StreamDecoder::new(true);
    for chunk in wire.as_bytes().chunks(13) {
        decoder.push(chunk).unwrap();
    }
    let result = decoder.finish().unwrap();
    assert_eq!(result.text, "Inspecting");
    assert_eq!(result.usage.total_tokens, 24);
    assert_eq!(result.tool_calls[0].name, "read_file");
    assert_eq!(result.tool_calls[0].arguments["path"], "README.md");
}

async fn fixture(
    body: String,
    content_type: &str,
    hold: bool,
) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let content_type = content_type.to_owned();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 1024];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            if count == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        line.strip_prefix("content-length:")
                            .and_then(|s| s.trim().parse::<usize>().ok())
                    })
                    .unwrap_or(0);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).await.unwrap();
        if hold {
            tokio::time::sleep(Duration::from_secs(30)).await;
        }
        for chunk in body.as_bytes().chunks(7) {
            if socket.write_all(chunk).await.is_err() {
                return;
            }
        }
    });
    (endpoint, task)
}

#[tokio::test]
async fn real_http_transport_handles_streaming_and_nonstreaming_fallback() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    for (content_type, body) in [
        (
            "text/event-stream",
            sse(json!({"choices":[{"delta":{"content":"connected"},"finish_reason":"stop"}]}))
                + "data: [DONE]\n\n",
        ),
        (
            "application/json",
            json!({"choices":[{"message":{"content":"connected"},"finish_reason":"stop"}]})
                .to_string(),
        ),
    ] {
        let (endpoint, task) = fixture(body, content_type, false).await;
        let config = ModelConfig {
            provider: "local".into(),
            name: "fixture".into(),
            endpoint,
            ..Default::default()
        };
        let client = ModelClient::new(config, &paths).unwrap();
        let mut visible = String::new();
        let result = client
            .chat(
                &[json!({"role":"user","content":"hello"})],
                &[],
                CancellationToken::new(),
                |text| visible.push_str(text),
            )
            .await
            .unwrap();
        assert_eq!(result.text, "connected");
        assert_eq!(visible, "connected");
        task.await.unwrap();
    }
}

#[tokio::test]
async fn cancellation_interrupts_a_provider_that_never_sends_a_body() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let (endpoint, task) = fixture("waiting".into(), "text/event-stream", true).await;
    let client = ModelClient::new(
        ModelConfig {
            provider: "local".into(),
            endpoint,
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let cancel = CancellationToken::new();
    let child = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        child.cancel();
    });
    let start = std::time::Instant::now();
    let result = client.chat(&[], &[], cancel, |_| {}).await;
    task.abort();
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert!(start.elapsed() < Duration::from_secs(2));
}

#[tokio::test]
async fn malformed_full_response_is_an_error_instead_of_a_panic() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let (endpoint, task) = fixture("{\"choices\":[]}".into(), "application/json", false).await;
    let client = ModelClient::new(
        ModelConfig {
            provider: "local".into(),
            endpoint,
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    assert!(client
        .chat(&[], &[], CancellationToken::new(), |_| {})
        .await
        .is_err());
    task.await.unwrap();
}

#[test]
fn thousands_of_frames_in_one_network_chunk_preserve_all_text() {
    let mut wire = sse(json!({"choices":[{"delta":{"content":"word "}}]})).repeat(10_000);
    wire.push_str(&sse(
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ));
    let mut decoder = StreamDecoder::new(false);
    let deltas = decoder.push(wire.as_bytes()).unwrap();
    assert_eq!(deltas.len(), 10_000);
    assert_eq!(decoder.finish().unwrap().text, "word ".repeat(10_000));
}

async fn status_fixture(status: u16, body: &str) -> (String, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let body = body.to_owned();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = vec![0; 4096];
        let _ = socket.read(&mut request).await;
        let payload = format!(
            "HTTP/1.1 {status} ERR\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(payload.as_bytes()).await;
    });
    (endpoint, task)
}

#[tokio::test]
async fn provider_http_429_and_500_do_not_execute_tools() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    // The fixture listens on 127.0.0.1, so its 500 is a local server's.
    for (status, needle) in [
        (429, "rate limit"),
        (500, "HTTP 500; the model server on this computer failed"),
    ] {
        let (endpoint, task) = status_fixture(status, "{\"error\":\"nope\"}").await;
        let client = ModelClient::new(
            ModelConfig {
                provider: "local".into(),
                name: "fixture".into(),
                endpoint,
                ..Default::default()
            },
            &paths,
        )
        .unwrap();
        let error = client
            .chat(
                &[json!({"role":"user","content":"hello"})],
                &[],
                CancellationToken::new(),
                |_| {},
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains(needle), "status {status} error was {error}");
        task.abort();
    }
}

#[test]
fn ollama_and_openai_request_bodies_carry_vision_payloads() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let mut client = ModelClient::new(
        ModelConfig {
            provider: "ollama".into(),
            name: "gemma4:12b".into(),
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let messages = vec![
        json!({"role":"system","content":"Instructions"}),
        json!({
            "role":"user",
            "content":"What is shown?",
            "images":["aGVsbG8="]
        }),
    ];
    let body = client.request_body(&messages, &[], 128);
    let converted = body["messages"].as_array().unwrap();
    let user = converted.iter().find(|m| m["role"] == "user").unwrap();
    assert_eq!(user["content"], "What is shown?");
    assert_eq!(user["images"][0], "aGVsbG8=");

    client.config.provider = "openai".into();
    let openai_messages = vec![json!({
        "role":"user",
        "content":[
            {"type":"text","text":"Describe"},
            {"type":"image_url","image_url":{"url":"data:image/png;base64,aGVsbG8="}}
        ]
    })];
    let openai = client.request_body(&openai_messages, &[], 128);
    let content = openai["messages"][0]["content"].as_array().unwrap();
    assert_eq!(content[0]["type"], "text");
    assert_eq!(content[1]["type"], "image_url");
    assert!(content[1]["image_url"]["url"]
        .as_str()
        .unwrap()
        .starts_with("data:image/png;base64,"));
    // Text-only path stays a plain string for non-vision turns.
    let plain = client.request_body(&[json!({"role":"user","content":"hi"})], &[], 64);
    assert_eq!(plain["messages"][0]["content"], "hi");
}

#[test]
fn duplicate_tool_ids_in_distinct_slots_reject_the_whole_response() {
    let private_id = "sk-testAbCdEfGhIjKlMnOpQrStUvWxYz012345";
    let calls = json!([
        {"index":0,"id":private_id,"function":{"name":"write_file","arguments":"{\"path\":\"first.txt\",\"content\":\"first\"}"}},
        {"index":1,"id":private_id,"function":{"name":"write_file","arguments":"{\"path\":\"second.txt\",\"content\":\"second\"}"}}
    ]);
    for ollama in [false, true] {
        let wire = if ollama {
            format!(
                "{}\n",
                json!({"message":{"tool_calls":calls},"done":true,"done_reason":"stop","prompt_eval_count":41,"eval_count":7})
            )
        } else {
            [sse(json!({"choices":[{"delta":{"tool_calls":calls},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":41,"completion_tokens":7,"total_tokens":48}})), "data: [DONE]\n\n".to_owned()].concat()
        };
        for chunk_size in [1, 7, 4096] {
            let mut decoder = StreamDecoder::new(ollama);
            for chunk in wire.as_bytes().chunks(chunk_size) {
                assert!(decoder.push(chunk).unwrap().is_empty());
            }
            decoder.flush().unwrap();
            let metadata = decoder.metadata(Some(200), wire.len());
            assert_eq!(metadata.tool_call_slots, 2);
            assert_eq!(metadata.reported_usage().unwrap().total_tokens, 48);
            assert!(!serde_json::to_string(&metadata)
                .unwrap()
                .contains(private_id));
            let error = decoder.finish().unwrap_err();
            assert!(
                error.to_string().contains("duplicate tool call IDs"),
                "{error}"
            );
            assert!(!error.to_string().contains(private_id));
            assert!(
                shadowcode_core::retry::classify(&error).is_none(),
                "Malformed call identity must not become a transport retry"
            );
        }
    }
}

#[test]
fn tool_id_uniqueness_is_response_scoped_and_missing_ids_still_work() {
    for _ in 0..2 {
        let mut decoder = StreamDecoder::new(false);
        decoder.push(sse(json!({"choices":[{"delta":{"tool_calls":[
            {"index":0,"id":"valid-reused-id","function":{"name":"read_file","arguments":"{\"path\":\"a.txt\"}"}},
            {"index":1,"function":{"name":"read_file","arguments":"{\"path\":\"b.txt\"}"}},
            {"index":2,"id":"","function":{"name":"read_file","arguments":"{\"path\":\"c.txt\"}"}}
        ]},"finish_reason":"tool_calls"}]})).as_bytes()).unwrap();
        let response = decoder.finish().unwrap();
        assert_eq!(response.tool_calls.len(), 3);
        assert_eq!(response.tool_calls[0].id, "valid-reused-id");
        let ids: std::collections::BTreeSet<_> =
            response.tool_calls.iter().map(|c| &c.id).collect();
        assert_eq!(ids.len(), 3);
        assert!(ids.iter().all(|id| !id.is_empty()));
    }
}

#[test]
fn unindexed_duplicate_object_calls_are_refused_without_losing_later_usage() {
    let private_id = "sk-testAbCdEfGhIjKlMnOpQrStUvWxYz012345";
    let calls = json!([
        {"id":private_id,"function":{"name":"write_file","arguments":{"path":"first.txt","content":"first"}}},
        {"id":private_id,"function":{"name":"write_file","arguments":{"path":"second.txt","content":"second"}}}
    ]);
    for ollama in [false, true] {
        let wire = if ollama {
            [json!({"message":{"tool_calls":calls},"done":false}),
             json!({"message":{},"done":true,"done_reason":"stop","prompt_eval_count":41,"eval_count":7})]
                .iter().map(|value| format!("{value}\n")).collect::<String>()
        } else {
            [sse(json!({"choices":[{"delta":{"tool_calls":calls}}]})),
             sse(json!({"choices":[{"delta":{},"finish_reason":"tool_calls"}]})),
             sse(json!({"choices":[],"usage":{"prompt_tokens":41,"completion_tokens":7,"total_tokens":48}})),
             "data: [DONE]\n\n".to_owned()].concat()
        };
        for chunk_size in [1, 7, 4096] {
            let mut decoder = StreamDecoder::new(ollama);
            for chunk in wire.as_bytes().chunks(chunk_size) {
                assert!(decoder.push(chunk).unwrap().is_empty());
            }
            decoder.flush().unwrap();
            let metadata = decoder.metadata(Some(200), wire.len());
            assert_eq!(
                metadata.reported_usage().unwrap().total_tokens,
                48,
                "Late reported tokens survive malformed identity refusal"
            );
            assert!(!serde_json::to_string(&metadata)
                .unwrap()
                .contains(private_id));
            let error = decoder.finish().unwrap_err();
            assert!(
                error.to_string().contains("duplicate tool call IDs"),
                "{error}"
            );
            assert!(!error.to_string().contains(private_id));
            assert!(shadowcode_core::retry::classify(&error).is_none());
        }
    }
}

/// A remote provider's refusal shows its JSON error message, redacted and
/// bounded, never an HTML error page; a local runtime's reply is shown as is.
#[test]
fn provider_error_details_are_the_providers_own_redacted_message() {
    use shadowcode_core::models::provider_error_detail;
    assert_eq!(
        provider_error_detail(br#"{"error":{"message":"Insufficient credits"}}"#, false).as_deref(),
        Some("Insufficient credits")
    );
    assert_eq!(
        provider_error_detail(br#"{"message":"Model not found"}"#, false).as_deref(),
        Some("Model not found")
    );
    assert_eq!(
        provider_error_detail(br#"{"error":"quota exhausted"}"#, false).as_deref(),
        Some("quota exhausted")
    );
    assert_eq!(
        provider_error_detail(b"<html><body>502 Bad Gateway</body></html>", false),
        None
    );
    let key = format!("{}{}", "sk-proj-", "AbCdEfGhIjKlMnOpQrStUvWxYz0123456789");
    let shown = provider_error_detail(
        json!({"error":{"message":format!("Incorrect API key provided: {key}")}})
            .to_string()
            .as_bytes(),
        false,
    )
    .unwrap();
    assert!(shown.starts_with("Incorrect API key provided"), "{shown}");
    assert!(!shown.contains("AbCdEfGh"), "{shown}");
    let long = json!({"error":{"message":"x".repeat(5000)}}).to_string();
    assert!(provider_error_detail(long.as_bytes(), false).unwrap().len() <= 300);
    assert_eq!(
        provider_error_detail(b"the request exceeds the available context size", true).as_deref(),
        Some("the request exceeds the available context size")
    );
}

/// A response that keeps streaming for longer than ten minutes (a large
/// answer from a slow local model) is not cut off by an overall request
/// deadline; only a stream with no bytes for 120 seconds stalls. Runs on
/// tokio's paused clock, so it takes no real time.
#[tokio::test(start_paused = true)]
async fn long_steady_streams_are_not_cut_off_by_a_total_deadline() {
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let count = socket.read(&mut buffer).await.unwrap();
            request.extend_from_slice(&buffer[..count]);
            if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                let length = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length:")?.trim().parse().ok())
                    .unwrap_or(0usize);
                if request.len() >= end + 4 + length {
                    break;
                }
            }
        }
        socket
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
        // Twelve minutes of steady output, one piece a minute.
        for n in 0..12 {
            let piece = sse(json!({"choices":[{"delta":{"content":format!("{n} ")}}]}));
            socket.write_all(piece.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        let end =
            sse(json!({"choices":[{"delta":{},"finish_reason":"stop"}]})) + "data: [DONE]\n\n";
        socket.write_all(end.as_bytes()).await.unwrap();
    });
    let client = ModelClient::new(
        ModelConfig {
            provider: "local".into(),
            name: "fixture".into(),
            endpoint,
            ..Default::default()
        },
        &paths,
    )
    .unwrap();
    let result = client
        .chat(
            &[json!({"role":"user","content":"write a lot"})],
            &[],
            CancellationToken::new(),
            |_| {},
        )
        .await
        .unwrap();
    assert_eq!(result.text, "0 1 2 3 4 5 6 7 8 9 10 11 ");
    server.await.unwrap();
}

/// A local runtime on a CPU-only machine sends its headers at once, then may
/// stay silent for minutes while it reads a long prompt. That silence before
/// the first byte must not stall the request; silence after the answer has
/// started still does after 120 seconds. Paused clock: no real waiting.
#[tokio::test(start_paused = true)]
async fn slow_local_prompt_processing_is_not_a_stall_but_a_silent_answer_is() {
    // `seen` is notified once the client has handed the first words to its
    // caller, so the silence after it starts only when the client is waiting.
    async fn serve(
        first_silence: u64,
        after_first: Option<(u64, std::sync::Arc<tokio::sync::Notify>)>,
    ) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                request.extend_from_slice(&buffer[..count]);
                if let Some(end) = request.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&request[..end]).to_lowercase();
                    let length = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length:")?.trim().parse().ok())
                        .unwrap_or(0usize);
                    if request.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            socket
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_secs(first_silence)).await;
            let piece = sse(json!({"choices":[{"delta":{"content":"ready "}}]}));
            socket.write_all(piece.as_bytes()).await.unwrap();
            if let Some((pause, seen)) = after_first {
                seen.notified().await;
                tokio::time::sleep(Duration::from_secs(pause)).await;
            }
            let end =
                sse(json!({"choices":[{"delta":{},"finish_reason":"stop"}]})) + "data: [DONE]\n\n";
            let _ = socket.write_all(end.as_bytes()).await;
        });
        endpoint
    }
    let root = tempfile::tempdir().unwrap();
    let paths = AppPaths::isolated(root.path()).unwrap();
    let client = |endpoint: String| {
        ModelClient::new(
            ModelConfig {
                provider: "local".into(),
                name: "fixture".into(),
                endpoint,
                ..Default::default()
            },
            &paths,
        )
        .unwrap()
    };
    let message = [json!({"role":"user","content":"read this long prompt"})];
    // Five minutes of prompt processing, then the answer.
    let slow = client(serve(300, None).await)
        .chat(&message, &[], CancellationToken::new(), |_| {})
        .await
        .unwrap();
    assert_eq!(slow.text, "ready ");
    // The answer starts, then goes silent for three minutes: a stall.
    let seen = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = seen.clone();
    let stalled = client(serve(1, Some((180, seen))).await)
        .chat(&message, &[], CancellationToken::new(), move |_| {
            signal.notify_one()
        })
        .await
        .unwrap_err();
    assert!(
        format!("{stalled:#}").contains("stalled for 120 seconds"),
        "{stalled:#}"
    );
}
