//! RUN-02: the engine fails inside a job before the model streams. The
//! conversation must end with one clear, durable failure the window can
//! finish on (no endless spinner), and the same engine must run the next task.
//! Faults are armed through a debug-build-only hook.
#![cfg(debug_assertions)]
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::{Engine, Fault, Job, StartRequest},
    paths::AppPaths,
};
use std::{fs, path::Path, time::Duration};

fn answer(text: &str) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
}

fn setup(endpoint: &str) -> (tempfile::TempDir, Engine) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("project")).unwrap();
    fs::write(root.path().join("project/notes.txt"), "keep me\n").unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY","context_limit":16384},"trusted_workspaces":[root.path().join("project")],"permissions":{"approve_shell":false},"agent":{"max_steps":6}})).unwrap();
    (root, Engine::open(paths).unwrap())
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
    tokio::time::timeout(Duration::from_secs(10), engine.wait(id))
        .await
        .expect("the failed job never finished: the conversation would spin forever")
        .expect("the finished job must stay readable")
}

/// The window's job stream ends only when the saved job is inactive and its
/// terminal event is durable at or before the job's cursor (ui/src/lib/jobEvents.ts).
fn assert_conversation_ends_with_one_failure(engine: &Engine, job: &Job) {
    let saved = engine.job(&job.id).unwrap().unwrap();
    assert_eq!(saved.status, "failed", "{saved:?}");
    let result = saved.result.as_ref().expect("a failed job keeps a result");
    assert_eq!(result["success"], false);
    let summary = saved.summary.as_str();
    assert!(
        summary.contains("internal error") && summary.contains("send the message again"),
        "the failure must say what happened and what to do, in plain words: {summary}"
    );
    assert!(
        !summary.to_ascii_lowercase().contains("panic") && !summary.contains("poison"),
        "no internal jargon in the conversation: {summary}"
    );
    let events = engine
        .store()
        .recent_events(&saved.session_id, 500)
        .unwrap();
    let terminal: Vec<_> = events
        .iter()
        .filter(|e| e["type"] == "agent.completed" && e["task_id"] == saved.task_id)
        .collect();
    assert_eq!(terminal.len(), 1, "exactly one durable terminal event");
    assert_eq!(
        terminal[0]["id"].as_i64().unwrap(),
        saved.event_cursor,
        "the job's cursor points at its terminal event, so the window's stream closes"
    );
    assert_eq!(terminal[0]["payload"]["success"], false);
    assert!(
        events
            .iter()
            .filter(|e| e["task_id"] == saved.task_id)
            .all(|e| e["id"].as_i64().unwrap() <= saved.event_cursor),
        "no row of the task lies beyond its terminal cursor"
    );
    assert!(
        !events.iter().any(|e| e["task_id"] == saved.task_id
            && matches!(e["type"].as_str(), Some("model.stream" | "model.delta"))),
        "the failure happened before any model output"
    );
    let task = engine.store().task(&saved.task_id).unwrap().unwrap();
    assert_eq!(
        task["status"], "failed",
        "the task row is final too: {task}"
    );
}

async fn fails_before_streaming_then_the_engine_keeps_working(fault: Fault) {
    let server = support::server(|_, _| (answer("second task done"), Duration::ZERO)).await;
    let (root, engine) = setup(&server.endpoint);
    engine.inject_fault(fault);
    let first = engine
        .start(request(root.path(), "summarize the notes", None))
        .await
        .unwrap();
    let failed = wait(&engine, &first.id).await;
    assert_eq!(failed.status, "failed", "{fault:?}: {failed:?}");
    assert_conversation_ends_with_one_failure(&engine, &first);
    assert!(
        server.requests.lock().unwrap().is_empty(),
        "{fault:?}: no model request was sent before the failure"
    );
    assert_eq!(
        fs::read_to_string(root.path().join("project/notes.txt")).unwrap(),
        "keep me\n",
        "the project is unchanged"
    );

    // The same engine and conversation take the next task.
    let next = engine
        .start(request(
            root.path(),
            "summarize the notes again",
            Some(first.session_id.clone()),
        ))
        .await
        .unwrap();
    let done = wait(&engine, &next.id).await;
    assert_eq!(done.status, "completed", "{fault:?}: {done:?}");
    assert_eq!(server.requests.lock().unwrap().len(), 1);

    // A restart sees the same final state, still with one terminal event.
    let paths = engine.paths().clone();
    engine.shutdown().await.unwrap();
    drop(engine);
    let reopened = Engine::open(paths).unwrap();
    assert_conversation_ends_with_one_failure(&reopened, &first);
    assert_eq!(reopened.job(&next.id).unwrap().unwrap().status, "completed");
    reopened.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panic_before_the_model_stream_ends_the_conversation_and_the_engine_continues() {
    fails_before_streaming_then_the_engine_keeps_working(Fault::PanicBeforeStream).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panic_inside_a_database_write_does_not_poison_the_store_for_later_tasks() {
    fails_before_streaming_then_the_engine_keeps_working(Fault::PanicInStoreWrite).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn panic_holding_the_job_record_still_saves_the_failure_and_frees_the_queue() {
    fails_before_streaming_then_the_engine_keeps_working(Fault::PanicHoldingJobRecord).await;
}
