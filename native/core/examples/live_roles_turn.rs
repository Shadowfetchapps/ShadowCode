//! One real Plan → Implement → Review task, driven like the desktop drives it
//! (Service -> Engine -> roles -> vendor CLI or ShadowCode's own loop).
//!
//! Uses an isolated profile and a scratch Git project with a one-line bug.
//! Vendor CLIs keep their own login. The request is tiny, so the turns use a
//! negligible part of any plan allowance. Approvals are answered yes and
//! printed, so their routing (`<Role> role (<model>): …`) can be checked.
//!
//! Usage:
//!   cargo run -p shadowcode-core --example live_roles_turn -- \
//!     --plan cli:claude --implement "" --review "" \
//!     --ollama hf.co/unsloth/Qwen3-4B-Instruct-2507-GGUF:Q6_K
//! `--ollama <model>` makes the conversation's model an Ollama model on this
//! computer (http://127.0.0.1:11434/v1), so the task asks consent before a
//! cloud role runs; `--conversation <picker id>` uses a picker row instead.
//! A role left out keeps the conversation's model; `skip` skips plan or
//! review.
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::time::{Duration, Instant};

async fn call(service: &Service, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|pos| args.get(pos + 1).cloned())
    };
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    std::fs::create_dir(&project)?;
    std::fs::write(
        project.join("calc.py"),
        "def add(a, b):\n    return a - b\n",
    )?;
    let git = |a: &[&str]| {
        std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@example.invalid"])
            .args(a)
            .current_dir(&project)
            .output()
    };
    git(&["init", "-q"])?;
    git(&["add", "-A"])?;
    git(&["commit", "-q", "-m", "init"])?;
    let project = project.canonicalize()?;
    let paths = AppPaths::isolated(&root.path().join("profile"))?;
    let mut config = json!({
        "trusted_workspaces": [project],
        "permissions": {"mode": "allow_edits"},
        "agent": {"max_steps": 16},
    });
    if let Some(model) = flag("--ollama") {
        config["model"] = json!({
            "provider": "ollama",
            "endpoint": "http://127.0.0.1:11434/v1",
            "name": model,
            "default": model,
            "context_limit": 16384,
        });
    }
    Config::patch(&paths, config)?;
    let service = Service::open(paths, Some(project.clone()))?;
    let mut roles = json!({"pipeline": true});
    for role in ["plan", "implement", "review"] {
        if let Some(value) = flag(&format!("--{role}")) {
            roles[role] = json!(value);
        }
    }
    let view = call(&service, "POST", "/api/roles", roles).await?;
    for role in ["plan", "implement", "review"] {
        let r = &view["roles"][role];
        println!(
            "role {role}: {} ({}, {}, {}){}",
            r["name"].as_str().unwrap_or(""),
            r["runner"].as_str().unwrap_or(""),
            if r["local"] == true { "local" } else { "cloud" },
            r["cost"].as_str().unwrap_or(""),
            if r["skipped"] == true { " skipped" } else { "" }
        );
    }
    let mut body = json!({
        "task": "calc.py has a bug: add() subtracts. Fix add so it returns the sum of a and b. Change nothing else.",
        "roles": true,
    });
    if let Some(target) = flag("--conversation") {
        body["model"] = json!(target);
    }
    let started = Instant::now();
    let mut job = call(&service, "POST", "/api/jobs", body.clone()).await?;
    if job["needs_consent"] == true {
        println!(
            "consent asked: {} -> {}",
            job["handoff"]["reason"].as_str().unwrap_or(""),
            job["handoff"]["to"]
        );
        body["handoff_consent"] = json!(true);
        job = call(&service, "POST", "/api/jobs", body).await?;
    }
    let id = job["id"].as_str().unwrap_or_default().to_owned();
    let session = job["session_id"].as_str().unwrap_or_default().to_owned();
    println!("job {id}: {}", job["model"]);
    let wait = service.engine.wait(&id);
    tokio::pin!(wait);
    let done = loop {
        tokio::select! {
            done = &mut wait => break done?,
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                for approval in service.engine.approvals().list(Some(&session)) {
                    println!("approval: {} | {}", approval.reason, approval.command);
                    let _ = service.engine.approvals().decide(&approval.id, &session, true);
                }
                if started.elapsed() > Duration::from_secs(900) {
                    service.engine.cancel(&id).await?;
                    anyhow::bail!("the task took longer than 15 minutes");
                }
            }
        }
    };
    let events = service
        .engine
        .store()
        .events_after(&session, 0, None, 10_000)?;
    for event in &events {
        let p = &event["payload"];
        match event["type"].as_str().unwrap_or("") {
            "subagent.started" => println!(
                "started {} on {} ({} {})",
                p["role"], p["model"], p["runner"], p["route"]
            ),
            "subagent.finished" => println!(
                "finished {}: {} tokens={} cost={} verdict={} files={}\n  {}",
                p["role"],
                p["status"],
                p["usage"]["total_tokens"],
                p["cost"],
                p["verdict"],
                p["files"].as_array().map_or(0, Vec::len),
                shadowcode_core::tools::truncate(
                    p["summary"].as_str().or(p["error"].as_str()).unwrap_or(""),
                    600
                )
                .replace('\n', "\n  ")
            ),
            "subagent.applied" => println!("applied {}", p["paths"]),
            _ => {}
        }
    }
    println!("status: {} in {:?}", done.status, started.elapsed());
    println!("usage: {}", json!(done.usage));
    println!("summary:\n{}", done.summary);
    println!(
        "calc.py now:\n{}",
        std::fs::read_to_string(project.join("calc.py"))?
    );
    let consented = service
        .engine
        .store()
        .session_meta(&session, shadowcode_core::roles::CONSENT_META)?;
    println!("consented providers: {consented:?}");
    service.engine.shutdown().await?;
    Ok(())
}
