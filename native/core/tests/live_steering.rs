mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    context,
    engine::{Engine, StartRequest},
    paths::AppPaths,
    workspace::Workspace,
};
use std::{fs, path::Path, sync::Arc, time::Duration};

fn response(text: &str, calls: Value) -> Value {
    let reason = if calls.as_array().is_some_and(|v| !v.is_empty()) {
        "tool_calls"
    } else {
        "stop"
    };
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":reason}],"usage":{"prompt_tokens":8,"completion_tokens":4,"total_tokens":12}})
}
fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn setup(endpoint: &str) -> (tempfile::TempDir, Engine) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("project")).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths,json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","context_limit":16384},"trusted_workspaces":[root.path().join("project")],"permissions":{"approve_shell":false},"agent":{"max_steps":12}})).unwrap();
    (root, Engine::open(paths).unwrap())
}
fn request(root: &Path, task: &str) -> StartRequest {
    StartRequest {
        workspace: root.join("project"),
        task: task.into(),
        session_id: None,
        model: None,
        mode: "code".into(),
        queue: false,
        images: Vec::new(),
        web: false,
    }
}

#[tokio::test]
async fn pause_does_not_replay_shell_and_steering_enters_next_context() {
    let requests = Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
    let captured = requests.clone();
    let server = support::server(move |index, body| {
        captured.lock().unwrap().push(body.clone());
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let value = match index {
            0 => response(
                "running once",
                json!([tool(
                    "e1",
                    "exec",
                    json!({"command":"printf started > running.flag; sleep 1; echo first-only"})
                )]),
            ),
            1 => {
                // After pause/resume, the next model turn must see steering text
                // and must not re-issue the first shell as a replay of history.
                let blob = body.to_string();
                assert!(
                    blob.contains("db/v2.sql") || blob.contains("do not refactor"),
                    "steering missing from model context: {blob}"
                );
                response("done with steering", json!([]))
            }
            _ => response("extra", json!([])),
        };
        (value, Duration::from_millis(30))
    })
    .await;
    assert!(server.requests.lock().unwrap().is_empty());
    let (root, engine) = setup(&server.endpoint);
    fs::create_dir_all(root.path().join("project/db")).unwrap();
    fs::write(root.path().join("project/db/v2.sql"), "-- v2\n").unwrap();
    let job = engine
        .start(request(root.path(), "Use the schema carefully"))
        .await
        .unwrap();
    // Pause while the first shell is still running so the next model turn
    // waits on resume and receives steering without replaying exec.
    for _ in 0..100 {
        let snap = engine.job(&job.id).unwrap().unwrap();
        if snap.status == "running" {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        while !root.path().join("project/running.flag").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    engine.pause_job(&job.id).unwrap();
    assert_eq!(engine.job(&job.id).unwrap().unwrap().status, "paused");
    assert!(engine
        .rewind_job(&job.id)
        .unwrap_err()
        .to_string()
        .contains("pause boundary"));
    engine
        .steer_job(
            &job.id,
            "do not refactor schema; use db/v2.sql",
            Some("db/v2.sql"),
        )
        .unwrap();
    // Let the in-flight shell finish under pause; resume afterward.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    assert_eq!(engine.rewind_job(&job.id).unwrap()["ok"], true);
    engine.resume_job(&job.id).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(8), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "completed", "{}", completed.summary);
    let tape = engine.store().messages(&job.id).unwrap();
    let execs = tape
        .iter()
        .filter(|m| m["role"] == "assistant")
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .filter(|c| c["function"]["name"] == "exec")
        .count();
    assert_eq!(execs, 1, "pause/resume must not replay shell tool calls");
    assert!(
        tape.iter().any(|m| m["role"] == "system"
            && m["content"]
                .as_str()
                .is_some_and(|c| c.contains("db/v2.sql"))),
        "steering system note missing"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn rewind_does_not_duplicate_side_effects() {
    let server = support::server(|index, _body| {
        let value = match index {
            0 => response(
                "write",
                json!([tool(
                    "w1",
                    "write_file",
                    json!({"path":"note.txt","content":"alpha"})
                )]),
            ),
            1 => response("done", json!([])),
            _ => response("extra", json!([])),
        };
        (value, Duration::ZERO)
    })
    .await;
    assert!(server.requests.lock().unwrap().is_empty());
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Write note.txt"))
        .await
        .unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(8), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "completed");
    assert_eq!(
        fs::read_to_string(root.path().join("project/note.txt")).unwrap(),
        "alpha"
    );
    let reservation = engine
        .reserve_workspace(&root.path().join("project"))
        .unwrap();
    assert!(engine.rewind_job(&job.id).is_err());
    drop(reservation);
    let restored = engine.rewind_job(&job.id).unwrap();
    assert_eq!(restored["ok"], true);
    assert!(
        !root.path().join("project/note.txt").exists()
            || fs::read_to_string(root.path().join("project/note.txt")).is_err()
            || !fs::read_to_string(root.path().join("project/note.txt"))
                .unwrap()
                .contains("alpha")
            || restored["restored"]
                .as_array()
                .is_some_and(|v| !v.is_empty())
    );
    // Session messages remain (not wiped).
    assert!(!engine.store().messages(&job.id).unwrap().is_empty());
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn rejected_pause_on_queued_task_does_not_park_it_later() {
    // First task holds the workspace lane long enough for a second task to be
    // queued behind it. Pausing the queued task must be rejected and must not
    // leave a paused flag that parks the task forever once it starts running.
    let server = support::server(|index, _body| {
        let value = match index {
            0 => response(
                "hold",
                json!([tool("h1", "exec", json!({"command":"sleep 1; echo held"}))]),
            ),
            _ => response("done", json!([])),
        };
        (value, Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let first = engine
        .start(request(root.path(), "Hold the workspace"))
        .await
        .unwrap();
    let mut queued_request = request(root.path(), "Second task");
    queued_request.queue = true;
    let second = engine.start(queued_request).await.unwrap();
    assert_eq!(second.status, "queued");
    let error = engine.pause_job(&second.id).unwrap_err().to_string();
    assert!(error.contains("running task"), "{error}");
    assert_eq!(engine.job(&second.id).unwrap().unwrap().status, "queued");
    for id in [&first.id, &second.id] {
        let done = tokio::time::timeout(Duration::from_secs(10), engine.wait(id))
            .await
            .expect("a task with a rejected pause must not hang at the pause boundary")
            .unwrap();
        assert_eq!(done.status, "completed", "{}", done.summary);
    }
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn pause_and_steer_during_final_response_changes_the_answer_before_completion() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        match index {
            // The request is observable before its body is returned, so pause
            // and steering happen during the final model request, not before it.
            0 => (
                response("Original answer without the new instruction.", json!([])),
                Duration::from_secs(1),
            ),
            1 => {
                assert!(
                    body["messages"].as_array().unwrap().iter().any(|message| {
                        message["role"] == "system"
                            && message["content"]
                                .as_str()
                                .is_some_and(|text| text.contains("STEERED_FINAL"))
                    }),
                    "The replacement final request must contain the user's steering"
                );
                (response("STEERED_FINAL", json!([])), Duration::ZERO)
            }
            _ => panic!("Final-response steering must not loop or replay the original request"),
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Give a brief greeting."))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("The original final request must reach the fixture");
    engine.pause_job(&job.id).unwrap();
    engine
        .steer_job(
            &job.id,
            "Replace the answer with exactly STEERED_FINAL.",
            None,
        )
        .unwrap();

    // pause_job's immediate notification is not a durable boundary event.
    // Wait for the worker to park, or observe the pre-fix premature completion.
    let parked = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            if engine
                .store()
                .last_task_event(&job.task_id, "agent.paused")
                .unwrap()
                .is_some()
            {
                break true;
            }
            if !matches!(
                engine.job(&job.id).unwrap().unwrap().status.as_str(),
                "running" | "paused"
            ) {
                break false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    if !matches!(parked, Ok(true)) {
        let status = engine.job(&job.id).unwrap().unwrap().status;
        engine.shutdown().await.unwrap();
        panic!("Final response ignored pause/steering instead of parking: {status}");
    }
    assert_eq!(engine.job(&job.id).unwrap().unwrap().status, "paused");
    assert!(engine
        .store()
        .last_task_event(&job.task_id, "agent.completed")
        .unwrap()
        .is_none());
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    engine.resume_job(&job.id).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(5), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(completed.status, "completed", "{}", completed.summary);
    assert_eq!(completed.summary, "STEERED_FINAL");
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    let tape = engine.store().messages(&job.id).unwrap();
    context::validate_pairs(&tape).unwrap();
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "agent.completed" && event["task_id"] == job.task_id)
            .count(),
        1,
        "Steering must result in one durable completion for this task"
    );
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn quick_pause_steer_resume_during_final_request_does_not_drop_the_instruction() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        match index {
            0 => (
                response("Original answer before quick steering.", json!([])),
                Duration::from_secs(1),
            ),
            1 => {
                assert!(
                    body["messages"].as_array().unwrap().iter().any(|message| {
                        message["role"] == "system"
                            && message["content"]
                                .as_str()
                                .is_some_and(|text| text.contains("QUICK_STEERED_FINAL"))
                    }),
                    "A resume before the worker parks must still deliver the instruction"
                );
                (response("QUICK_STEERED_FINAL", json!([])), Duration::ZERO)
            }
            _ => panic!("Quick steering must cause exactly one replacement final request"),
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Give a brief greeting."))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("The original final request must reach the fixture");
    // There is no await between these operations. The task cannot reach the
    // pause boundary on this current-thread runtime before resume is recorded.
    engine.pause_job(&job.id).unwrap();
    engine
        .steer_job(
            &job.id,
            "Replace the answer with exactly QUICK_STEERED_FINAL.",
            None,
        )
        .unwrap();
    engine.resume_job(&job.id).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(5), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    let tape = engine.store().messages(&job.id).unwrap();
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(completed.status, "completed", "{}", completed.summary);
    assert_eq!(completed.summary, "QUICK_STEERED_FINAL");
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    context::validate_pairs(&tape).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "agent.steered" && event["task_id"] == job.task_id)
            .count(),
        1,
        "Quick steering must be applied and recorded exactly once"
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "agent.completed" && event["task_id"] == job.task_id)
            .count(),
        1
    );
}

#[tokio::test]
async fn steering_discards_an_inflight_edit_proposal_before_it_can_change_files() {
    let server = support::server(|index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        match index {
            0 => (
                response(
                    "Inspecting the current note.",
                    json!([tool("read-note", "read_file", json!({"path":"note.txt"}))]),
                ),
                Duration::ZERO,
            ),
            1 => (
                response(
                    "Updating the note.",
                    json!([tool(
                        "stale-edit",
                        "edit_file",
                        json!({"path":"note.txt","old_string":"original","new_string":"changed"})
                    )]),
                ),
                Duration::from_secs(1),
            ),
            2 => {
                assert!(body["messages"].as_array().unwrap().iter().any(|message| {
                    message["role"] == "system"
                        && message["content"]
                            .as_str()
                            .is_some_and(|text| text.contains("DO_NOT_EDIT"))
                }));
                (
                    response("The note remains unchanged.", json!([])),
                    Duration::ZERO,
                )
            }
            _ => panic!("The discarded edit must be reconsidered only once"),
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let note = root.path().join("project/note.txt");
    fs::write(&note, "original\n").unwrap();
    let job = engine
        .start(request(
            root.path(),
            "Change note.txt from original to changed.",
        ))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while server.requests.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("The edit proposal request must reach the fixture");
    engine.pause_job(&job.id).unwrap();
    engine
        .steer_job(
            &job.id,
            "DO_NOT_EDIT: do not edit note.txt; leave its original contents unchanged.",
            None,
        )
        .unwrap();
    tokio::time::timeout(Duration::from_secs(4), async {
        while engine
            .store()
            .last_task_event(&job.task_id, "agent.paused")
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("The pending edit proposal must reach the pause boundary");
    assert_eq!(fs::read_to_string(&note).unwrap(), "original\n");
    engine.resume_job(&job.id).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(5), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    let tape = engine.store().messages(&job.id).unwrap();
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(
        fs::read_to_string(&note).unwrap(),
        "original\n",
        "An edit proposed before steering must not execute after the user says do not edit; task {}: {}",
        completed.status,
        completed.summary
    );
    assert_eq!(completed.status, "completed", "{}", completed.summary);
    context::validate_pairs(&tape).unwrap();
    let results = tape
        .iter()
        .filter(|message| message["role"] == "tool" && message["tool_call_id"] == "stale-edit")
        .collect::<Vec<_>>();
    assert_eq!(
        results.len(),
        1,
        "The discarded proposal still needs one paired result"
    );
    let result: Value = serde_json::from_str(results[0]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        result["success"], false,
        "A discarded edit must not claim success"
    );
    assert!(!events.iter().any(|event| {
        event["type"] == "tool.started" && event["payload"]["tool"] == "edit_file"
    }));
    assert_eq!(server.requests.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn steering_during_completion_hook_changes_the_answer_before_completion() {
    assert_steering_during_completion_hook(false).await;
}

#[tokio::test]
async fn steering_during_failed_completion_hook_precedes_retry_exhaustion() {
    assert_steering_during_completion_hook(true).await;
}

async fn assert_steering_during_completion_hook(fail_first_check: bool) {
    let server = support::server(move |index, body| {
        context::validate_pairs(body["messages"].as_array().unwrap()).unwrap();
        let answer = match index {
            0 => "Original answer before the completion check.",
            1 => {
                let messages = body["messages"].as_array().unwrap();
                let steering = messages
                    .iter()
                    .position(|message| {
                        message["role"] == "system"
                            && message["content"]
                                .as_str()
                                .is_some_and(|text| text.contains("HOOK_STEERED_FINAL"))
                    })
                    .expect("The revised request must contain the user's steering");
                if fail_first_check {
                    let failure = messages
                        .iter()
                        .position(|message| {
                            message["role"] == "system"
                                && message["content"].as_str().is_some_and(|text| {
                                    text.contains("FIRST_COMPLETION_CHECK_FAILED")
                                })
                        })
                        .expect("The revised request must retain the failed check's evidence");
                    assert!(
                        failure < steering,
                        "The newer user instruction must follow the earlier check failure"
                    );
                }
                "HOOK_STEERED_FINAL"
            }
            _ => panic!("Steering during the check must request exactly one revised answer"),
        };
        (response(answer, json!([])), Duration::ZERO)
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let project = root.path().join("project");
    let hook = ".shadowcode/hooks/completion.yaml";
    let command = if fail_first_check {
        "if test -e completion-hook.started; then exit 0; fi; printf started > completion-hook.started; sleep 1; printf FIRST_COMPLETION_CHECK_FAILED >&2; exit 7"
    } else {
        "printf started > completion-hook.started; sleep 1"
    };
    fs::create_dir_all(project.join(".shadowcode/hooks")).unwrap();
    fs::write(
        project.join(hook),
        serde_json::to_vec(&json!({
            "name":"completion", "events":["on_complete"],
            "command":command, "timeout_sec":3,
        }))
        .unwrap(),
    )
    .unwrap();
    let workspace = Workspace::open(&project).unwrap();
    let hash = workspace.read(hook).unwrap().hash;
    let mut config = Config::load(engine.paths(), None).unwrap();
    shadowcode_core::hooks::activate(&workspace, &mut config, hook, &hash, true).unwrap();
    Config::patch(engine.paths(), json!({"hooks":config.hooks})).unwrap();
    if fail_first_check {
        Config::patch(engine.paths(), json!({"agent":{"max_fix_retries":0}})).unwrap();
    }
    let job = engine
        .start(request(root.path(), "Give a brief greeting."))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while !project.join("completion-hook.started").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("The configured completion check must actually start");
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    // The hook is still running. No await between these calls lets resume
    // arrive before a later worker boundary without dropping the instruction.
    engine.pause_job(&job.id).unwrap();
    engine
        .steer_job(
            &job.id,
            "Replace the answer with exactly HOOK_STEERED_FINAL.",
            None,
        )
        .unwrap();
    engine.resume_job(&job.id).unwrap();
    let completed = tokio::time::timeout(Duration::from_secs(6), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    let tape = engine.store().messages(&job.id).unwrap();
    let events = engine.store().recent_events(&job.session_id, 200).unwrap();
    engine.shutdown().await.unwrap();
    assert_eq!(completed.status, "completed", "{}", completed.summary);
    assert_eq!(
        completed.summary, "HOOK_STEERED_FINAL",
        "Steering accepted during completion checks must be applied before final completion"
    );
    context::validate_pairs(&tape).unwrap();
    assert_eq!(server.requests.lock().unwrap().len(), 2);
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "agent.steered" && event["task_id"] == job.task_id)
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event["type"] == "agent.completed" && event["task_id"] == job.task_id)
            .count(),
        1
    );
}
