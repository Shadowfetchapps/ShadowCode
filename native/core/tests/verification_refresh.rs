use serde_json::{json, Value};
use shadowcode_core::{
    engine::Job,
    paths::AppPaths,
    service::{Request, Service},
};

async fn refresh(service: &Service, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: "POST".into(),
            path: "/api/jobs/verification-refresh".into(),
            body,
        })
        .await
}

#[tokio::test]
async fn bounded_refresh_route_preserves_identity_and_rejects_invalid_batches() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    let service = Service::open(
        AppPaths::isolated(&root.path().join("profile")).unwrap(),
        Some(project.clone()),
    )
    .unwrap();
    let mut ids = Vec::new();
    for index in 0..32 {
        let job = Job {
            id: format!("receipt-{index}"),
            task_id: format!("task-{index}"),
            session_id: format!("session-{index}"),
            workspace: project.clone(),
            status: "completed".into(),
            result: Some(json!({"verification":{"status":"not_run","commands":[]}})),
            ..Default::default()
        };
        service
            .engine
            .store()
            .save_job(&serde_json::to_value(&job).unwrap())
            .unwrap();
        ids.push(job.id);
    }
    let result = refresh(&service, json!({"job_ids":ids})).await.unwrap();
    let rows = result["verifications"].as_object().unwrap();
    assert_eq!(rows.len(), 32);
    for id in &ids {
        assert_eq!(rows[id]["status"], "not_run");
        let individual = service
            .dispatch(Request {
                method: "GET".into(),
                path: format!("/api/jobs/{id}/verification"),
                body: Value::Null,
            })
            .await
            .unwrap();
        assert_eq!(rows[id]["commands"], individual["commands"]);
        assert_eq!(rows[id]["verified"], individual["verified"]);
    }
    let mut oversized = ids.clone();
    oversized.push("receipt-32".into());
    for body in [
        Value::Null,
        json!({"job_ids":[]}),
        json!({"job_ids":[""]}),
        json!({"job_ids":["receipt-0","receipt-0"]}),
        json!({"job_ids":["receipt-0","missing"]}),
        json!({"job_ids":["x".repeat(129)]}),
        json!({"job_ids":oversized}),
        json!({"job_ids":[7]}),
        json!({"job_ids":["receipt-0"],"unexpected":true}),
    ] {
        assert!(refresh(&service, body.clone()).await.is_err(), "{body}");
    }
    // A malformed refresh changes neither recorded job state nor evidence.
    let saved = service.engine.job("receipt-0").unwrap().unwrap();
    assert_eq!(saved.status, "completed");
    assert_eq!(saved.result.unwrap()["verification"]["status"], "not_run");
    service.engine.shutdown().await.unwrap();
}
