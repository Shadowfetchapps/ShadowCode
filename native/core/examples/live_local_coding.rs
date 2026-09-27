//! Opt-in coding evaluation with explicit installed GGUF files. No downloads,
//! user project changes, account access or network tools. Run under a loopback-
//! only network namespace to enforce offline inference. Reports failures too.
//! Usage: live_local_coding <report.json> <model.gguf> [model.gguf ...]
use anyhow::{ensure, Result};
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    engine::Job,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{fs, time::Duration};

#[path = "support/coding_fixture.rs"]
mod coding_fixture;
use coding_fixture::{assertions_passed, check, CHECK_COMMAND, SOURCE, TASK, TESTS};

async fn evaluate(file: &str, server: &str) -> Result<Value> {
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    fs::create_dir(&project)?;
    fs::write(project.join("helpers.py"), SOURCE)?;
    fs::write(project.join("test_helpers.py"), TESTS)?;
    let baseline = check(&project).await?;
    ensure!(!baseline.ok && baseline.stdout.contains("SHADOWCODE_EVALUATOR:FAIL") && baseline.stderr.contains("Ran 5 tests"), "The evaluator must actually run and fail all five-case fixture tests before model execution: {}", baseline.stderr);
    let paths = AppPaths::isolated(&root.path().join("profile"))?;
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"unused","context_limit":8192},
            "local_engine":{"llama_binary":server,"files":[file],"context_size":8192},
        "network":{"mode":"offline"},"cli_agents":{"enabled":false},
        "trusted_workspaces":[project],
        "sandbox":{"require":true,"home_binds":[]},
            "agent":{"max_steps":14,"max_task_tokens":50000,"tool_timeout_sec":30}
        }),
    )?;
    let service = Service::open(paths, Some(project.clone()))?;
    let catalog = service
        .dispatch(Request {
            method: "GET".into(),
            path: "/api/local-models".into(),
            body: Value::Null,
        })
        .await?;
    let model = catalog["models"]
        .as_array()
        .and_then(|m| m.iter().find(|m| m["path"] == file))
        .ok_or_else(|| anyhow::anyhow!("Explicit model missing from catalog"))?;
    let id = model["id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("Model ID missing"))?;
    let controller = service.engine.clone();
    let approved_project = project.clone();
    let approvals = tokio::spawn(async move {
        loop {
            for approval in controller.approvals().list(None) {
                let allow = approval.tool == "exec"
                    && approval.arguments["command"] == CHECK_COMMAND
                    && (matches!(approval.arguments["cwd"].as_str(), None | Some("" | "."))
                        || approval.arguments["cwd"].as_str() == approved_project.to_str());
                let _ = controller
                    .approvals()
                    .decide(&approval.id, &approval.session_id, allow);
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    });
    let outcome = async {
        let job: Job = serde_json::from_value(
            service
                .dispatch(Request {
                    method: "POST".into(),
                    path: "/api/jobs".into(),
                    body: json!({
                        "workspace":project,"model":id,"mode":"code","web":false,
                        "task":TASK
                    }),
                })
                .await?,
        )?;
        let done = match tokio::time::timeout(
            Duration::from_secs(240),
            service.engine.wait(&job.id),
        )
        .await
        {
            Ok(done) => done?,
            Err(_) => {
                tokio::time::timeout(Duration::from_secs(15), service.engine.cancel(&job.id))
                    .await??;
                tokio::time::timeout(Duration::from_secs(5), service.engine.wait(&job.id)).await??
            }
        };
        let preserved = fs::read_to_string(project.join("test_helpers.py"))? == TESTS;
        let independent = check(&project).await?;
        let assertions_passed = assertions_passed(&independent);
        let events = service
            .engine
            .store()
            .recent_events(&job.session_id, 2000)?;
        let ran_check = events.iter().any(|e| {
            e["type"] == "tool.completed"
                && e["payload"]["tool"] == "exec"
                && e["payload"]["success"] == true
        });
        Ok::<Value, anyhow::Error>(json!({
            "model":model,"hardware":catalog["hardware"],"runtime":catalog["runtime"],
            "fixture_provenance":coding_fixture::provenance(),
            "job":done,"tests_preserved":preserved,"model_ran_successful_command":ran_check,
            "independent_check":independent,"baseline_check":baseline,
            "independent_assertions_passed":assertions_passed,
            "passed":done.status=="completed" && preserved && assertions_passed && ran_check,
            "source_after":fs::read_to_string(project.join("helpers.py"))?,"events":events
        }))
    }
    .await;
    approvals.abort();
    let cleanup = tokio::time::timeout(Duration::from_secs(15), service.engine.shutdown()).await;
    let cleanup_error = match cleanup {
        Ok(Ok(())) => None,
        Ok(Err(e)) => Some(format!("{e:#}")),
        Err(_) => Some("shutdown exceeded 15 seconds".into()),
    };
    let mut report =
        outcome.unwrap_or_else(|e| json!({"file":file,"passed":false,"error":format!("{e:#}")}));
    if let Some(error) = cleanup_error {
        report["cleanup_error"] = json!(error);
        report["passed"] = json!(false);
    }
    Ok(report)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    ensure!(
        args.len() >= 2,
        "usage: live_local_coding report.json model.gguf [model.gguf ...]"
    );
    let server = std::env::var("SHADOWCODE_LLAMA_SERVER")?;
    let mut reports = Vec::new();
    for file in &args[1..] {
        let result = match evaluate(file, &server).await {
            Ok(report) => report,
            Err(error) => json!({"file":file,"passed":false,"error":format!("{error:#}")}),
        };
        println!(
            "{} passed={} status={} error={}",
            file, result["passed"], result["job"]["status"], result["error"]
        );
        reports.push(result);
        fs::write(
            &args[0],
            serde_json::to_vec_pretty(&json!({
                "scope":"One small Python repair fixture per model; not a general coding benchmark",
                "context_tokens":8192,"max_steps":14,"timeout_seconds":240,"results":reports
            }))?,
        )?;
    }
    ensure!(
        reports.iter().all(|r| r["passed"] == true),
        "One or more models did not pass the coding fixture; inspect the report"
    );
    Ok(())
}
