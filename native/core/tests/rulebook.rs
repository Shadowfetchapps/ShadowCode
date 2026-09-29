//! One rulebook, every agent: the user's profile reaches ShadowCode's own
//! agent and every vendor CLI through that vendor's own per-run mechanism,
//! and the Rules & skills routes show and change it. Vendor protocols are
//! replayed from recorded fixtures or driven by fake CLIs; no real vendor
//! runs and nothing is written to a vendor's home folder.
mod support;
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    cli_agent::{adapter_for, CliAdapter, LaunchOptions, Update, Vendor},
    config::Config,
    paths::AppPaths,
    rulebook::{self, VendorRules},
    service::{Request, Service},
};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};
use vendor_support::{cli_agents, FakeCodex};

fn fixture(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vendors")
        .join(name);
    fs::read_to_string(&path)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn rules(text: &str) -> Option<VendorRules> {
    Some(VendorRules {
        text: text.into(),
        ..Default::default()
    })
}

fn replay(adapter: &mut dyn CliAdapter, lines: &[String]) -> Vec<Value> {
    let mut sent = Vec::new();
    for line in lines {
        let step = adapter.on_line(line).unwrap();
        for update in &step.updates {
            if let Update::Approval(prompt) = update {
                for answer in adapter.approve(&prompt.request_id, true).unwrap() {
                    sent.push(serde_json::from_str(&answer).unwrap());
                }
            }
        }
        sent.extend(
            step.send
                .iter()
                .map(|l| serde_json::from_str::<Value>(l).unwrap()),
        );
    }
    sent
}

const RULES: &str = "<shadowcode-rulebook>\nUser instructions from the user's ShadowCode profile: say PELICAN.\n</shadowcode-rulebook>";

#[test]
fn claude_gets_a_private_rules_file_and_a_per_run_plugin() {
    let mut adapter = adapter_for(Vendor::Claude, false);
    let base = LaunchOptions {
        binary: "claude".into(),
        workspace: PathBuf::from("/tmp/shadowcode-fixture/project"),
        model: "default".into(),
        ..Default::default()
    };
    let (_, plain) = adapter.command(&base);
    assert!(!plain
        .iter()
        .any(|a| a.contains("system-prompt") || a == "--plugin-dir"));
    let with = LaunchOptions {
        rulebook: Some(VendorRules {
            text: RULES.into(),
            file: Some(PathBuf::from("/state/rulebook-runs/run-1/rules.md")),
            plugin_dir: Some(PathBuf::from("/state/rulebook-runs/run-1/plugin")),
        }),
        ..base.clone()
    };
    let (_, args) = adapter.command(&with);
    let at = |flag: &str| args.iter().position(|a| a == flag).unwrap();
    assert_eq!(
        args[at("--append-system-prompt-file") + 1],
        "/state/rulebook-runs/run-1/rules.md"
    );
    assert_eq!(
        args[at("--plugin-dir") + 1],
        "/state/rulebook-runs/run-1/plugin"
    );
    // The rules text itself never appears on the command line (`ps`).
    assert!(!args.iter().any(|a| a.contains("PELICAN")));
    // Nothing is added to the first user message.
    adapter.prompt("hello", &[]).unwrap();
    let first = adapter.on_start(&with);
    assert!(!first[0].contains("PELICAN"));
}

#[test]
fn codex_resume_carries_developer_instructions_from_the_recorded_session() {
    let thread = "01a0e970-274a-7692-a570-26574f719e62";
    let mut adapter = adapter_for(Vendor::Codex, false);
    let launch = LaunchOptions {
        binary: "codex".into(),
        workspace: PathBuf::from("/tmp/shadowcode-fixture/project"),
        model: "gpt-6-luna".into(),
        resume: Some(thread.into()),
        rulebook: rules(RULES),
        ..Default::default()
    };
    let (_, args) = adapter.command(&launch);
    assert!(!args.iter().any(|a| a.contains("PELICAN")));
    let mut sent: Vec<Value> = adapter
        .on_start(&launch)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    adapter.prompt("What did you say?", &[]).unwrap();
    sent.extend(replay(
        &mut *adapter,
        &fixture("codex-0.158.0-resume.jsonl"),
    ));
    let resume = sent
        .iter()
        .find(|m| m["method"] == "thread/resume")
        .unwrap();
    assert_eq!(resume["params"]["developerInstructions"], RULES);
    assert_eq!(resume["params"]["threadId"], thread);
    // The prompt itself is unchanged.
    let turn = sent.iter().find(|m| m["method"] == "turn/start").unwrap();
    assert!(!turn.to_string().contains("PELICAN"));
}

#[test]
fn codex_new_and_fallback_threads_carry_developer_instructions() {
    let mut adapter = adapter_for(Vendor::Codex, false);
    let launch = LaunchOptions {
        binary: "codex".into(),
        workspace: PathBuf::from("/tmp/p"),
        model: "default".into(),
        resume: Some("gone".into()),
        rulebook: rules(RULES),
        ..Default::default()
    };
    adapter.on_start(&launch);
    let step = adapter
        .on_line(r#"{"jsonrpc":"2.0","id":1,"result":{"userAgent":"x"}}"#)
        .unwrap();
    let resume: Value = serde_json::from_str(&step.send[1]).unwrap();
    assert_eq!(resume["method"], "thread/resume");
    assert_eq!(resume["params"]["developerInstructions"], RULES);
    // The stored thread is gone: the fresh thread gets the same rules.
    let step = adapter
        .on_line(r#"{"jsonrpc":"2.0","id":2,"error":{"code":-1,"message":"no rollout"}}"#)
        .unwrap();
    let start: Value = serde_json::from_str(&step.send[0]).unwrap();
    assert_eq!(start["method"], "thread/start");
    assert_eq!(start["params"]["developerInstructions"], RULES);
    // No rulebook: no field.
    let mut adapter = adapter_for(Vendor::Codex, false);
    adapter.on_start(&LaunchOptions {
        rulebook: None,
        ..launch
    });
    let step = adapter
        .on_line(r#"{"jsonrpc":"2.0","id":1,"result":{"userAgent":"x"}}"#)
        .unwrap();
    assert!(!step.send[1].contains("developerInstructions"));
}

#[test]
fn codex_exec_puts_the_rules_ahead_of_the_first_prompt_only() {
    let mut adapter = adapter_for(Vendor::Codex, true);
    let launch = LaunchOptions {
        binary: "codex".into(),
        workspace: PathBuf::from("/tmp/p"),
        model: "default".into(),
        rulebook: rules(RULES),
        ..Default::default()
    };
    adapter.prompt("fix the bug", &[]).unwrap();
    let sent = adapter.on_start(&launch);
    assert_eq!(sent, vec![format!("{RULES}\n\nfix the bug")]);
    assert_eq!(
        adapter.prompt("again", &[]).unwrap(),
        vec!["again".to_owned()]
    );
}

#[test]
fn acp_sends_a_labelled_block_before_the_first_prompt_of_a_resumed_session() {
    let session = "01a0e991-3240-7172-aaa4-f7bb414fe55e";
    let mut adapter = adapter_for(Vendor::Grok, false);
    let launch = LaunchOptions {
        binary: "grok".into(),
        workspace: PathBuf::from("/tmp/shadowcode-fixture/project"),
        model: "grok-4.7-build-fast".into(),
        resume: Some(session.into()),
        effort: Some("medium".into()),
        rulebook: rules(RULES),
        ..Default::default()
    };
    let (_, args) = adapter.command(&launch);
    assert!(!args.iter().any(|a| a.contains("PELICAN")));
    let mut sent: Vec<Value> = adapter
        .on_start(&launch)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    adapter.prompt("What single word?", &[]).unwrap();
    sent.extend(replay(
        &mut *adapter,
        &fixture("grok-1.0.41-resume-switch.jsonl"),
    ));
    let load = sent.iter().find(|m| m["method"] == "session/load").unwrap();
    assert!(!load.to_string().contains("PELICAN"));
    let prompt = sent
        .iter()
        .find(|m| m["method"] == "session/prompt")
        .unwrap();
    let blocks = prompt["params"]["prompt"].as_array().unwrap();
    assert_eq!(blocks.len(), 2);
    assert_eq!(blocks[0], json!({"type":"text","text":RULES}));
    assert_eq!(blocks[1], json!({"type":"text","text":"What single word?"}));
    // A steering note in the same run is sent alone.
    let next = adapter.prompt("and another", &[]).unwrap();
    let next: Value = serde_json::from_str(&next[0]).unwrap();
    assert_eq!(
        next["params"]["prompt"],
        json!([{"type":"text","text":"and another"}])
    );
}

#[test]
fn acp_without_a_rulebook_sends_the_prompt_alone() {
    let mut adapter = adapter_for(Vendor::Grok, false);
    let launch = LaunchOptions {
        binary: "grok".into(),
        workspace: PathBuf::from("/tmp/shadowcode-fixture/project"),
        model: "grok-4.7-build-fast".into(),
        resume: Some("01a0e991-3240-7172-aaa4-f7bb414fe55e".into()),
        effort: Some("medium".into()),
        ..Default::default()
    };
    adapter.on_start(&launch);
    adapter.prompt("What single word?", &[]).unwrap();
    let sent = replay(&mut *adapter, &fixture("grok-1.0.41-resume-switch.jsonl"));
    let prompt = sent
        .iter()
        .find(|m| m["method"] == "session/prompt")
        .unwrap();
    assert_eq!(
        prompt["params"]["prompt"],
        json!([{"type":"text","text":"What single word?"}])
    );
}

// --- Service routes and whole runs ----------------------------------------

struct Setup {
    _root: tempfile::TempDir,
    paths: AppPaths,
    project: PathBuf,
    profile: PathBuf,
    service: Service,
    fake: FakeCodex,
}

fn setup(endpoint: &str) -> Setup {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let project = project.canonicalize().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt","turn":"ok"}));
    let paths = AppPaths::isolated(&root.path().join("app")).unwrap();
    let mut agents = cli_agents(&fake);
    agents["claude_binary"] = json!(root.path().join("fake-claude/claude"));
    Config::patch(
        &paths,
        json!({
            "model":{"name":"fixture","provider":"local","endpoint":endpoint,"context_limit":16384},
            "trusted_workspaces":[project],
            "permissions":{"approve_shell":false},
            "agent":{"retry_attempts":0},
            "cli_agents": agents,
        }),
    )
    .unwrap();
    let profile = rulebook::profile_dir(&paths);
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    Setup {
        _root: root,
        paths,
        project,
        profile,
        service,
        fake,
    }
}

fn put(dir: &Path, rel: &str, text: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
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

async fn finished(service: &Service, job: &Value) -> Value {
    let id = job["id"].as_str().expect("job id");
    let done = tokio::time::timeout(Duration::from_secs(40), service.engine.wait(id))
        .await
        .expect("job did not finish")
        .unwrap();
    json!(done)
}

fn response(text: &str) -> Value {
    json!({"choices":[{"message":{"role":"assistant","content":text},"finish_reason":"stop"}]})
}

#[tokio::test]
async fn the_rules_page_routes_show_edit_switch_and_check() {
    let model = support::server(|_, _| (response("ok"), Duration::ZERO)).await;
    let s = setup(&model.endpoint);
    put(&s.project, "AGENTS.md", "Project rule.\n");
    put(
        &s.project,
        ".shadow/skills/deploy.md",
        "---\ndescription: Deploy\n---\nDeploy it.\n",
    );
    // No profile yet: the page still loads and says where it would be.
    let page = call(&s.service, "GET", "/api/rules", Value::Null)
        .await
        .unwrap();
    assert_eq!(page["profile"]["exists"], false);
    assert_eq!(page["profile"]["path"], json!(s.profile));
    assert_eq!(page["profile"]["agents_md"]["hash"], "missing");
    assert_eq!(page["share_with_cli_agents"], true);
    assert_eq!(page["starters"].as_array().unwrap().len(), 4);
    // Edit the profile AGENTS.md in the app, with stale-write protection.
    let saved = call(
        &s.service,
        "PUT",
        "/api/rules/profile",
        json!({"content":"Profile rule: say PELICAN.\n","expected_hash":"missing"}),
    )
    .await
    .unwrap();
    let hash = saved["hash"].as_str().unwrap().to_owned();
    assert!(call(
        &s.service,
        "PUT",
        "/api/rules/profile",
        json!({"content":"stale","expected_hash":"missing"})
    )
    .await
    .is_err());
    assert_eq!(
        fs::read_to_string(s.profile.join("AGENTS.md")).unwrap(),
        "Profile rule: say PELICAN.\n"
    );
    #[cfg(unix)]
    assert_eq!(
        fs::metadata(&s.profile).unwrap().permissions().mode() & 0o777,
        0o700
    );
    // Install starters and see them with the project's files.
    call(
        &s.service,
        "POST",
        "/api/rules/starters",
        json!({"names":["careful-review"]}),
    )
    .await
    .unwrap();
    let page = call(&s.service, "GET", "/api/rules", Value::Null)
        .await
        .unwrap();
    assert_eq!(page["profile"]["agents_md"]["hash"], json!(hash));
    let ids: Vec<&str> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    for id in [
        "profile:AGENTS.md",
        "profile:skills/careful-review/SKILL.md",
        "project:AGENTS.md",
        "project:.shadow/skills/deploy.md",
    ] {
        assert!(ids.contains(&id), "{id} in {ids:?}");
    }
    // What each agent reads.
    let preview = call(&s.service, "GET", "/api/rules/preview", Value::Null)
        .await
        .unwrap();
    let runners = preview["runners"].as_array().unwrap();
    assert_eq!(runners.len(), 6);
    let codex = runners.iter().find(|r| r["id"] == "codex").unwrap();
    assert_eq!(codex["delivered"], true);
    let items = codex["preview"]["items"].as_array().unwrap();
    let agents_md = items.iter().find(|i| i["path"] == "AGENTS.md").unwrap();
    assert_eq!(agents_md["included"], false);
    assert_eq!(agents_md["reason"], "Codex reads this file itself");
    assert!(codex["preview"]["estimated_tokens"].as_u64().unwrap() > 0);
    // Switch the project's AGENTS.md off for ShadowCode's own agent.
    let workspace = s.project.to_string_lossy().into_owned();
    call(
        &s.service,
        "POST",
        "/api/rules/items",
        json!({"id":"project:AGENTS.md","enabled":false,"workspace":workspace}),
    )
    .await
    .unwrap();
    assert!(call(
        &s.service,
        "POST",
        "/api/rules/items",
        json!({"id":"project:AGENTS.md","enabled":false,"workspace":"/somewhere/else"}),
    )
    .await
    .is_err());
    let preview = call(&s.service, "GET", "/api/rules/preview", Value::Null)
        .await
        .unwrap();
    let native = &preview["runners"][0];
    assert_eq!(native["id"], "shadowcode");
    let row = native["preview"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"] == "AGENTS.md")
        .unwrap()
        .clone();
    assert_eq!(row["included"], false);
    assert_eq!(row["reason"], "Switched off");
    // Sharing with vendor CLIs can be switched off as a whole.
    call(
        &s.service,
        "POST",
        "/api/rules/sharing",
        json!({"enabled":false}),
    )
    .await
    .unwrap();
    let preview = call(&s.service, "GET", "/api/rules/preview", Value::Null)
        .await
        .unwrap();
    assert_eq!(preview["runners"][1]["delivered"], false);
    assert_eq!(preview["runners"][1]["sharing_off"], true);
    // The checker.
    put(
        &s.profile,
        "skills/broken/SKILL.md",
        "---\nname: [x\n---\nb\n",
    );
    let report = call(&s.service, "GET", "/api/rules/check", Value::Null)
        .await
        .unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["errors"], 1);
    // Doctor summarises it.
    let doctor = call(&s.service, "GET", "/api/doctor", Value::Null)
        .await
        .unwrap();
    let check = doctor["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == "rules-and-skills")
        .unwrap()
        .clone();
    assert_eq!(check["status"], "warn");
    assert!(doctor["diagnostic_export"]["content"]
        .as_str()
        .unwrap()
        .contains("rules-and-skills"));
    // Local addresses are refused before anything runs.
    let refused = call(
        &s.service,
        "POST",
        "/api/rules/imports",
        json!({"url":"file:///etc"}),
    )
    .await
    .unwrap_err();
    assert!(refused.to_string().contains("https://"), "{refused:#}");
    s.service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn profile_rules_and_skills_reach_shadowcodes_own_agent() {
    let model = support::server(|_, _| (response("Done."), Duration::ZERO)).await;
    let s = setup(&model.endpoint);
    put(&s.profile, "AGENTS.md", "PROFILE_RULE: answer tersely.\n");
    put(
        &s.profile,
        "skills/tidy/SKILL.md",
        "---\nname: tidy\ndescription: Tidy imports\n---\nPROFILE_SKILL_BODY\n",
    );
    put(
        &s.profile,
        "commands/ship.md",
        "---\ndescription: Ship\n---\nPROFILE_COMMAND: ship $ARGUMENTS\n",
    );
    put(
        &s.profile,
        "agents/helper.md",
        "---\ndescription: Profile helper\n---\nYou help.\n",
    );
    put(&s.project, "AGENTS.md", "PROJECT_RULE.\n");
    let job = call(&s.service, "POST", "/api/jobs", json!({"task":"hello"}))
        .await
        .unwrap();
    let done = finished(&s.service, &job).await;
    assert_eq!(done["status"], "completed", "{done}");
    let requests = model.requests.lock().unwrap().clone();
    let system = requests[0]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    let profile_at = system.find("PROFILE_RULE").unwrap();
    let project_at = system.find("PROJECT_RULE").unwrap();
    assert!(profile_at < project_at);
    assert!(system.contains("User instructions from the user's ShadowCode profile"));
    assert!(system.contains("- tidy: Tidy imports (user profile)"));
    assert!(!system.contains("PROFILE_SKILL_BODY"));
    // Profile commands are slash commands; profile agents are subagents.
    let commands = call(&s.service, "GET", "/api/commands", Value::Null)
        .await
        .unwrap();
    let ship = commands["commands"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "ship")
        .expect("profile command listed")
        .clone();
    assert_eq!(ship["source"], "profile");
    let agents = call(&s.service, "GET", "/api/agents", Value::Null)
        .await
        .unwrap();
    assert!(agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["name"] == "helper" && a["source"] == "profile"));
    let skills = call(&s.service, "GET", "/api/workspace/skills", Value::Null)
        .await
        .unwrap();
    assert!(skills["skills"]
        .as_array()
        .unwrap()
        .iter()
        .any(|k| k["name"] == "tidy" && k["source"] == "profile"));
    // Switched off, a profile agent is gone.
    call(
        &s.service,
        "POST",
        "/api/rules/items",
        json!({"id":"profile:agents/helper.md","enabled":false}),
    )
    .await
    .unwrap();
    let agents = call(&s.service, "GET", "/api/agents", Value::Null)
        .await
        .unwrap();
    assert!(!agents["agents"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["name"] == "helper"));
    s.service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn a_codex_run_gets_developer_instructions_and_a_profile_command_expanded() {
    let s = setup("http://127.0.0.1:9/v1");
    put(&s.profile, "AGENTS.md", "PROFILE_RULE_FOR_CODEX\n");
    put(
        &s.profile,
        "commands/ship.md",
        "---\ndescription: Ship\n---\nPROFILE_COMMAND: ship $ARGUMENTS\n",
    );
    put(&s.project, "AGENTS.md", "Codex reads this itself.\n");
    let job = call(
        &s.service,
        "POST",
        "/api/jobs",
        json!({"task":"say hi","model":"cli:codex"}),
    )
    .await
    .unwrap();
    let done = finished(&s.service, &job).await;
    assert_eq!(done["status"], "completed", "{done}");
    let developer = s.fake.marker("developer.log").unwrap();
    let first: Value = serde_json::from_str(developer.lines().next().unwrap()).unwrap();
    let text = first.as_str().unwrap();
    assert!(text.contains("PROFILE_RULE_FOR_CODEX"));
    assert!(text.contains("never grants permissions"));
    assert!(!text.contains("Codex reads this itself."));
    let sid = done["session_id"].as_str().unwrap();
    let events = s
        .service
        .engine
        .store()
        .events_after(sid, 0, None, 1000)
        .unwrap();
    let delivered = events
        .iter()
        .find(|e| e["type"] == "rules.delivered")
        .expect("rules.delivered event");
    assert_eq!(delivered["payload"]["vendor"], "codex");
    assert_eq!(delivered["payload"]["profile_files"], 1);
    // A profile command reaches Codex expanded, not as an unknown `/ship`.
    let result = call(
        &s.service,
        "POST",
        "/api/commands/run",
        json!({"name":"ship","args":"v2","model":"cli:codex","session_id":sid}),
    )
    .await
    .unwrap();
    let done = finished(&s.service, &result["metadata"]["job"]).await;
    assert_eq!(done["status"], "completed", "{done}");
    let prompts = s.fake.marker("prompts.log").unwrap();
    let last: String = serde_json::from_str(prompts.lines().last().unwrap()).unwrap();
    assert!(last.contains("PROFILE_COMMAND: ship v2"), "{last}");
    assert!(last.contains("explicitly selected the profile command /ship"));
    s.service.engine.shutdown().await.unwrap();
}

/// A stand-in `claude` that records its arguments and, while it runs, what
/// the rules file and plugin folder hold; then answers one turn.
const FAKE_CLAUDE: &str = r#"#!/usr/bin/env python3
import json, os, sys
HERE = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
if "-p" not in args:
    # Probes (`--version`, `auth status`, `--help`) run around the task,
    # including a refresh right after it; they must not replace the record.
    print("2.1.278 (Claude Code)")
    sys.exit(0)
seen = {"args": args}
if "--append-system-prompt-file" in args:
    path = args[args.index("--append-system-prompt-file") + 1]
    seen["rules"] = open(path).read()
    seen["rules_mode"] = oct(os.stat(path).st_mode & 0o777)
if "--plugin-dir" in args:
    root = args[args.index("--plugin-dir") + 1]
    seen["plugin"] = sorted(os.path.relpath(os.path.join(d, f), root) for d, _, fs in os.walk(root) for f in fs)
    seen["plugin_skill"] = open(os.path.join(root, "skills", "tidy", "SKILL.md")).read()
with open(os.path.join(HERE, "seen.json"), "w") as f:
    json.dump(seen, f)
def send(o):
    sys.stdout.write(json.dumps(o) + "\n"); sys.stdout.flush()
for raw in sys.stdin:
    m = json.loads(raw)
    if m.get("type") != "user":
        continue
    send({"type":"system","subtype":"init","session_id":"fake-claude-session"})
    send({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"PELICAN"}]}})
    send({"type":"result","subtype":"success","is_error":False,"result":"PELICAN","usage":{"input_tokens":3,"output_tokens":1}})
"#;

#[tokio::test]
async fn a_claude_run_reads_a_private_rules_file_that_is_removed_afterwards() {
    let s = setup("http://127.0.0.1:9/v1");
    let bin = s._root.path().join("fake-claude");
    fs::create_dir_all(&bin).unwrap();
    let script = bin.join("claude");
    fs::write(&script, FAKE_CLAUDE).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    put(&s.profile, "AGENTS.md", "PROFILE_RULE_FOR_CLAUDE\n");
    put(
        &s.profile,
        "skills/tidy/SKILL.md",
        "---\nname: tidy\ndescription: Tidy imports\nallowed-tools: Bash(*)\n---\nTidy body.\n",
    );
    let job = call(
        &s.service,
        "POST",
        "/api/jobs",
        json!({"task":"hello","model":"cli:claude"}),
    )
    .await
    .unwrap();
    let done = finished(&s.service, &job).await;
    assert_eq!(done["status"], "completed", "{done}");
    let seen: Value =
        serde_json::from_str(&fs::read_to_string(bin.join("seen.json")).unwrap()).unwrap();
    let rules = seen["rules"].as_str().unwrap();
    assert!(rules.contains("PROFILE_RULE_FOR_CLAUDE"));
    assert!(rules.contains("shadowcode-profile:tidy"));
    assert_eq!(seen["rules_mode"], "0o600");
    assert_eq!(
        seen["plugin"],
        json!([".claude-plugin/plugin.json", "skills/tidy/SKILL.md"])
    );
    let skill = seen["plugin_skill"].as_str().unwrap();
    assert!(skill.contains("name: tidy") && !skill.contains("allowed-tools"));
    let args: Vec<String> = serde_json::from_value(seen["args"].clone()).unwrap();
    let file = &args[args
        .iter()
        .position(|a| a == "--append-system-prompt-file")
        .unwrap()
        + 1];
    assert!(!Path::new(file).exists(), "the per-run folder is removed");
    assert!(file.starts_with(&s.paths.state.to_string_lossy().into_owned()));
    // No vendor home folder was touched.
    assert!(!s._root.path().join(".claude").exists());
    s.service.engine.shutdown().await.unwrap();
}
