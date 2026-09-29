//! What ShadowCode itself adds to the first model request of a task: the
//! system prompt and the tool definitions. Each is capped a little above
//! today's size, so a change that makes every request bigger (and slower and
//! costlier) fails here and is made on purpose.
mod support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{fs, time::Duration};

/// Sizes in bytes of JSON, measured for 1.0.0 (system 2,173; tools 11,522
/// and 12,613; whole request 13,896 and 14,987) plus 10%. Raise a cap only
/// for a change that is worth the extra on every request.
const CAPS: [(&str, u64, usize, usize, usize); 2] = [
    // (model, context limit, system prompt, tool definitions, whole request)
    ("small", 16_384, 2_390, 12_674, 15_286),
    ("large", 131_072, 2_390, 13_874, 16_486),
];

async fn first_request(context: u64) -> Value {
    let server = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"role":"assistant","content":"Hello."},"finish_reason":"stop"}],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}}),
            Duration::ZERO,
        )
    })
    .await;
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("README.md"), "# Demo\n").unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"model":{"provider":"local","endpoint":server.endpoint,"name":"fixture","context_limit":context},"trusted_workspaces":[project]})).unwrap();
    let service = Service::open(paths, Some(project)).unwrap();
    let job = service
        .dispatch(Request {
            method: "POST".into(),
            path: "/api/jobs".into(),
            body: json!({"task":"Say hello"}),
        })
        .await
        .unwrap();
    let id = job["id"].as_str().unwrap().to_owned();
    for _ in 0..400 {
        let done = service.engine.job(&id).unwrap().unwrap();
        if !matches!(done.status.as_str(), "queued" | "running") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    service.engine.shutdown().await.unwrap();
    let first = server.requests.lock().unwrap()[0].clone();
    first
}

#[tokio::test]
async fn the_first_request_stays_small() {
    for (label, context, system_cap, tools_cap, whole_cap) in CAPS {
        let request = first_request(context).await;
        let messages = request["messages"].as_array().unwrap();
        let system: usize = messages
            .iter()
            .filter(|m| m["role"] == "system")
            .map(|m| m["content"].to_string().len())
            .sum();
        let tools = request["tools"].to_string().len();
        let names: Vec<&str> = request["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["function"]["name"].as_str())
            .collect();
        let whole = request.to_string().len();
        eprintln!(
            "{label}: system {system} bytes, tools {tools} bytes ({} tools: {}), whole request {whole} bytes",
            names.len(),
            names.join(", "),
        );
        assert!(
            system <= system_cap,
            "{label}: the system prompt grew to {system} bytes (cap {system_cap})"
        );
        assert!(
            tools <= tools_cap,
            "{label}: the tool definitions grew to {tools} bytes (cap {tools_cap})"
        );
        assert!(
            whole <= whole_cap,
            "{label}: the first request grew to {whole} bytes (cap {whole_cap})"
        );
    }
}
