//! Codex identity-envelope regressions. Only local fake protocol frames/CLIs.
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::cli_agent::{adapter_for, CliAdapter, LaunchOptions, Step, Update, Vendor};
use std::path::Path;

fn frame(method: &str, params: Value) -> Value {
    json!({"jsonrpc":"2.0","method":method,"params":params})
}
fn response(id: u64, result: Value) -> Value {
    json!({"jsonrpc":"2.0","id":id,"result":result})
}
fn feed(adapter: &mut dyn CliAdapter, frame: Value) -> Step {
    adapter.on_line(&frame.to_string()).unwrap()
}
fn bound(turn_bound: bool) -> Box<dyn CliAdapter> {
    let mut a = adapter_for(Vendor::Codex, false);
    a.on_start(&LaunchOptions {
        binary: "fake-unused".into(),
        workspace: Path::new("/tmp").into(),
        model: "default".into(),
        read_only: false,
        resume: Some("owned-thread".into()),
        effort: None,
        legacy_effort: false,
        mcp_servers: vec![],
        rulebook: None,
    });
    a.prompt("one", &[]).unwrap();
    let start = feed(&mut *a, response(1, json!({})));
    assert!(start.send.iter().any(|line| line.contains("thread/resume")));
    feed(
        &mut *a,
        response(2, json!({"thread":{"id":"owned-thread"}})),
    );
    if turn_bound {
        feed(&mut *a, response(3, json!({"turn":{"id":"owned-turn"}})));
    }
    a
}
fn delta(thread: &str, turn: &str, text: &str) -> Value {
    frame(
        "item/agentMessage/delta",
        json!({"threadId":thread,"turnId":turn,"itemId":"same-message","delta":text}),
    )
}
fn approval(thread: &str, turn: &str, id: u64) -> Value {
    let mut f = frame(
        "item/commandExecution/requestApproval",
        json!({"threadId":thread,"turnId":turn,
        "itemId":"command","command":"printf fixture","startedAtMs":0}),
    );
    f["id"] = json!(id);
    f
}
fn terminal(thread: &str, turn: &str) -> Value {
    frame(
        "turn/completed",
        json!({"threadId":thread,"turn":{"id":turn,"status":"completed"}}),
    )
}
fn warning_only(step: &Step) -> bool {
    step.updates.iter().all(|u| matches!(u, Update::Warning(_)))
}
fn has_text(step: &Step, text: &str) -> bool {
    step.updates
        .iter()
        .any(|u| matches!(u, Update::Text(t) if t==text))
}

#[test]
fn foreign_thread_and_turn_cannot_emit_task_events_or_approvals() {
    for (thread, turn) in [
        ("foreign-thread", "owned-turn"),
        ("owned-thread", "foreign-turn"),
    ] {
        let mut a = bound(true);
        let frames = vec![
            delta(thread, turn, "FOREIGN_TEXT"),
            frame(
                "item/started",
                json!({"threadId":thread,"turnId":turn,"item":{"id":"x","type":"commandExecution","command":"printf foreign"}}),
            ),
            frame(
                "item/completed",
                json!({"threadId":thread,"turnId":turn,"item":{"id":"x","type":"fileChange","status":"completed","changes":[{"path":"foreign.txt"}]}}),
            ),
            frame(
                "thread/tokenUsage/updated",
                json!({"threadId":thread,"turnId":turn,"tokenUsage":{"last":{"inputTokens":999,"outputTokens":999}}}),
            ),
            frame(
                "error",
                json!({"threadId":thread,"turnId":turn,"willRetry":false,"error":{"codexErrorInfo":"usageLimitExceeded","message":"foreign quota"}}),
            ),
            terminal(thread, turn),
        ];
        for f in frames {
            let s = feed(&mut *a, f.clone());
            assert!(warning_only(&s), "foreign frame was published: {f}");
        }
        let s = feed(&mut *a, approval(thread, turn, 99));
        assert!(warning_only(&s));
        assert!(s
            .send
            .iter()
            .any(|line| serde_json::from_str::<Value>(line).unwrap()["error"].is_object()));
        assert!(
            a.approve("99", true).is_err(),
            "foreign permission remained answerable"
        );
        assert!(has_text(
            &feed(&mut *a, delta("owned-thread", "owned-turn", "owned")),
            "owned"
        ));
        assert!(feed(&mut *a, terminal("owned-thread", "owned-turn"))
            .updates
            .iter()
            .any(|u| matches!(u, Update::TurnCompleted { .. })));
    }
}

#[test]
fn modern_missing_or_malformed_identity_is_rejected_but_global_notices_survive() {
    for bad in [Value::Null, json!(""), json!(42), json!({})] {
        for key in ["threadId", "turnId"] {
            let mut a = bound(true);
            let mut f = delta("owned-thread", "owned-turn", "unscoped");
            f["params"][key] = bad.clone();
            assert!(warning_only(&feed(&mut *a, f)));
            let mut f = approval("owned-thread", "owned-turn", 99);
            f["params"][key] = bad.clone();
            let s = feed(&mut *a, f);
            assert!(warning_only(&s));
            assert!(!s.send.is_empty());
            assert!(a.approve("99", true).is_err());
        }
    }
    let mut a = bound(true);
    let s = feed(
        &mut *a,
        frame(
            "account/rateLimits/updated",
            json!({"rateLimits":{"primary":{"usedPercent":5}}}),
        ),
    );
    assert!(s.updates.iter().any(|u| matches!(u, Update::RateLimits(_))));
    let s = feed(
        &mut *a,
        frame(
            "configWarning",
            json!({"summary":"global configuration notice"}),
        ),
    );
    assert!(s
        .updates
        .iter()
        .any(|u| matches!(u,Update::Warning(t) if t=="global configuration notice")));
}

#[test]
fn early_frames_wait_for_authoritative_response_then_replay_only_matching_turn() {
    let mut a = bound(false);
    for f in [
        delta("owned-thread", "foreign-turn", "foreign early"),
        approval("owned-thread", "foreign-turn", 98),
        frame(
            "turn/started",
            json!({"threadId":"owned-thread","turn":{"id":"foreign-turn"}}),
        ),
        delta("owned-thread", "owned-turn", "owned early"),
        approval("owned-thread", "owned-turn", 99),
    ] {
        assert!(
            feed(&mut *a, f).updates.is_empty(),
            "unbound frame escaped before response"
        );
    }
    let s = feed(&mut *a, response(3, json!({"turn":{"id":"owned-turn"}})));
    assert!(has_text(&s, "owned early"));
    assert!(!has_text(&s, "foreign early"));
    assert_eq!(
        s.updates
            .iter()
            .filter(|u| matches!(u, Update::Approval(_)))
            .count(),
        1
    );
    assert!(s
        .send
        .iter()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["id"] == 98));
    assert!(a.approve("98", true).is_err());
    assert!(a.approve("99", true).unwrap()[0].contains("accept"));
}

#[test]
fn early_frames_have_explicit_count_and_byte_limits() {
    for oversized in [false, true] {
        let mut a = bound(false);
        let queued = feed(&mut *a, approval("owned-thread", "owned-turn", 99));
        assert!(queued.updates.is_empty());
        let mut failed = false;
        for n in 0..129 {
            let text = if oversized {
                "x".repeat(1024 * 1024)
            } else {
                n.to_string()
            };
            let s = feed(&mut *a, delta("owned-thread", "owned-turn", &text));
            if s.updates.iter().any(|u| matches!(u, Update::TurnFailed(_))) {
                assert!(s
                    .send
                    .iter()
                    .any(|line| serde_json::from_str::<Value>(line).unwrap()["id"] == 99));
                failed = true;
                break;
            }
        }
        assert!(failed, "unbounded early frames");
        assert!(a.approve("99", true).is_err());
        assert!(
            feed(&mut *a, response(3, json!({"turn":{"id":"owned-turn"}})))
                .updates
                .is_empty()
        );
    }
}

#[test]
fn malformed_or_failed_start_retires_early_requests_and_late_responses() {
    for response_value in [
        response(3, json!({"turn":{}})),
        response(3, json!({"turn":{"id":""}})),
        json!({"jsonrpc":"2.0","id":3,"error":{"code":-1,"message":"start refused"}}),
    ] {
        let mut a = bound(false);
        feed(&mut *a, approval("owned-thread", "owned-turn", 99));
        let s = feed(&mut *a, response_value);
        assert!(s.updates.iter().any(|u| matches!(u, Update::TurnFailed(_))));
        assert!(s
            .send
            .iter()
            .any(|line| serde_json::from_str::<Value>(line).unwrap()["id"] == 99));
        assert!(a.approve("99", true).is_err());
        assert!(
            feed(&mut *a, response(3, json!({"turn":{"id":"owned-turn"}})))
                .updates
                .is_empty()
        );
        assert!(warning_only(&feed(
            &mut *a,
            delta("owned-thread", "owned-turn", "late")
        )));
    }
}

#[test]
fn responses_are_consumed_once_and_continuation_resets_item_and_permission_state() {
    let mut a = bound(true);
    feed(&mut *a, delta("owned-thread", "owned-turn", "first"));
    feed(&mut *a, approval("owned-thread", "owned-turn", 99));
    let complete = feed(&mut *a, terminal("owned-thread", "owned-turn"));
    assert!(complete
        .send
        .iter()
        .any(|line| serde_json::from_str::<Value>(line).unwrap()["id"] == 99));
    assert!(a.approve("99", true).is_err());
    let lines = a.prompt("two", &[]).unwrap();
    let request: Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    let id = request["id"].as_u64().unwrap();
    for stale in [
        response(1, json!({})),
        response(2, json!({"thread":{"id":"foreign-thread"}})),
        response(3, json!({"turn":{"id":"owned-turn"}})),
    ] {
        let s = feed(&mut *a, stale);
        assert!(s.send.is_empty());
        assert!(s.updates.is_empty());
    }
    assert_eq!(a.native_session().as_deref(), Some("owned-thread"));
    feed(&mut *a, terminal("owned-thread", "owned-turn"));
    let replay = feed(&mut *a, response(id, json!({"turn":{"id":"second-turn"}})));
    assert!(warning_only(&replay));
    let s = feed(
        &mut *a,
        frame(
            "item/completed",
            json!({"threadId":"owned-thread","turnId":"second-turn","item":{"id":"same-message","type":"agentMessage","text":"second"}}),
        ),
    );
    assert!(
        has_text(&s, "second"),
        "prior turn's streamed item IDs suppressed the new answer"
    );
    assert!(feed(&mut *a, terminal("owned-thread", "second-turn"))
        .updates
        .iter()
        .any(|u| matches!(u, Update::TurnCompleted { .. })));
}

#[test]
fn legacy_approvals_require_matching_conversation_and_bound_active_turn() {
    for method in ["execCommandApproval", "applyPatchApproval"] {
        let mut a = bound(false);
        let mut f = frame(
            method,
            json!({"conversationId":"owned-thread","callId":"legacy","command":["printf","fixture"],"fileChanges":{}}),
        );
        f["id"] = json!(99);
        assert!(warning_only(&feed(&mut *a, f.clone())));
        assert!(a.approve("99", true).is_err());
        feed(&mut *a, response(3, json!({"turn":{"id":"owned-turn"}})));
        let own = feed(&mut *a, f.clone());
        assert!(own.updates.iter().any(|u| matches!(u, Update::Approval(_))));
        assert!(a.approve("99", true).unwrap()[0].contains("approved"));
        for bad in [Value::Null, json!("foreign-thread"), json!(2)] {
            f["params"]["conversationId"] = bad;
            assert!(warning_only(&feed(&mut *a, f.clone())));
            assert!(a.approve("99", true).is_err());
        }
    }
}

#[test]
fn foreign_duplicate_request_ids_retire_bound_and_early_approval_authority() {
    for turn_bound in [false, true] {
        let mut a = bound(turn_bound);
        feed(&mut *a, approval("owned-thread", "owned-turn", 99));
        let s = feed(&mut *a, approval("foreign-thread", "foreign-turn", 99));
        assert!(s.updates.iter().any(|u| matches!(u, Update::TurnFailed(_))));
        assert!(
            a.approve("99", true).is_err(),
            "already rejected request remained approvable"
        );
        let late = feed(&mut *a, response(3, json!({"turn":{"id":"owned-turn"}})));
        assert!(late.updates.is_empty());
    }
}

#[test]
fn matching_current_frames_publish_and_global_token_refresh_stays_declined() {
    let mut a = bound(true);
    assert!(has_text(
        &feed(&mut *a, delta("owned-thread", "owned-turn", "valid")),
        "valid"
    ));
    let s = feed(
        &mut *a,
        frame(
            "thread/tokenUsage/updated",
            json!({"threadId":"owned-thread","turnId":"owned-turn","tokenUsage":{"last":{"inputTokens":3,"outputTokens":2}}}),
        ),
    );
    assert!(s.updates.iter().any(|u| matches!(
        u,
        Update::Usage {
            input: 3,
            output: 2,
            ..
        }
    )));
    let own = feed(&mut *a, approval("owned-thread", "owned-turn", 99));
    assert!(own.updates.iter().any(|u| matches!(u, Update::Approval(_))));
    assert!(a.approve("99", false).unwrap()[0].contains("decline"));
    let mut token = frame("account/chatgptAuthTokens/refresh", json!({}));
    token["id"] = json!(100);
    let token = feed(&mut *a, token);
    assert!(warning_only(&token));
    assert!(token.send[0].contains("-32601"));
    assert!(feed(&mut *a, terminal("owned-thread", "owned-turn"))
        .updates
        .iter()
        .any(|u| matches!(u, Update::TurnCompleted { .. })));
}

#[tokio::test]
async fn real_runner_keeps_owned_turn_running_after_foreign_frames() {
    use shadowcode_core::{
        config::Config,
        paths::AppPaths,
        service::{Request, Service},
    };
    use std::{fs, time::Duration};
    use vendor_support::{cli_agents, FakeCodex};
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let fake = FakeCodex::new(
        root.path(),
        json!({"auth":"chatgpt","turn":"scope_boundary"}),
    );
    let script = fs::read_to_string(fake.binary()).unwrap();
    let anchor = "        mode = C.get(\"turn\", \"ok\")";
    assert_eq!(script.matches(anchor).count(), 1);
    let body = r#"
        if mode == "scope_boundary":
            mark("scope_pid", str(os.getpid()))
            send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"foreign-thread","turnId":"foreign-turn","itemId":"foreign-message","delta":"FOREIGN_TEXT"}})
            send({"jsonrpc":"2.0","method":"item/started","params":{"threadId":"foreign-thread","turnId":"foreign-turn","item":{"id":"foreign-tool","type":"commandExecution","command":"printf foreign"}}})
            send({"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"threadId":"foreign-thread","turnId":"foreign-turn","tokenUsage":{"last":{"inputTokens":999,"outputTokens":999}}}})
            send({"jsonrpc":"2.0","id":99,"method":"item/commandExecution/requestApproval","params":{"threadId":"foreign-thread","turnId":"foreign-turn","itemId":"foreign-command","command":"printf foreign"}})
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"previous-turn","status":"completed"}}})
            send({"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"threadId":"thr-1","turnId":"turn-1","tokenUsage":{"last":{"inputTokens":3,"outputTokens":2}}}})
            send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":"thr-1","turnId":"turn-1","itemId":"owned-message","delta":"OWNED_WAITING"}})
            mark("scope_written")
            until = time.monotonic() + 5
            while not os.path.exists(os.path.join(HERE,"scope_release")) and time.monotonic() < until:
                time.sleep(0.005)
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":"thr-1","turn":{"id":"turn-1","status":"completed"}}})
            continue
"#;
    fs::write(
        fake.binary(),
        script.replace(anchor, &format!("{anchor}{body}")),
    )
    .unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({"cli_agents":cli_agents(&fake),"trusted_workspaces":[project]}),
    )
    .unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let job=service.dispatch(Request{method:"POST".into(),path:"/api/jobs".into(),body:json!({"workspace":project,"task":"scope fixture","model":"cli:codex:gpt-6-astra"})}).await.unwrap();
    let id = job["id"].as_str().unwrap();
    let sid = job["session_id"].as_str().unwrap();
    // Observe processing, not only a write into the kernel pipe. The owned
    // text is ordered after all malformed frames; baseline may instead stop.
    let observed = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let current = service.engine.job(id).unwrap().unwrap();
            let events = service
                .engine
                .store()
                .events_after(sid, 0, None, 1000)
                .unwrap();
            let own = events.iter().any(|e| {
                e["type"] == "model.stream"
                    && e["payload"]["text"]
                        .as_str()
                        .is_some_and(|s| s.contains("OWNED_WAITING"))
            });
            if fake.marker("scope_written").is_some() && (own || current.status != "running") {
                break (
                    current.status,
                    events,
                    service.engine.approvals().list(Some(sid)),
                );
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await;
    // Always release the fake and shut down before any behavioral assertion.
    fs::write(fake.dir.join("scope_release"), "release").unwrap();
    let completion = tokio::time::timeout(Duration::from_secs(5), service.engine.wait(id)).await;
    if completion.is_err() {
        let _ = service.engine.cancel(id).await;
    }
    let shutdown = service.engine.shutdown().await;
    let final_events = service
        .engine
        .store()
        .events_after(sid, 0, None, 1000)
        .unwrap();
    let pid = fake.marker("scope_pid").unwrap_or_default();
    #[cfg(target_os = "linux")]
    let reaped = !Path::new(&format!("/proc/{}", pid.trim())).exists();
    #[cfg(not(target_os = "linux"))]
    let reaped = true;
    eprintln!(
        "scope observation={} owned_pid={} reaped={} final_events={}",
        observed
            .as_ref()
            .map(|(s, _, a)| json!({"status":s,"pending_approvals":a.len()}))
            .unwrap_or(json!({"timeout":true})),
        pid.trim(),
        reaped,
        final_events.len()
    );
    assert!(shutdown.is_ok());
    assert!(reaped, "owned fixture child was not reaped");
    let (status, events, pending) = observed.expect("processing barrier was not observed");
    assert_eq!(status, "running", "foreign terminal ended the current task");
    assert!(pending.is_empty(), "foreign approval reached the host");
    assert!(!events.iter().any(|e| e["type"] == "approval.requested"));
    assert!(!final_events
        .iter()
        .any(|e| e["type"] == "tool.started" && e["payload"]["call_id"] == "foreign-tool"));
    assert!(!final_events.iter().any(|e| e["type"] == "model.stream"
        && e["payload"]["text"]
            .as_str()
            .is_some_and(|t| t.contains("FOREIGN_TEXT"))));
    let done = completion.expect("owned terminal did not finish").unwrap();
    assert_eq!(done.status, "completed");
    assert_eq!(done.summary, "OWNED_WAITING");
    assert!(!done.usage_is_estimated);
    assert_eq!(done.usage.prompt_tokens, 3);
    assert_eq!(done.usage.completion_tokens, 2);
    assert_eq!(done.usage.total_tokens, 5);
    assert_eq!(
        final_events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        1
    );
    assert!(fake.marker("exec_ran").is_none());
}
