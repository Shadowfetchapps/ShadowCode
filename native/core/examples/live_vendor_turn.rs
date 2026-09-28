//! One minimal real turn through a signed-in subscription CLI, driven exactly
//! like the desktop drives it (Service -> Engine -> runtime -> adapter).
//!
//! Uses an isolated profile and a scratch project; the vendor CLI keeps its
//! own login. The prompt asks for a one-word answer and no tools, so the turn
//! uses a negligible part of the plan allowance.
//!
//! Usage: cargo run --example live_vendor_turn -- cli:grok:grok-4.7 [--second]
//! `--second` sends a follow-up to check native session resume.
//! `--binary <path>` runs another copy of the vendor CLI (a newer release in
//! a scratch prefix) through `cli_agents.<vendor>_binary`. `--effort <level>`
//! sends the composer's reasoning effort. `--model-switch <picker id>` sends
//! the follow-up on another model of the same vendor. `--file` asks for one
//! file edit through an approval. `--mcp` enables a small stdio MCP server
//! for the project and asks the vendor to call it.
use serde_json::{json, Value};
use shadowcode_core::{
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::time::Duration;

fn which(name: &str) -> Option<String> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(name))
        .find(|path| path.is_file())
        .map(|path| path.display().to_string())
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

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let target = std::env::args()
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("usage: live_vendor_turn <picker id> [--second]"))?;
    let second = std::env::args().any(|a| a == "--second");
    let root = tempfile::tempdir()?;
    let project = root.path().join("project");
    std::fs::create_dir(&project)?;
    std::fs::write(project.join("README.md"), "# scratch\n")?;
    let paths = AppPaths::isolated(&root.path().join("profile"))?;
    Config::patch(&paths, json!({"trusted_workspaces":[project]}))?;
    let args: Vec<String> = std::env::args().collect();
    let flag_value = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|pos| args.get(pos + 1).cloned())
    };
    if let Some(binary) = flag_value("--binary") {
        let vendor = target
            .trim_start_matches("cli:")
            .split(':')
            .next()
            .unwrap_or_default()
            .to_owned();
        Config::patch(
            &paths,
            json!({"cli_agents":{format!("{vendor}_binary"): binary}}),
        )?;
        println!("binary: {binary}");
    }
    // --openrouter-cache <file>: start from a saved OpenRouter model list
    // (`openrouter-models.json`) instead of downloading it.
    if let Some(cache) = flag_value("--openrouter-cache") {
        std::fs::create_dir_all(&paths.state)?;
        std::fs::copy(&cache, paths.state.join("openrouter-models.json"))?;
    }
    let effort = flag_value("--effort");
    let model_switch = flag_value("--model-switch");
    let file = std::env::args().any(|a| a == "--file");
    let mcp = std::env::args().any(|a| a == "--mcp");
    let service = Service::open(paths, Some(project.clone()))?;
    if mcp {
        // A dependency-free stdio MCP server with one tool that returns a
        // fixed word, enabled for this project like the MCP page does.
        std::fs::write(
            project.join("probe-mcp.mjs"),
            r#"import { createInterface } from "node:readline";
const send = (id, result) => process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
createInterface({ input: process.stdin }).on("line", (line) => {
  const msg = JSON.parse(line);
  if (msg.method === "initialize") send(msg.id, { protocolVersion: msg.params.protocolVersion, serverInfo: { name: "probe", version: "1" }, capabilities: { tools: {} } });
  else if (msg.method === "tools/list") send(msg.id, { tools: [{ name: "shadow_probe", description: "Returns the ShadowCode probe word.", inputSchema: { type: "object", properties: {} }, annotations: { readOnlyHint: true } }] });
  else if (msg.method === "tools/call") send(msg.id, { content: [{ type: "text", text: "MCPWORD-7391" }] });
  else if (msg.id !== undefined) send(msg.id, {});
});
"#,
        )?;
        let node = which("node").ok_or_else(|| anyhow::anyhow!("node is not on PATH"))?;
        call(
            &service,
            "POST",
            "/api/mcp/servers",
            json!({"definition":{"name":"probe","command":[node, project.join("probe-mcp.mjs")]}}),
        )
        .await?;
        let catalog = call(&service, "GET", "/api/mcp/servers", json!({})).await?;
        let entry = catalog["servers"]
            .as_array()
            .and_then(|s| s.iter().find(|e| e["id"] == "config:probe").cloned())
            .ok_or_else(|| anyhow::anyhow!("probe MCP server was not registered: {catalog}"))?;
        call(
            &service,
            "POST",
            "/api/mcp/activation",
            json!({"workspace":project,"server":"config:probe","hash":entry["hash"],"enabled":true}),
        )
        .await?;
        println!("mcp: probe server enabled");
    }
    // --compare <other id>: race this model against another on a small bug
    // in a scratch git repository, keep the first lane that changed the
    // file, and check the fix reached the project.
    let args: Vec<String> = std::env::args().collect();
    if let Some(pos) = args.iter().position(|a| a == "--compare") {
        let other = args.get(pos + 1).cloned().unwrap_or_default();
        let git = |a: &[&str]| {
            std::process::Command::new("git")
                .args(a)
                .current_dir(&project)
                .output()
        };
        std::fs::write(
            project.join("calc.py"),
            "def add(a, b):\n    return a - b\n",
        )?;
        git(&["init", "-q"])?;
        git(&["-c", "user.name=t", "-c", "user.email=t@t", "add", "-A"])?;
        git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-qm",
            "init",
        ])?;
        let started = call(
            &service,
            "POST",
            "/api/compare",
            json!({"workspace": project, "task": "calc.py's add() subtracts. Fix it so add(2, 3) == 5. Edit the file; don't run anything.", "models": [target, other]}),
        )
        .await?;
        let id = started["id"].as_str().unwrap_or_default().to_owned();
        println!(
            "compare {id}: {} lanes",
            started["lanes"].as_array().map_or(0, Vec::len)
        );
        let begin = std::time::Instant::now();
        let record = loop {
            // Lane jobs may ask to edit files; approve them like a user would.
            let pending = call(&service, "GET", "/api/approvals", json!({})).await?;
            for approval in pending["approvals"].as_array().into_iter().flatten() {
                let aid = approval["id"].as_str().unwrap_or_default();
                call(
                    &service,
                    "POST",
                    &format!("/api/approvals/{aid}"),
                    json!({"decision":"approve","session_id":approval["session_id"]}),
                )
                .await?;
            }
            let record = call(&service, "GET", &format!("/api/compare/{id}"), json!({})).await?;
            if record["state"] != "running" || begin.elapsed() > Duration::from_secs(300) {
                break record;
            }
            tokio::time::sleep(Duration::from_millis(700)).await;
        };
        println!(
            "state={} in {:.1}s",
            record["state"],
            begin.elapsed().as_secs_f64()
        );
        for lane in record["lanes"].as_array().into_iter().flatten() {
            println!(
                "  lane {} status={} files={} summary={:?}",
                lane["model"],
                lane["status"],
                lane["changed_files"],
                lane["summary"]
                    .as_str()
                    .map(|s| s.chars().take(120).collect::<String>())
            );
        }
        if let Some(winner) = record["lanes"].as_array().and_then(|l| {
            l.iter().find(|lane| {
                lane["changed_files"]
                    .as_array()
                    .is_some_and(|f| !f.is_empty())
            })
        }) {
            let kept = call(
                &service,
                "POST",
                &format!("/api/compare/{id}/keep"),
                json!({"model": winner["model"]}),
            )
            .await?;
            println!(
                "kept {} -> state={} applied={}",
                winner["model"], kept["state"], kept["applied_files"]
            );
            println!(
                "calc.py now: {:?}",
                std::fs::read_to_string(project.join("calc.py"))?
            );
            let board = call(
                &service,
                "GET",
                &format!("/api/compare/scoreboard?workspace={}", project.display()),
                json!({}),
            )
            .await?;
            println!("scoreboard: {}", board["rows"]);
            let worktrees = git(&["worktree", "list"])?;
            println!(
                "worktrees left: {}",
                String::from_utf8_lossy(&worktrees.stdout).lines().count()
            );
        }
        return Ok(());
    }
    // --allowance: print the Allowance rows and stop.
    if std::env::args().any(|a| a == "--allowance") {
        let allowance = call(&service, "GET", "/api/allowance", json!({})).await?;
        for row in allowance["rows"].as_array().into_iter().flatten() {
            println!(
                "{:<16} {:<14} {}",
                row["product"].as_str().unwrap_or(""),
                row["state"].as_str().unwrap_or(""),
                row["headline"].as_str().unwrap_or("")
            );
        }
        return Ok(());
    }
    let picker = call(&service, "GET", "/api/picker", json!({})).await?;
    let row = picker["targets"]
        .as_array()
        .and_then(|rows| rows.iter().find(|r| r["id"] == target.as_str()).cloned())
        .ok_or_else(|| {
            let prefix = target.split(':').take(2).collect::<Vec<_>>().join(":");
            let rows: Vec<String> = picker["targets"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["id"].as_str())
                .filter(|id| id.starts_with(&prefix))
                .map(str::to_owned)
                .collect();
            anyhow::anyhow!("{target} is not a picker row; rows: {rows:?}")
        })?;
    println!(
        "row: {} | {} | {} | usage: {}",
        row["name"], row["availability_label"], row["subtitle"], row["usage"]["label"]
    );
    if std::env::args().any(|a| a == "--picker-only") {
        let vendor = row["provider"]
            .as_str()
            .unwrap_or("")
            .trim_start_matches("cli:")
            .to_owned();
        println!("usage json: {}", row["usage"]);
        println!("vendor status: {}", picker["vendors"][&vendor]);
        return Ok(());
    }
    // --command: one harmless shell command, so the runtime must ask for
    // permission; the approval is granted through the same API the window uses.
    let command = std::env::args().any(|a| a == "--command");
    // --web: the task gets web tools (native loop only). --image: a small red
    // PNG is attached, with consent to upload it.
    let web = std::env::args().any(|a| a == "--web");
    let image = std::env::args().any(|a| a == "--image");
    if image {
        std::fs::write(
            project.join("red.png"),
            [
                0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
                0x44, 0x52, 0x00, 0x00, 0x00, 0x10, 0x00, 0x00, 0x00, 0x10, 0x08, 0x02, 0x00, 0x00,
                0x00, 0x90, 0x91, 0x68, 0x36, 0x00, 0x00, 0x00, 0x17, 0x49, 0x44, 0x41, 0x54, 0x78,
                0x9c, 0x63, 0xf8, 0xcf, 0xc0, 0x40, 0x12, 0x22, 0x4d, 0xf5, 0xa8, 0x86, 0x51, 0x0d,
                0x43, 0x4a, 0x03, 0x00, 0x90, 0xf9, 0xff, 0x01, 0xf9, 0xe1, 0xfa, 0x78, 0x00, 0x00,
                0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
            ],
        )?;
    }
    let prompts: &[&str] = if web {
        &["Use web_search to find the official website of the Tokio asynchronous Rust runtime, then reply with only its URL."]
    } else if image {
        &["What colour is the attached image? Reply with one word."]
    } else if command {
        &["Run the shell command `date +%Y` in the project folder and reply with only its output."]
    } else if file {
        &["Create a file named hello.txt in the project folder containing exactly the text: hi from shadowcode\nThen reply with the word DONE."]
    } else if mcp {
        &["Call the MCP tool shadow_probe from the probe MCP server and reply with exactly the text it returned, nothing else."]
    } else if second || model_switch.is_some() {
        &[
            "Reply with exactly the word ALPHA and nothing else. Do not use any tools.",
            "What single word did you reply with last time? Reply with just that word. Do not use any tools.",
        ]
    } else {
        &["Reply with exactly the word ALPHA and nothing else. Do not use any tools."]
    };
    // --prompt <text> replaces the first prompt (e.g. a command that needs
    // the vendor to ask for permission outside its sandbox).
    let custom = flag_value("--prompt");
    let mut session: Option<String> = None;
    for (index, prompt) in prompts.iter().enumerate() {
        let prompt = match (&custom, index) {
            (Some(custom), 0) => custom.as_str(),
            _ => *prompt,
        };
        // The follow-up of --model-switch runs on the other model.
        let model = match (&model_switch, &session) {
            (Some(other), Some(_)) => other.clone(),
            _ => target.clone(),
        };
        let mut body = json!({"task": prompt, "model": model, "workspace": project, "web": web});
        if let Some(effort) = &effort {
            body["effort"] = json!(effort);
        }
        if image {
            body["images"] = json!(["red.png"]);
            body["handoff_consent"] = json!(true);
        }
        if let Some(sid) = &session {
            body["session_id"] = json!(sid);
        }
        let started = std::time::Instant::now();
        let job = call(&service, "POST", "/api/jobs", body).await?;
        if job["needs_consent"] == true {
            println!("consent requested: {}", job["handoff"]);
            break;
        }
        let id = job["id"].as_str().unwrap_or_default().to_owned();
        session = job["session_id"].as_str().map(str::to_owned);
        let waiting = service.engine.wait(&id);
        tokio::pin!(waiting);
        let deadline = tokio::time::sleep(Duration::from_secs(240));
        tokio::pin!(deadline);
        let done = loop {
            tokio::select! {
                done = &mut waiting => break done?,
                _ = &mut deadline => {
                    let _ = tokio::time::timeout(Duration::from_secs(10), service.engine.cancel(&id)).await;
                    service.engine.shutdown().await?;
                    anyhow::bail!("turn did not finish in 240 s");
                },
                _ = tokio::time::sleep(Duration::from_millis(300)), if command || file || mcp => {
                    let pending = call(&service, "GET", "/api/approvals", json!({})).await?;
                    for approval in pending["approvals"].as_array().into_iter().flatten() {
                        println!("approval asked: kind={} command={} reason={}", approval["kind"], approval["command"], approval["reason"]);
                        let aid = approval["id"].as_str().unwrap_or_default();
                        call(&service, "POST", &format!("/api/approvals/{aid}"), json!({"decision": "approve", "session_id": approval["session_id"]})).await?;
                    }
                }
            }
        };
        let done = json!(done);
        println!(
            "status={} in {:.1}s | answer={:?} | error={:?}",
            done["status"],
            started.elapsed().as_secs_f64(),
            done["result"]["summary"]
                .as_str()
                .or(done["result"]["text"].as_str())
                .map(|s| s.chars().take(200).collect::<String>()),
            done["error"].as_str()
        );
        println!("job usage: {}", done["usage"]);
        if let Some(sid) = &session {
            let events = service.engine.store().events_after(sid, 0, None, 10_000)?;
            let kinds: Vec<String> = events
                .iter()
                .filter_map(|e| e["type"].as_str().map(str::to_owned))
                .collect();
            println!("events: {}", kinds.join(","));
            for e in &events {
                if e["type"] == "vendor.session"
                    || e["type"] == "model.switched"
                    || e["type"] == "limit.reached"
                    || e["type"] == "usage.updated"
                    || e["type"] == "agent.warning"
                {
                    println!("  {} {}", e["type"], e["payload"]);
                }
            }
        }
        // A completed job is not enough: the minimal text/resume smoke must
        // preserve the exact answer. In particular, duplicate stream/result
        // text must make this executable fail rather than merely print it.
        let expected_word = !web && !image && !command && !file && !mcp && custom.is_none();
        let answer = done["result"]["summary"]
            .as_str()
            .or(done["result"]["text"].as_str())
            .unwrap_or("")
            .trim();
        if file {
            println!(
                "hello.txt: {:?}",
                std::fs::read_to_string(project.join("hello.txt")).ok()
            );
        }
        if mcp && !answer.contains("MCPWORD-7391") {
            service.engine.shutdown().await?;
            anyhow::bail!("live smoke failed: the MCP tool's word is missing from the answer");
        }
        if done["status"] != "completed" || (expected_word && answer != "ALPHA") {
            service.engine.shutdown().await?;
            anyhow::bail!(
                "live smoke failed: status={}, exact-answer check={}",
                done["status"],
                !expected_word || answer == "ALPHA"
            );
        }
    }
    // --switch <picker id>: continue the same conversation on another
    // provider; the first attempt must ask for consent, the second hands off.
    let args: Vec<String> = std::env::args().collect();
    if let (Some(pos), Some(sid)) = (args.iter().position(|a| a == "--switch"), &session) {
        let other = args.get(pos + 1).cloned().unwrap_or_default();
        let body = json!({"task":"What single word did you reply with earlier in this conversation? Reply with just that word. Do not use any tools.","model":other,"workspace":project,"session_id":sid});
        let first = call(&service, "POST", "/api/jobs", body.clone()).await?;
        println!(
            "switch without consent: needs_consent={} status={} handoff={}",
            first["needs_consent"], first["status"], first["handoff"]
        );
        let mut consented = body;
        consented["handoff_consent"] = json!(true);
        let job = call(&service, "POST", "/api/jobs", consented).await?;
        let id = job["id"].as_str().unwrap_or_default().to_owned();
        let done =
            json!(tokio::time::timeout(Duration::from_secs(240), service.engine.wait(&id)).await??);
        println!(
            "after consent: status={} answer={:?}",
            done["status"],
            done["result"]["summary"].as_str()
        );
        for e in service.engine.store().events_after(sid, 0, None, 10_000)? {
            if e["type"] == "agent.handoff" {
                println!("  agent.handoff {}", e["payload"]);
            }
        }
    }
    if let Some(sid) = &session {
        let s = call(&service, "GET", &format!("/api/sessions/{sid}"), json!({})).await?;
        println!(
            "execution_target={} native_sessions={}",
            s["execution_target"], s["native_sessions"]
        );
    }
    service.engine.shutdown().await.ok();
    Ok(())
}
