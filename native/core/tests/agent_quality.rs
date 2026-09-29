//! Agent quality: a task that fails the same way three times pauses and
//! says so; a finished task that skipped a test gets a heads-up.
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::{Engine, StartRequest},
    paths::AppPaths,
};
use std::{fs, path::Path, time::Duration};

fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn calls(calls: Vec<Value>) -> (Value, Duration) {
    (
        json!({"choices":[{"message":{"role":"assistant","content":"","tool_calls":calls},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
        Duration::ZERO,
    )
}
fn answer(text: &str) -> (Value, Duration) {
    (
        json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
        Duration::ZERO,
    )
}
fn setup(endpoint: &str) -> (tempfile::TempDir, Engine) {
    let root = tempfile::tempdir().unwrap();
    fs::create_dir(root.path().join("project")).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"provider":"local","endpoint":endpoint,"name":"fixture","api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY","context_limit":16384},"trusted_workspaces":[root.path().join("project")],"permissions":{"approve_shell":false,"mode":"allow_edits"},"agent":{"max_steps":12}})).unwrap();
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

#[tokio::test(flavor = "multi_thread")]
async fn the_same_failure_three_times_pauses_the_task_and_says_so() {
    let server = support::server(|index, _| {
        if index < 4 {
            calls(vec![tool(
                &format!("c{index}"),
                "exec",
                json!({"command":"sh -c 'echo \"test failed: expected 3\"; exit 3'"}),
            )])
        } else {
            answer("Stopped.")
        }
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    let job = engine
        .start(request(root.path(), "Fix the failing test"))
        .await
        .unwrap();
    let paused = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let current = engine.job(&job.id).unwrap().unwrap();
            if current.status == "paused" {
                break current;
            }
            assert!(current.finished_at.is_none(), "{current:?}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("the task pauses");
    let stuck: Vec<Value> = engine
        .store()
        .recent_events(&paused.session_id, 300)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == "agent.stuck")
        .collect();
    assert_eq!(stuck.len(), 1);
    assert_eq!(stuck[0]["payload"]["kind"], "same_failure");
    assert!(stuck[0]["payload"]["text"]
        .as_str()
        .unwrap()
        .contains("3 times"));
    // Exactly three runs happened before the pause.
    assert_eq!(server.requests.lock().unwrap().len(), 3);
    engine.cancel(&job.id).await.ok();
    engine.shutdown().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_task_that_skips_a_test_gets_a_heads_up() {
    let server = support::server(|index, _| match index {
        0 => calls(vec![tool("r1", "read_file", json!({"path":"tests/math_test.py"}))]),
        1 => calls(vec![tool(
            "w1",
            "write_file",
            json!({"path":"tests/math_test.py","content":"import pytest\n\n@pytest.mark.skip\ndef test_add():\n    assert 1 + 1 == 2\n"}),
        )]),
        _ => answer("Done: the tests pass now."),
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::create_dir_all(root.path().join("project/tests")).unwrap();
    fs::write(
        root.path().join("project/tests/math_test.py"),
        "def test_add():\n    assert 1 + 1 == 2\n",
    )
    .unwrap();
    let job = engine
        .start(request(root.path(), "Make the tests pass"))
        .await
        .unwrap();
    let done = tokio::time::timeout(Duration::from_secs(30), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    let honesty = &done.result.as_ref().unwrap()["honesty"];
    assert_eq!(honesty["count"], 1, "{done:?}");
    assert_eq!(honesty["flags"][0]["kind"], "skipped_tests");
    assert_eq!(honesty["flags"][0]["line"], 3);
    assert!(honesty["text"]
        .as_str()
        .unwrap()
        .starts_with("Heads up: this task skipped"));
    let events = engine.store().recent_events(&done.session_id, 300).unwrap();
    assert!(events.iter().any(|e| e["type"] == "task.flags"));
    engine.shutdown().await.ok();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tool_call_written_as_text_by_a_local_model_still_runs() {
    let server = support::server(|index, _| match index {
        0 => answer("Let me read it.\n<tool_call>\n{\"name\": \"read_file\", \"arguments\": {\"path\": \"notes.txt\",}}\n</tool_call>"),
        _ => answer("The notes say hello."),
    })
    .await;
    let (root, engine) = setup(&server.endpoint);
    fs::write(root.path().join("project/notes.txt"), "hello\n").unwrap();
    let job = engine
        .start(request(root.path(), "What do the notes say?"))
        .await
        .unwrap();
    let done = tokio::time::timeout(Duration::from_secs(30), engine.wait(&job.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "completed", "{done:?}");
    // The second request carries the file the repaired call read.
    let requests = server.requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 2);
    assert!(requests[1]["messages"].to_string().contains("hello"));
    let events = engine.store().recent_events(&done.session_id, 300).unwrap();
    let repaired: Vec<&Value> = events
        .iter()
        .filter(|e| e["type"] == "tool_call.repaired")
        .collect();
    assert_eq!(repaired.len(), 1);
    assert_eq!(repaired[0]["payload"]["from"], "text");
    engine.shutdown().await.ok();
}
