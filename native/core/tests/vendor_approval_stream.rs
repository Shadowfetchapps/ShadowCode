//! Approval races through an actual bounded subprocess pipe, without a vendor.
#![cfg(unix)]
use serde_json::Value;
use shadowcode_core::{
    approvals::ApprovalHub,
    cli_agent::{runner, CliAgentsConfig, LaunchOptions, Vendor},
    events::TaskEvents,
    paths::AppPaths,
    steering::SteerControl,
    store::Store,
};
use std::{collections::HashSet, fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
MODE = open(os.path.join(HERE, 'mode')).read()
OP = 'sk-test_ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789abcdefghijk' if MODE == 'completed_secret' else 'operation'
turn_count = 0
def mark(name, data=''):
    with open(os.path.join(HERE, name), 'w') as f: f.write(data)
def send(frame):
    print(json.dumps(frame), flush=True)
def update(value):
    send({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'fixture','update':value}})
def barrier(name="change-now"):
    until = time.monotonic() + 10
    while not os.path.exists(os.path.join(HERE, name)):
        if time.monotonic() > until: sys.exit(2)
        time.sleep(.005)
mark('pid', str(os.getpid()))
prompt_id = None
for raw in sys.stdin:
    frame = json.loads(raw)
    method = frame.get('method')
    if method == 'initialize':
        send({'jsonrpc':'2.0','id':frame['id'],'result':{'protocolVersion':1,'agentCapabilities':{}}})
    elif method == 'session/new':
        send({'jsonrpc':'2.0','id':frame['id'],'result':{'sessionId':'fixture'}})
    elif method == 'session/prompt':
        prompt_id = frame['id']
        turn_count += 1
        if MODE == 'pause' and turn_count == 1:
            mark('first-active')
            continue
        if MODE == 'pause' and turn_count == 3:
            update({'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'both pauses honored'}})
            send({'jsonrpc':'2.0','id':prompt_id,'result':{'stopReason':'end_turn'}})
            continue
        edit = MODE == 'edit'
        tool = {'sessionUpdate':'tool_call','toolCallId':OP,'kind':'edit' if edit else 'execute','status':'pending',
            'rawInput':{'path':'helpers.py'} if edit else {'command':'printf harmless'},
            'content':[{'type':'diff','path':'helpers.py','oldText':'before','newText':'safe'}] if edit else []}
        update(tool)
        if MODE == 'noise':
            for _ in range(20): print('fixture diagnostic outside JSON', flush=True)
        send({'jsonrpc':'2.0','id':77,'method':'session/request_permission','params':{'sessionId':'fixture',
            'toolCall':{'toolCallId':OP},'options':[{'optionId':'allow','kind':'allow_once'},{'optionId':'deny','kind':'reject_once'}]}})
        mark('permission-written')
        barrier()
        if MODE == 'noise':
            for _ in range(20): print('fixture diagnostic outside JSON', flush=True)
        if MODE == 'command':
            update({'sessionUpdate':'tool_call_update','toolCallId':OP,'rawInput':{'command':'printf changed'}})
        elif MODE == 'edit':
            update({'sessionUpdate':'tool_call_update','toolCallId':OP,'content':[{'type':'diff','path':'helpers.py','oldText':'before','newText':'changed'}]})
        elif MODE == 'stream':
            update({'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'visible while approval is pending'}})
        elif MODE in ('completed', 'completed_secret'):
            update({'sessionUpdate':'tool_call_update','toolCallId':OP,'status':'completed'})
        elif MODE == 'queued':
            send({'jsonrpc':'2.0','id':78,'method':'session/request_permission','params':{'sessionId':'fixture',
                'toolCall':{'toolCallId':'second','kind':'execute','rawInput':{'command':'printf second'}},
                'options':[{'optionId':'allow','kind':'allow_once'},{'optionId':'deny','kind':'reject_once'}]}})
        elif MODE in ('partial', 'partial_timeout'):
            mutation = json.dumps({'jsonrpc':'2.0','method':'session/update','params':{'sessionId':'fixture',
                'update':{'sessionUpdate':'tool_call_update','toolCallId':OP,'rawInput':{'command':'printf split mutation'}}}}) + '\n'
            split = len(mutation) // 2
            sys.stdout.write(mutation[:split]); sys.stdout.flush()
            mark('change-written')
            barrier('finish-fragment')
            sys.stdout.write(mutation[split:]); sys.stdout.flush()
        elif MODE in ('drain', 'drain_mutation'):
            # This complete burst fits in the pipe; the test deliberately holds
            # the host task at the barrier until every frame has been written.
            for _ in range(160 if MODE == 'drain' else 40): send({'method':'x'})
            if MODE == 'drain_mutation':
                update({'sessionUpdate':'tool_call_update','toolCallId':OP,'rawInput':{'command':'printf changed in queued burst'}})
        elif MODE == 'duplicate':
            send({'jsonrpc':'2.0','id':77,'method':'session/request_permission','params':{'sessionId':'fixture',
                'toolCall':{'toolCallId':'different','kind':'execute','rawInput':{'command':'printf replaced'}},
                'options':[{'optionId':'allow','kind':'allow_once'},{'optionId':'deny','kind':'reject_once'}]}})
        elif MODE in ('count', 'bytes'):
            for i in range(33 if MODE == 'count' else 4):
                send({'jsonrpc':'2.0','id':100+i,'method':'session/request_permission','params':{'sessionId':'fixture',
                    'toolCall':{'toolCallId':'extra-'+str(i),'kind':'execute','rawInput':{'command':'printf pending', 'fixture': 'x' * (2900000 if MODE == 'bytes' else 1)}},
                    'options':[{'optionId':'allow','kind':'allow_once'},{'optionId':'deny','kind':'reject_once'}]}})
        elif MODE == 'terminal':
            send({'jsonrpc':'2.0','id':prompt_id,'result':{'stopReason':'end_turn'}})
        mark('change-written')
    elif method == 'session/cancel':
        mark('cancel-'+str(turn_count))
        send({'jsonrpc':'2.0','id':prompt_id,'result':{'stopReason':'cancelled'}})
    elif frame.get('id') in (77, 78):
        mark('reply' if frame['id'] == 77 else 'second-reply', json.dumps(frame))
        outcome = frame.get('result',{}).get('outcome',{})
        if outcome.get('outcome') == 'selected' and outcome.get('optionId') == 'allow':
            mark('executed', 'the currently proposed operation ran')
        if MODE == 'pause': continue
        update({'sessionUpdate':'tool_call_update','toolCallId':OP if frame['id'] == 77 else 'second','status':'completed','rawOutput':{'exitCode':0,'stdout':'','stderr':''}})
        if MODE == 'queued' and not all(os.path.exists(os.path.join(HERE, f)) for f in ('reply', 'second-reply')): continue
        send({'jsonrpc':'2.0','id':prompt_id,'result':{'stopReason':'end_turn'}})
"#;

async fn fixture(mode: &str, answer: bool, cancel_run: bool) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let binary = root.path().join("fake-cursor");
    fs::write(&binary, FAKE).unwrap();
    fs::write(root.path().join("mode"), mode).unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let store = Arc::new(Store::open(&paths.database()).unwrap());
    let session = store.create_session(&project, "cli:cursor", "").unwrap();
    let session_id = session["id"].as_str().unwrap().to_owned();
    let task_id = store.create_task(&session_id, "approval race").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(64);
    let events = TaskEvents {
        store,
        session_id: session_id.clone(),
        task_id: task_id.clone(),
        sender,
    };
    let approvals = ApprovalHub::default();
    let cancel = CancellationToken::new();
    let steer = SteerControl::default();
    let config = CliAgentsConfig {
        approval_timeout_sec: 10,
        max_run_time_sec: if mode == "partial_timeout" { 1 } else { 5 },
        ..Default::default()
    };
    let options = LaunchOptions {
        binary: binary.to_string_lossy().into_owned(),
        workspace: project,
        model: "default".into(),
        ..Default::default()
    };
    let mut observed = false;
    let mut first_pause = false;
    let mut resumes = 0;
    let mut answered = HashSet::new();
    let must_fail = matches!(
        mode,
        "duplicate" | "count" | "bytes" | "drain" | "partial_timeout"
    );
    let result = tokio::time::timeout(Duration::from_secs(8), async {
        let run = runner::run(runner::Request {
            vendor: Vendor::Cursor, options, config: &config, prompt: "fixture".into(), images: vec![],
            session_id: session_id.clone(), task_id, job_id: "fixture-job".into(), events: &events,
            approvals: &approvals, cancel: cancel.clone(), steer: &steer, approvals_required: true, catalog: None, approval_route: None,
        });
        tokio::pin!(run);
        loop {
            tokio::select! {
                result = &mut run => break result,
                _ = tokio::time::sleep(Duration::from_millis(5)) => {
                    if mode == "partial" && !answered.is_empty() {
                        let resolved = events.store.events_after(&session_id, 0, None, 100).unwrap().iter()
                            .any(|event| event["type"] == "approval.resolved");
                        if resolved {
                            // Only release the second fragment after the host has consumed
                            // the user's answer; no guessed transport sleep is involved.
                            fs::write(root.path().join("finish-fragment"), "go").unwrap();
                        }
                    }
                    if mode == "pause" {
                        if !first_pause && root.path().join("first-active").exists() {
                            first_pause = true;
                            steer.pause(Default::default()).unwrap();
                            steer.set_instruction("continue to the approval fixture").unwrap();
                        }
                        let parked = events.store.events_after(&session_id, 0, None, 100).unwrap().iter()
                            .filter(|event| event["type"] == "agent.paused").count();
                        if parked > resumes {
                            assert!(approvals.list(Some(&session_id)).is_empty(), "pause must retire pending cards");
                            resumes = parked;
                            steer.resume().unwrap();
                        }
                    }
                    if let Some(pending) = approvals.list(Some(&session_id)).into_iter().next() {
                        if !observed {
                            observed = true;
                            if mode == "completed_secret" {
                                assert_eq!(pending.arguments["tool_call_id"], "[redacted secret]");
                            }
                            // The writer cannot mutate the proposal until the host dialog exists.
                            fs::write(root.path().join("change-now"), "go").unwrap();
                            if matches!(mode, "drain" | "drain_mutation") {
                                // Hold this current-thread host future so the entire burst is
                                // already readable when approval resolves. The writer is a
                                // real independent OS process, not another async task.
                                let until = std::time::Instant::now() + Duration::from_secs(2);
                                while !root.path().join("change-written").is_file() {
                                    assert!(std::time::Instant::now() < until, "writer barrier timed out");
                                    std::thread::sleep(Duration::from_millis(1));
                                }
                            }
                        }
                        let streamed = mode != "stream" || events.store.events_after(&session_id, 0, None, 100).unwrap()
                            .iter().any(|event| event["type"] == "model.stream" && event["payload"]["text"] == "visible while approval is pending");
                        if !answered.contains(&pending.id) && root.path().join("change-written").is_file() && !matches!(mode, "terminal" | "completed" | "completed_secret" | "count" | "bytes") && streamed {
                            answered.insert(pending.id.clone());
                            // This barrier proves the changed frame is in the pipe before the
                            // user answer; no sleep is used to infer transport progress.
                            if mode == "pause" {
                                steer.pause(Default::default()).unwrap();
                                steer.set_instruction("finish after the second pause without running the tool").unwrap();
                            }
                            else if cancel_run { cancel.cancel(); }
                            else { approvals.decide(&pending.id, &session_id, answer).unwrap(); }
                        }
                    }
                }
            }
        }
    }).await;
    assert!(observed, "fixture never reached a real pending approval");
    assert!(
        approvals.list(Some(&session_id)).is_empty(),
        "ended task left a live approval behind"
    );
    let result = result.expect("approval transport fixture exceeded deadline");
    if cancel_run {
        assert!(result.is_err());
        assert!(!root.path().join("executed").exists());
        return;
    }
    if must_fail {
        let error = result
            .expect_err("invalid permission stream must fail closed")
            .to_string();
        assert!(
            error.contains(if mode == "duplicate" {
                "outstanding permission request ID"
            } else if mode == "partial_timeout" {
                "active runtime limit"
            } else if mode == "drain" {
                "bounded approval reconciliation limit"
            } else {
                "pending permission request limit"
            }),
            "{error}"
        );
        assert!(!root.path().join("executed").exists());
        return;
    }
    result.unwrap();
    if mode == "terminal" {
        assert!(!root.path().join("executed").exists());
        return;
    }
    let reply: Value =
        serde_json::from_slice(&fs::read(root.path().join("reply")).unwrap()).unwrap();
    let allowed = reply["result"]["outcome"]["outcome"] == "selected"
        && reply["result"]["outcome"]["optionId"] == "allow";
    assert_eq!(
        allowed,
        matches!(mode, "unchanged" | "stream" | "queued" | "noise") && answer,
        "stale proposal received a selected allow: {reply}"
    );
    assert_eq!(root.path().join("executed").exists(), allowed);
    if mode == "pause" {
        assert_eq!(resumes, 2);
        assert!(root.path().join("cancel-1").exists());
        assert!(root.path().join("cancel-2").exists());
    }
    if mode == "queued" {
        let second: Value =
            serde_json::from_slice(&fs::read(root.path().join("second-reply")).unwrap()).unwrap();
        assert_eq!(second["result"]["outcome"]["optionId"], "allow");
        assert_eq!(
            answered.len(),
            2,
            "each distinct operation needs its own answer"
        );
    }
}

#[tokio::test]
async fn command_update_written_during_user_wait_invalidates_the_grant() {
    fixture("command", true, false).await;
}
#[tokio::test]
async fn edit_update_written_during_user_wait_invalidates_the_grant() {
    fixture("edit", true, false).await;
}
#[tokio::test]
async fn unchanged_operation_still_honors_allow_and_deny() {
    fixture("unchanged", true, false).await;
    fixture("unchanged", false, false).await;
}
#[tokio::test]
async fn terminal_turn_retires_the_pending_dialog_without_an_answer() {
    fixture("terminal", false, false).await;
}
#[tokio::test]
async fn cancellation_during_approval_never_sends_a_grant() {
    fixture("unchanged", true, true).await;
}

#[tokio::test]
async fn text_is_processed_and_flushed_while_an_approval_waits() {
    fixture("stream", true, false).await;
}
#[tokio::test]
async fn completed_tool_retires_its_dialog_without_an_answer() {
    fixture("completed", false, false).await;
}
#[tokio::test]
async fn duplicate_outstanding_request_id_never_reuses_the_old_decision() {
    fixture("duplicate", true, false).await;
}
#[tokio::test]
async fn pending_approval_count_is_bounded_while_user_is_waiting() {
    fixture("count", false, false).await;
}
#[tokio::test]
async fn pending_approval_bytes_are_bounded_while_user_is_waiting() {
    fixture("bytes", false, false).await;
}

#[tokio::test]
async fn ready_frame_burst_cannot_hold_decision_reconciliation_open() {
    fixture("drain", true, false).await;
}

#[tokio::test]
async fn distinct_queued_permissions_keep_independent_user_decisions() {
    fixture("queued", true, false).await;
}

#[tokio::test]
async fn redacted_display_id_does_not_prevent_completed_tool_retirement() {
    fixture("completed_secret", false, false).await;
}
#[tokio::test]
async fn second_pause_retires_approval_and_interrupts_the_resumed_turn() {
    fixture("pause", true, false).await;
}

#[tokio::test]
async fn kernel_ready_mutation_is_reconciled_before_reactor_notification() {
    fixture("drain_mutation", true, false).await;
}

#[tokio::test]
async fn split_mutation_finishes_after_user_answer_before_any_grant() {
    fixture("partial", true, false).await;
}
#[tokio::test]
async fn unfinished_mutation_exhausts_active_deadline_without_a_grant() {
    fixture("partial_timeout", true, false).await;
}

#[tokio::test]
async fn valid_approval_resets_the_malformed_line_streak() {
    fixture("noise", true, false).await;
}
