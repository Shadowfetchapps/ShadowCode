mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::{Config, ModelConfig},
    engine::{Job, StartRequest},
    model_registry,
    paths::AppPaths,
    routing,
    service::{Request, Service},
};
use std::{fs, time::Duration};

fn setup(endpoint: &str) -> (tempfile::TempDir, Service) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"default":"primary","name":"shared","provider":"local","endpoint":endpoint,"context_limit":8192,"api_key_env":"PRIMARY_MODEL_KEY"},"trusted_workspaces":[project],"permissions":{"approve_shell":false},"agent":{"retry_attempts":0}})).unwrap();
    (root, Service::open(paths, Some(project)).unwrap())
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
fn response(text: &str, calls: Value) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text,"tool_calls":calls},"finish_reason":if calls.as_array().is_some_and(|v|!v.is_empty()){"tool_calls"}else{"stop"}}],"usage":{"prompt_tokens":20,"completion_tokens":10,"total_tokens":30}})
}
fn request(service: &Service, mode: &str) -> StartRequest {
    StartRequest {
        workspace: service.workspace().unwrap(),
        task: "Explain your approach".into(),
        session_id: None,
        model: None,
        mode: mode.into(),
        queue: true,
        images: Vec::new(),
        web: false,
    }
}
async fn wait(service: &Service, id: &str) -> Job {
    tokio::time::timeout(Duration::from_secs(8), service.engine.wait(id))
        .await
        .unwrap()
        .unwrap()
}
async fn register(service: &Service, id: &str, endpoint: &str) -> ModelConfig {
    serde_json::from_value(call(service,"POST","/api/models/register",json!({"id":id,"name":"shared","provider":"local","endpoint":endpoint,"api_key_env":"SECONDARY_MODEL_KEY","context_limit":4096})).await.unwrap()["model"].clone()).unwrap()
}

#[tokio::test]
async fn endpoint_changes_do_not_inherit_custom_keys_and_invalid_settings_do_not_write_secrets() {
    let (_root, service) = setup("http://127.0.0.1:9001/v1");
    let same = call(
        &service,
        "POST",
        "/api/models/register",
        json!({"provider":"local","name":"another","endpoint":"http://127.0.0.1:9001/v1"}),
    )
    .await
    .unwrap();
    assert_eq!(same["model"]["api_key_env"], "PRIMARY_MODEL_KEY");
    let different = call(
        &service,
        "POST",
        "/api/models/register",
        json!({"provider":"local","name":"another","endpoint":"http://127.0.0.1:9002/v1"}),
    )
    .await
    .unwrap();
    assert_eq!(different["model"]["api_key_env"], "OPENAI_API_KEY");
    assert!(call(&service,"PUT","/api/config",json!({"values":{"routing":{"enabled":"invalid"}},"api_key_env":"PRIMARY_MODEL_KEY","api_key":"must-not-be-written"})).await.is_err());
    assert!(shadowcode_core::config::secrets(service.engine.paths())
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn provider_failure_does_not_silently_retry_against_the_default_endpoint() {
    let primary =
        support::server(|_, _| (response("Must not be called", json!([])), Duration::ZERO)).await;
    let (_root, service) = setup(&primary.endpoint);
    register(&service, "unreachable", "http://127.0.0.1:1/v1").await;
    call(
        &service,
        "PUT",
        "/api/routing",
        json!({"values":{"enabled":true,"coder":"unreachable"}}),
    )
    .await
    .unwrap();
    let job = service
        .engine
        .start(request(&service, "code"))
        .await
        .unwrap();
    let result = wait(&service, &job.id).await;
    assert_eq!(result.status, "failed");
    assert_eq!(result.routing.unwrap().source, "purpose");
    assert!(primary.requests.lock().unwrap().is_empty());
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn discovery_scopes_names_and_preserves_configured_credentials_and_context() {
    let (_root, service) = setup("http://127.0.0.1:9001/v1");
    let cfg = Config::load(service.engine.paths(), None).unwrap();
    let store = service.engine.store();
    let id = model_registry::model_id("local", "http://127.0.0.1:9001/v1", "shared");
    assert_eq!(
        id,
        model_registry::model_id("local", "http://127.0.0.1:9001/v1/", "shared")
    );
    let selected = register(&service, &id, "http://127.0.0.1:9001/v1").await;
    let detected = [
        json!({"provider":"local","endpoint":"http://127.0.0.1:9001/v1","models":[{"id":"shared","context_limit":128000,"capabilities":{"tools":true}}]}),
        json!({"provider":"local","endpoint":"http://127.0.0.1:9002/v1","models":[{"id":"shared"}]}),
        json!({"provider":"ollama","endpoint":"http://127.0.0.1:11434","models":[{"id":"shared"}]}),
    ];
    model_registry::record_detected(&store, &detected).unwrap();
    model_registry::record_detected(&store, &detected).unwrap();
    let resolved = model_registry::resolve(&store, &id, &cfg.model).unwrap();
    assert_eq!(resolved.api_key_env, selected.api_key_env);
    assert_eq!(resolved.context_limit, 4096);
    assert_eq!(resolved.endpoint, selected.endpoint);
    assert!(model_registry::resolve(&store, "shared", &cfg.model)
        .unwrap_err()
        .to_string()
        .contains("ambiguous"));
    let rows = model_registry::catalog(&store, &cfg.model).unwrap();
    assert_eq!(rows.len(), 4); // primary and custom credentials are distinct choices
    assert_eq!(rows.iter().filter(|r| r["provider"] == "ollama").count(), 1);
    assert_eq!(
        rows.iter().find(|r| r["id"] == id).unwrap()["metadata"]["capabilities"]["tools"],
        true
    );
    assert_eq!(
        model_registry::resolve(&store, "primary", &cfg.model)
            .unwrap()
            .api_key_env,
        "PRIMARY_MODEL_KEY"
    );
}

#[tokio::test]
async fn unchanged_legacy_aliases_stay_resolvable_without_duplicate_picker_entries() {
    let (_root, service) = setup("http://127.0.0.1:9001/v1");
    call(&service, "GET", "/api/routing", Value::Null)
        .await
        .unwrap();
    call(
        &service,
        "PUT",
        "/api/config",
        json!({"values":{"model":{"default":"shared"}}}),
    )
    .await
    .unwrap();
    let cfg = Config::load(service.engine.paths(), None).unwrap();
    let store = service.engine.store();
    assert!(cfg.model.default.starts_with("model:"));
    let catalog = model_registry::catalog(&store, &cfg.model).unwrap();
    assert_eq!(catalog.len(), 1);
    assert_eq!(catalog[0]["id"], cfg.model.default);
    assert_eq!(
        model_registry::resolve(&store, "primary", &cfg.model)
            .unwrap()
            .api_key_env,
        cfg.model.api_key_env
    );
}

#[tokio::test]
async fn registration_rejects_alias_retargeting_and_keeps_the_old_default_available() {
    let (_root, service) = setup("http://127.0.0.1:9001/v1");
    register(&service, "alternate", "http://127.0.0.1:9002/v1").await;
    let error=call(&service,"POST","/api/models/register",json!({"id":"alternate","name":"shared","provider":"local","endpoint":"http://127.0.0.1:9003/v1"})).await.unwrap_err();
    assert!(error.to_string().contains("unique ID"));
    call(&service,"PUT","/api/config",json!({"values":{"model":{"default":"shared","name":"shared","provider":"local","endpoint":"http://127.0.0.1:9004/v1"}}})).await.unwrap();
    let cfg = Config::load(service.engine.paths(), None).unwrap();
    let old = model_registry::resolve(&service.engine.store(), "primary", &cfg.model).unwrap();
    assert_eq!(old.endpoint, "http://127.0.0.1:9001/v1");
    assert_eq!(old.api_key_env, "PRIMARY_MODEL_KEY");
    assert_ne!(cfg.model.default, "shared");
    assert_eq!(
        model_registry::resolve(&service.engine.store(), "alternate", &cfg.model)
            .unwrap()
            .endpoint,
        "http://127.0.0.1:9002/v1"
    );
}

#[tokio::test]
async fn invalid_routes_are_rejected_and_missing_saved_models_report_the_default_fallback() {
    let (_root, service) = setup("http://127.0.0.1:9001/v1");
    for values in [
        json!({"enabled":"yes"}),
        json!({"typo":"primary"}),
        json!({"coder":42}),
        json!({"coder":"missing"}),
    ] {
        assert!(
            call(&service, "PUT", "/api/routing", json!({"values":values}))
                .await
                .is_err()
        );
    }
    Config::patch(
        service.engine.paths(),
        json!({"routing":{"enabled":true,"coder":"missing"}}),
    )
    .unwrap();
    let view = call(&service, "GET", "/api/routing", Value::Null)
        .await
        .unwrap();
    assert_eq!(view["decisions"]["coder"]["source"], "fallback");
    assert_eq!(view["table"]["coder"], "primary");
    assert!(view["decisions"]["coder"]["fallback_reason"]
        .as_str()
        .unwrap()
        .contains("not registered"));
    assert_eq!(view["decisions"]["planner"]["source"], "default");
    call(
        &service,
        "PUT",
        "/api/routing",
        json!({"values":{"coder":"primary"}}),
    )
    .await
    .unwrap();
    let cfg = Config::load(service.engine.paths(), None).unwrap();
    assert_eq!(
        routing::select(&service.engine.store(), &cfg, None, "coder")
            .unwrap()
            .1
            .source,
        "purpose"
    );
}

#[tokio::test]
async fn queued_routes_are_frozen_explicit_selection_wins_and_plan_permissions_stay_read_only() {
    let primary = support::server(|_, _| {
        (
            response("Default provider answered", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    let secondary = support::server(|_, body| {
        assert_eq!(body["model"], "shared");
        let names: Vec<_> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        assert!(names.contains(&"read_file"));
        assert!(!names.contains(&"write_file"));
        assert!(!names.contains(&"exec"));
        (
            response("Planner provider answered", json!([])),
            Duration::from_millis(120),
        )
    })
    .await;
    let (_root, service) = setup(&primary.endpoint);
    let alternate = register(&service, "planner-model", &secondary.endpoint).await;
    call(
        &service,
        "PUT",
        "/api/routing",
        json!({"values":{"enabled":true,"planner":"planner-model","coder":"planner-model"}}),
    )
    .await
    .unwrap();
    let first = service
        .engine
        .start(request(&service, "plan"))
        .await
        .unwrap();
    let second = service
        .engine
        .start(request(&service, "plan"))
        .await
        .unwrap();
    call(
        &service,
        "PUT",
        "/api/routing",
        json!({"values":{"planner":"primary"}}),
    )
    .await
    .unwrap();
    assert_eq!(wait(&service, &first.id).await.status, "completed");
    let completed = wait(&service, &second.id).await;
    assert_eq!(completed.status, "completed");
    assert_eq!(
        completed.routing.as_ref().unwrap().model_id,
        alternate.default
    );
    let third = service
        .engine
        .start(request(&service, "plan"))
        .await
        .unwrap();
    assert_eq!(
        wait(&service, &third.id).await.routing.unwrap().model_id,
        "primary"
    );
    let mut explicit = request(&service, "code");
    explicit.model = Some(Config::load(service.engine.paths(), None).unwrap().model);
    let job = service.engine.start(explicit).await.unwrap();
    assert_eq!(
        wait(&service, &job.id).await.routing.unwrap().source,
        "explicit"
    );
    assert_eq!(secondary.requests.lock().unwrap().len(), 2);
    assert_eq!(primary.requests.lock().unwrap().len(), 2);
    let events = service
        .engine
        .store()
        .events_after(&second.session_id, 0, None, 100)
        .unwrap();
    let decision = events
        .iter()
        .find(|e| e["type"] == "routing.selected")
        .unwrap();
    assert_eq!(decision["payload"]["model_id"], "planner-model");
    assert!(!decision.to_string().contains("SECONDARY_MODEL_KEY"));
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn missing_saved_route_emits_a_persisted_fallback_event() {
    let server =
        support::server(|_, _| (response("Default answered", json!([])), Duration::ZERO)).await;
    let (_root, service) = setup(&server.endpoint);
    Config::patch(
        service.engine.paths(),
        json!({"routing":{"enabled":true,"coder":"removed-model"}}),
    )
    .unwrap();
    let job = service
        .engine
        .start(request(&service, "code"))
        .await
        .unwrap();
    assert_eq!(wait(&service, &job.id).await.status, "completed");
    let events = service
        .engine
        .store()
        .events_after(&job.session_id, 0, None, 100)
        .unwrap();
    let event = events
        .iter()
        .find(|e| e["type"] == "routing.fallback")
        .unwrap();
    assert_eq!(event["payload"]["requested"], "removed-model");
    assert_eq!(event["payload"]["model_name"], "shared");
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn goal_verification_uses_the_tester_route_and_executes_its_acceptance_command() {
    goal_verification_on_tester_route(true).await;
}

#[tokio::test]
async fn goal_verification_rejects_unconfigured_tester_commands_despite_success_prose() {
    goal_verification_on_tester_route(false).await;
}

async fn goal_verification_on_tester_route(configured: bool) {
    const CHECK: &str = "test \"$(cat acceptance.txt)\" = verified";
    let primary = support::server(|_, _| {
        (
            response("Implementation milestone complete", json!([])),
            Duration::ZERO,
        )
    })
    .await;
    let tester = support::server(|index, _| {
        let value = if index == 0 {
            response(
                "Running acceptance",
                json!([
                    {"id":"acceptance-write","type":"function","function":{"name":"exec","arguments":json!({"command":"printf verified > acceptance.txt"}).to_string()}},
                    {"id":"acceptance-check","type":"function","function":{"name":"exec","arguments":json!({"command":CHECK}).to_string()}}
                ]),
            )
        } else {
            response("Acceptance passed", json!([]))
        };
        (value, Duration::ZERO)
    })
    .await;
    let (_root, service) = setup(&primary.endpoint);
    if configured {
        // A successful write is process evidence, not a configured check.
        // Keep acceptance read-only so its workspace fingerprint stays valid.
        Config::patch(
            service.engine.paths(),
            json!({"verification":{"commands":[CHECK]}}),
        )
        .unwrap();
    }
    register(&service, "tester-model", &tester.endpoint).await;
    call(
        &service,
        "PUT",
        "/api/routing",
        json!({"values":{"enabled":true,"tester":"tester-model"}}),
    )
    .await
    .unwrap();
    let goal=call(&service,"POST","/api/goals",json!({"instruction":"Explain the result, then run acceptance","milestones":[{"title":"Explain","mode":"code"},{"title":"Accept","mode":"code","require_verification":true}],"run":true})).await.unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(8),
        service.engine.wait_goal(goal["id"].as_str().unwrap()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        result["status"],
        if configured { "completed" } else { "blocked" },
        "{result}"
    );
    if !configured {
        assert!(result["run_detail"]
            .as_str()
            .unwrap()
            .contains("verification is not_run"));
    }
    assert_eq!(
        fs::read_to_string(service.workspace().unwrap().join("acceptance.txt")).unwrap(),
        "verified"
    );
    let jobs = service.engine.store().jobs(10).unwrap();
    let tester_jobs: Vec<_> = jobs
        .iter()
        .filter(|j| {
            j["routing"]["purpose"] == "tester" && j["routing"]["model_id"] == "tester-model"
        })
        .collect();
    assert_eq!(tester_jobs.len(), 1);
    let tester_job = tester_jobs[0];
    let verification = &tester_job["result"]["verification"];
    assert_eq!(
        verification["status"],
        if configured { "passed" } else { "not_run" }
    );
    assert_eq!(verification["verified"], configured);
    let commands = verification["commands"].as_array().unwrap();
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0]["kind"], "command");
    let check = commands
        .iter()
        .find(|receipt| receipt["command"] == CHECK)
        .unwrap();
    assert_eq!(
        check["kind"],
        if configured {
            "configured_check"
        } else {
            "command"
        }
    );
    for receipt in commands {
        assert_eq!(receipt["state"], "passed");
        assert_eq!(receipt["exit_code"], 0);
        assert_eq!(receipt["provenance"], "locally_observed");
        assert_eq!(receipt["task_id"], tester_job["task_id"]);
    }
    assert_eq!(primary.requests.lock().unwrap().len(), 1);
    assert_eq!(tester.requests.lock().unwrap().len(), 2);
    eprintln!(
        "{}",
        json!({"configured":configured,"goal_status":result["status"],"tester_route":tester_job["routing"]["model_id"],"verification":verification["status"],"observed_commands":commands.len()})
    );
    service.engine.shutdown().await.unwrap();
}
