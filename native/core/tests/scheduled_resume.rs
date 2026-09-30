//! "Resume at <time>" after a plan limit: the continuation is saved, survives
//! a restart, runs on the same model when the scheduler reaches its time,
//! can be cancelled, and a time missed by far is reported instead of run.
//! The limited task is written directly (a real vendor limit needs a signed-in
//! subscription); its model is a fake one on loopback.
#![cfg(unix)]
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::{Engine, Job},
    paths::AppPaths,
    routing::Decision,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

fn setup(endpoint: &str) -> (tempfile::TempDir, AppPaths, PathBuf) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"default":"fixture-model","provider":"local","endpoint":endpoint,"name":"fixture","api_key_env":"SHADOWCODE_TEST_UNUSED_API_KEY","context_limit":32768},
            "trusted_workspaces":[project],
            "permissions":{"approve_shell":false},
            "cli_agents":{"enabled":false},
            "agent":{"model_retries":0},
        }),
    )
    .unwrap();
    (root, paths, project)
}

/// A task that stopped at its plan limit, which resets at `resets_at`.
fn limited(engine: &Engine, project: &Path, resets_at: f64) -> Job {
    let store = engine.store();
    let session = store.create_session(project, "fixture-model", "").unwrap();
    let mut job = Job {
        id: shadowcode_core::id(),
        workspace: project.to_path_buf(),
        session_id: session["id"].as_str().unwrap().to_owned(),
        task_id: shadowcode_core::id(),
        task: "Fix the failing test".into(),
        status: "queued".into(),
        mode: "code".into(),
        model: "Fixture".into(),
        routing: Some(Decision {
            model_id: "fixture-model".into(),
            model_name: "fixture".into(),
            provider: "local".into(),
            ..Default::default()
        }),
        started_at: shadowcode_core::now(),
        ..Default::default()
    };
    store.create_job(&json!(job)).unwrap();
    job.status = "limit_reached".into();
    job.finished_at = Some(shadowcode_core::now());
    job.result = Some(
        json!({"success":false,"limit_reached":{"vendor":"codex","detail":"You've hit your usage limit.","resets_at":resets_at}}),
    );
    store.save_job(&json!(job)).unwrap();
    job
}

fn events(engine: &Engine, session: &str, kind: &str) -> Vec<Value> {
    engine
        .store()
        .recent_events(session, 1000)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == kind)
        .map(|e| e["payload"].clone())
        .collect()
}

#[tokio::test]
async fn a_scheduled_resume_survives_a_restart_and_runs_on_the_same_model() {
    let server = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"role":"assistant","content":"Picked up where it stopped."},"finish_reason":"stop"}],
                "usage":{"prompt_tokens":10,"completion_tokens":5,"total_tokens":15}}),
            Duration::ZERO,
        )
    })
    .await;
    let (_root, paths, project) = setup(&server.endpoint);
    let engine = Engine::open(paths.clone()).unwrap();
    // Whole seconds: a reset time is read back from JSON, which need not
    // round-trip every fractional digit.
    let at = (shadowcode_core::now() + 3600.0).floor();
    let job = limited(&engine, &project, at);
    let resume = engine.schedule_resume(&job.id, false).unwrap();
    assert_eq!(resume.at, at);
    assert_eq!(resume.target, "fixture-model");
    let scheduled = &events(&engine, &job.session_id, "resume.scheduled")[0];
    assert_eq!(scheduled["at"], at);
    // The window's "Review and resume" keeps the task's mode and web access.
    assert_eq!(scheduled["mode"], "code");
    assert_eq!(scheduled["web"], false);
    // Before its time, a tick starts nothing.
    engine.automation_tick(at - 60.0).await.unwrap();
    assert!(events(&engine, &job.session_id, "resume.started").is_empty());
    // It is saved: a restarted engine still has it.
    engine.shutdown().await.unwrap();
    drop(engine);
    let engine = Engine::open(paths).unwrap();
    let saved = shadowcode_core::resume::for_session(&engine.store(), &job.session_id)
        .unwrap()
        .unwrap();
    assert_eq!(saved.id, resume.id);
    engine.automation_tick(at + 5.0).await.unwrap();
    let started = events(&engine, &job.session_id, "resume.started");
    assert_eq!(
        started.len(),
        1,
        "{:?}",
        events(&engine, &job.session_id, "resume.failed")
    );
    let follow_up = started[0]["job_id"].as_str().unwrap().to_owned();
    let done = tokio::time::timeout(Duration::from_secs(120), engine.wait(&follow_up))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(done.status, "completed", "{}", done.summary);
    assert_eq!(done.session_id, job.session_id, "same conversation");
    assert!(done.task.starts_with("Continue where Codex stopped"));
    assert_eq!(
        done.routing.unwrap().model_id,
        "fixture-model",
        "same model"
    );
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    // Once run, it is gone.
    engine.automation_tick(at + 60.0).await.unwrap();
    assert_eq!(events(&engine, &job.session_id, "resume.started").len(), 1);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_resume_can_be_cancelled_and_one_missed_by_far_is_reported() {
    let (_root, paths, project) = setup("http://127.0.0.1:9/v1");
    let engine = Engine::open(paths).unwrap();
    let now = shadowcode_core::now();
    // Without a known reset time there is nothing to schedule.
    let unknown = limited(&engine, &project, 0.0);
    assert!(engine.schedule_resume(&unknown.id, false).is_err());
    let cancelled = limited(&engine, &project, now + 600.0);
    engine.schedule_resume(&cancelled.id, false).unwrap();
    let removed = engine.cancel_resume(&cancelled.session_id).unwrap();
    assert!(removed.is_some());
    assert_eq!(
        events(&engine, &cancelled.session_id, "resume.cancelled").len(),
        1
    );
    let missed = limited(&engine, &project, now + 60.0);
    engine.schedule_resume(&missed.id, false).unwrap();
    engine
        .automation_tick(now + 60.0 + 13.0 * 3600.0)
        .await
        .unwrap();
    assert_eq!(
        events(&engine, &missed.session_id, "resume.missed").len(),
        1
    );
    assert!(events(&engine, &missed.session_id, "resume.started").is_empty());
    assert!(events(&engine, &cancelled.session_id, "resume.started").is_empty());
    assert!(shadowcode_core::resume::list(&engine.store())
        .unwrap()
        .is_empty());
    engine.shutdown().await.unwrap();
}
