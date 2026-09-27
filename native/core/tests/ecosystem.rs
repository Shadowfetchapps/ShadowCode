//! Compatibility with other agents' project files: agent definitions,
//! CLAUDE.md / nested AGENTS.md / Cursor rules, `.claude/commands`,
//! `.claude/skills`, and MCP servers shared with vendor CLIs.
#![cfg(unix)]
use serde_json::{json, Value};
use shadowcode_core::{
    agents::{self, AgentMode},
    cli_agent::{adapter_for, CliAdapter, LaunchOptions, McpServerSpec, Vendor},
    config::{Config, PermissionLevel},
    context, instructions,
    mcp::{registry, vendor},
    workflows,
    workspace::Workspace,
};
use std::{fs, path::Path};

fn project() -> (tempfile::TempDir, Workspace) {
    let root = tempfile::tempdir().unwrap();
    let workspace = Workspace::open(root.path()).unwrap();
    (root, workspace)
}
fn put(root: &Path, path: &str, text: &str) {
    let path = root.join(path);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn agent_definitions_are_discovered_with_precedence_and_tool_mapping() {
    let (root, workspace) = project();
    let user = tempfile::tempdir().unwrap();
    put(
        root.path(),
        ".shadow/agents/reviewer.md",
        "---\nname: reviewer\ndescription: ShadowCode reviewer\nmodel: local:gguf:x\nmax_turns: 7\n---\nReview carefully.\n",
    );
    put(
        root.path(),
        ".claude/agents/reviewer.md",
        "---\nname: reviewer\ndescription: Claude reviewer\n---\nShadowed.\n",
    );
    put(
        root.path(),
        ".claude/agents/tester.md",
        "---\nname: tester\ndescription: Runs tests\ntools: Read, Grep, Bash(npm test:*)\nmodel: sonnet\ncolor: blue\n---\nRun the tests.\n",
    );
    put(
        root.path(),
        ".opencode/agent/docs.md",
        "---\ndescription: Writes docs\nmode: subagent\ntools:\n  write: true\n  bash: false\n---\nWrite documentation.\n",
    );
    put(
        root.path(),
        ".shadowcode/agents/explore.md",
        "---\ndescription: Project explorer\n---\nExplore this monorepo.\n",
    );
    put(
        root.path(),
        ".claude/agents/broken.md",
        "---\nmode: sideways\n---\nx\n",
    );
    put(
        user.path(),
        "helper.md",
        "---\nname: helper\ndescription: User helper\nmode: write\n---\nHelp.\n",
    );
    put(
        user.path(),
        "tester.md",
        "---\nname: tester\n---\nUser tester is shadowed by the project.\n",
    );
    let catalog = agents::discover(&workspace, Some(user.path()));
    let get = |name: &str| catalog.get(name).unwrap();
    let reviewer = get("reviewer");
    assert_eq!(reviewer.path, ".shadow/agents/reviewer.md");
    assert_eq!(reviewer.model.as_deref(), Some("local:gguf:x"));
    assert_eq!(reviewer.max_turns, Some(7));
    assert_eq!(reviewer.mode, AgentMode::ReadOnly);
    let tester = get("tester");
    assert_eq!(tester.source, "project");
    // Claude tool names map to native tools; Bash implies write mode.
    for tool in ["read_file", "search_text", "exec"] {
        assert!(tester.tools.contains(&tool.to_owned()), "{tool}");
    }
    assert_eq!(tester.mode, AgentMode::Write);
    assert_eq!(tester.ignored, vec!["color"]);
    let docs = get("docs");
    assert_eq!(docs.mode, AgentMode::Write);
    assert!(docs.deny.contains(&"exec".to_owned()));
    assert!(docs.tools.is_empty());
    // A project file replaces a built-in of the same name.
    assert_eq!(get("explore").source, "project");
    assert_eq!(get("helper").source, "user");
    for builtin in ["plan", "review", "general"] {
        assert_eq!(get(builtin).source, "builtin");
    }
    assert_eq!(get("general").mode, AgentMode::Write);
    assert!(get("agent-plan").name == "plan");
    let shadowed: Vec<_> = catalog
        .shadowed
        .iter()
        .map(|s| s["path"].as_str().unwrap().to_owned())
        .collect();
    assert!(shadowed.contains(&".claude/agents/reviewer.md".to_owned()));
    assert!(shadowed
        .iter()
        .any(|p| p.ends_with("tester.md") && p.starts_with('/')));
    assert!(
        shadowed.contains(&String::new()),
        "the built-in explore is listed as shadowed"
    );
    assert!(catalog
        .issues
        .iter()
        .any(|i| i.starts_with(".claude/agents/broken.md")));
    let json = catalog.to_json();
    assert!(json["agents"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a.get("instructions").is_none()));
}

#[test]
fn mentions_name_agents_but_not_paths_or_addresses() {
    let (_root, workspace) = project();
    let catalog = agents::discover(&workspace, None);
    let (agent, prompt) = agents::mention("@explore where is main?", &catalog).unwrap();
    assert_eq!(
        (agent.name.as_str(), prompt.as_str()),
        ("explore", "where is main?")
    );
    let (agent, prompt) = agents::mention("please ask @agent-review to check", &catalog).unwrap();
    assert_eq!(agent.name, "review");
    assert_eq!(prompt, "please ask  to check");
    assert!(agents::mention("mail me@explore.dev", &catalog).is_none());
    assert!(agents::mention("see @plan/notes.md", &catalog).is_none());
    assert!(agents::mention("see @plan.md", &catalog).is_none());
    assert!(agents::mention("@unknown do it", &catalog).is_none());
    let (agent, _) = agents::mention("Can @plan.", &catalog).unwrap();
    assert_eq!(agent.name, "plan");
}

#[test]
fn claude_md_cursor_rules_and_nested_guidance_are_loaded_once_and_bounded() {
    let (root, workspace) = project();
    put(root.path(), "AGENTS.md", "Shared rules\n");
    // A CLAUDE.md that repeats AGENTS.md is included once.
    put(root.path(), "CLAUDE.md", "Shared rules\n");
    put(root.path(), ".claude/CLAUDE.md", "Claude-only rule\n");
    put(root.path(), ".cursorrules", "Legacy cursor rule\n");
    put(
        root.path(),
        ".cursor/rules/always.mdc",
        "---\ndescription: Always\nalwaysApply: true\n---\nAlways-on cursor rule\n",
    );
    put(
        root.path(),
        ".cursor/rules/rust.mdc",
        "---\nglobs: src/**/*.rs\nalwaysApply: false\n---\nRust glob rule\n",
    );
    put(
        root.path(),
        ".cursor/rules/db.mdc",
        "---\ndescription: Database migrations\n---\nMigration steps\n",
    );
    put(root.path(), "src/deep/AGENTS.md", "Deep folder rule\n");
    put(root.path(), "src/CLAUDE.md", "Src folder rule\n");
    let system = context::system(&workspace, "code");
    assert_eq!(system.matches("Shared rules").count(), 1);
    for text in [
        "Claude-only rule",
        "Legacy cursor rule",
        "Always-on cursor rule",
        "- .cursor/rules/db.mdc: Database migrations",
    ] {
        assert!(system.contains(text), "{text}");
    }
    assert!(!system.contains("Rust glob rule"));
    assert!(!system.contains("Deep folder rule"));
    let nested = instructions::NestedGuidance::default();
    let found = nested.for_paths(&workspace, &["src/deep/lib.rs".into()]);
    let paths: Vec<_> = found.iter().map(|f| f["path"].as_str().unwrap()).collect();
    assert_eq!(
        paths,
        vec![
            ".cursor/rules/rust.mdc",
            "src/CLAUDE.md",
            "src/deep/AGENTS.md"
        ]
    );
    // Delivered once per task.
    assert!(nested
        .for_paths(&workspace, &["src/deep/other.rs".into()])
        .is_empty());
    let absolute = workspace.path.join("src/deep/x.rs");
    assert!(nested
        .for_paths(&workspace, &[absolute.display().to_string()])
        .is_empty());
    // The total root budget is enforced with a note naming what was left out.
    put(root.path(), "CLAUDE.local.md", &"x".repeat(40_000));
    put(root.path(), ".shadow/instructions.md", &"y".repeat(40_000));
    let system = context::system(&workspace, "code");
    assert!(system.contains("Project guidance omitted to stay within"));
    assert!(instructions::glob_match("*.ts", "web/app.ts"));
    assert!(instructions::glob_match("src/**/*.rs", "src/main.rs"));
    assert!(instructions::glob_match("{a,b}/*.md", "b/x.md"));
    assert!(!instructions::glob_match("src/*.rs", "src/a/b.rs"));
}

#[test]
fn claude_commands_are_slash_commands_with_arguments() {
    let (root, workspace) = project();
    put(
        root.path(),
        ".claude/commands/fix-issue.md",
        "---\ndescription: Fix a GitHub issue\nargument-hint: <issue-number>\nallowed-tools: Bash(git:*)\nmodel: claude-sonnet\n---\nFix issue $ARGUMENTS and add a test.\n",
    );
    put(
        root.path(),
        ".claude/commands/review.md",
        "---\ndescription: Claude review\n---\nClaude review body\n",
    );
    put(
        root.path(),
        ".shadow/commands/review.md",
        "---\ndescription: Native review\n---\nNative review body\n",
    );
    let catalog = workflows::discover(&workspace);
    let fix = catalog.resolve("fix-issue", None).unwrap();
    assert_eq!(fix.info.kind, "command");
    assert_eq!(fix.command_row()["arg_spec"], "<issue-number>");
    let guidance = fix.guidance("1234").unwrap();
    assert!(guidance
        .instructions
        .contains("Fix issue 1234 and add a test."));
    // ShadowCode's own command wins; the Claude one is reported, not ambiguous.
    assert_eq!(
        catalog.resolve("review", None).unwrap().info.path,
        ".shadow/commands/review.md"
    );
    assert!(catalog
        .issues
        .iter()
        .any(|i| i.contains(".claude/commands/review.md is hidden")));
    // The same fields are still refused in ShadowCode's own directories.
    put(
        root.path(),
        ".shadow/commands/strict.md",
        "---\nmodel: x\n---\nbody\n",
    );
    assert!(workflows::discover(&workspace)
        .issues
        .iter()
        .any(|i| i.contains("strict.md") && i.contains("not supported")));
}

#[test]
fn claude_skills_are_listed_for_the_model_and_loaded_lazily() {
    let (root, workspace) = project();
    put(
        root.path(),
        ".claude/skills/pdf/SKILL.md",
        "---\nname: pdf\ndescription: Work with PDF files\n---\nUse pdftotext first.\n",
    );
    put(
        root.path(),
        ".claude/skills/secret/SKILL.md",
        "---\nname: secret\ndescription: Manual only\ndisable-model-invocation: true\n---\nManual body\n",
    );
    let skills = workflows::model_skills(&workspace);
    assert_eq!(skills.len(), 1);
    assert_eq!(skills[0].0, "pdf");
    let loaded = workflows::load_skill(&workspace, "pdf", 10).unwrap();
    assert_eq!(loaded["instructions"], "Use pdftot");
    assert_eq!(loaded["truncated"], true);
    assert_eq!(loaded["directory"], ".claude/skills/pdf");
    assert!(workflows::load_skill(&workspace, "secret", 1000).is_err());
    // Users can still run it explicitly as /skill secret.
    assert!(workflows::discover(&workspace)
        .resolve("secret", Some("skill"))
        .is_ok());
}

fn servers() -> Vec<McpServerSpec> {
    vec![
        McpServerSpec {
            name: "files".into(),
            command: vec!["node".into(), "server.mjs".into(), "--root".into()],
            url: String::new(),
        },
        McpServerSpec {
            name: "docs".into(),
            command: Vec::new(),
            url: "http://127.0.0.1:9000/mcp".into(),
        },
    ]
}
fn options(root: &Path) -> LaunchOptions {
    LaunchOptions {
        binary: "cursor-agent".into(),
        workspace: root.to_path_buf(),
        model: "default".into(),
        mcp_servers: servers(),
        ..Default::default()
    }
}
fn session_params(adapter: &mut dyn CliAdapter, init: Value) -> Value {
    let mut sent = Vec::new();
    let step = adapter
        .on_line(&json!({"jsonrpc":"2.0","id":1,"result":init}).to_string())
        .unwrap();
    sent.extend(step.send);
    let frame: Value = serde_json::from_str(sent.last().unwrap()).unwrap();
    assert_eq!(frame["method"], "session/new");
    frame["params"].clone()
}

#[test]
fn acp_sessions_receive_the_enabled_mcp_servers() {
    let root = tempfile::tempdir().unwrap();
    let mut adapter = adapter_for(Vendor::Grok, false);
    adapter.on_start(&options(root.path()));
    let params = session_params(&mut *adapter, json!({"protocolVersion":1}));
    // Without the http capability only stdio servers are offered.
    assert_eq!(
        params["mcpServers"],
        json!([{"name":"files","command":"node","args":["server.mjs","--root"],"env":[]}])
    );
    let mut adapter = adapter_for(Vendor::Grok, false);
    adapter.on_start(&options(root.path()));
    let params = session_params(
        &mut *adapter,
        json!({"protocolVersion":1,"agentCapabilities":{"mcpCapabilities":{"http":true}}}),
    );
    assert_eq!(params["mcpServers"].as_array().unwrap().len(), 2);
    assert_eq!(
        params["mcpServers"][1],
        json!({"type":"http","name":"docs","url":"http://127.0.0.1:9000/mcp","headers":[]})
    );
    // session/load (resume) carries them too.
    let mut adapter = adapter_for(Vendor::Grok, false);
    adapter.on_start(&LaunchOptions {
        resume: Some("old".into()),
        ..options(root.path())
    });
    let step = adapter
        .on_line(&json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{"loadSession":true}}}).to_string())
        .unwrap();
    let frame: Value = serde_json::from_str(step.send.last().unwrap()).unwrap();
    assert_eq!(frame["method"], "session/load");
    assert_eq!(frame["params"]["mcpServers"].as_array().unwrap().len(), 1);
    // No servers: the payload stays an empty list.
    let mut adapter = adapter_for(Vendor::Grok, false);
    adapter.on_start(&LaunchOptions {
        mcp_servers: Vec::new(),
        ..options(root.path())
    });
    assert_eq!(
        session_params(&mut *adapter, json!({"protocolVersion":1}))["mcpServers"],
        json!([])
    );
}

#[test]
fn claude_and_codex_receive_mcp_servers_through_per_run_flags() {
    let root = tempfile::tempdir().unwrap();
    let claude = adapter_for(Vendor::Claude, false);
    let (_, args) = claude.command(&options(root.path()));
    let at = args.iter().position(|a| a == "--mcp-config").unwrap();
    let config: Value = serde_json::from_str(&args[at + 1]).unwrap();
    assert_eq!(
        config,
        json!({"mcpServers":{
            "files":{"type":"stdio","command":"node","args":["server.mjs","--root"]},
            "docs":{"type":"http","url":"http://127.0.0.1:9000/mcp"}
        }})
    );
    let (_, args) = claude.command(&LaunchOptions {
        mcp_servers: Vec::new(),
        ..options(root.path())
    });
    assert!(!args.contains(&"--mcp-config".to_owned()));
    let codex = adapter_for(Vendor::Codex, false);
    let (_, args) = codex.command(&options(root.path()));
    assert_eq!(
        args,
        vec![
            "-c",
            "mcp_servers.files.command=\"node\"",
            "-c",
            "mcp_servers.files.args=[\"server.mjs\",\"--root\"]",
            "-c",
            "mcp_servers.docs.url=\"http://127.0.0.1:9000/mcp\"",
            "app-server"
        ]
    );
    let exec = adapter_for(Vendor::Codex, true);
    let (_, args) = exec.command(&options(root.path()));
    assert_eq!(args[0], "-c");
    assert!(args.iter().position(|a| a == "exec").unwrap() == 6);
}

#[test]
fn only_enabled_secret_free_servers_are_shared_with_vendor_clis() {
    let (_root, workspace) = project();
    let mut config = Config {
        trusted_workspaces: vec![workspace.path.display().to_string()],
        mcp: json!({"servers":[
            {"name":"plain","command":["node","plain.mjs"]},
            {"name":"secret","command":["node","secret.mjs"],"env_refs":{"TOKEN":"MY_TOKEN"}},
            {"name":"off","command":["node","off.mjs"]}
        ]}),
        ..Default::default()
    };
    for id in ["config:plain", "config:secret"] {
        let entry = registry::read(&workspace, &config, id).unwrap();
        registry::activate(&workspace, &mut config, id, &entry.hash, true).unwrap();
    }
    let (shared, skipped) = vendor::plan(&workspace, &config);
    assert_eq!(
        shared,
        vec![McpServerSpec {
            name: "plain".into(),
            command: vec!["node".into(), "plain.mjs".into()],
            url: String::new(),
        }]
    );
    assert_eq!(skipped.len(), 1);
    assert!(skipped[0].starts_with("secret:"));
    let mut read_only = config.clone();
    read_only.permissions.level = PermissionLevel::ReadOnly;
    assert!(vendor::servers(&workspace, &read_only).is_empty());
    let mut off = config.clone();
    off.mcp["share_with_cli_agents"] = json!(false);
    assert!(vendor::servers(&workspace, &off).is_empty());
    let mut untrusted = config.clone();
    untrusted.trusted_workspaces.clear();
    assert!(vendor::servers(&workspace, &untrusted).is_empty());
    // A changed definition is not shared until it is enabled again.
    config.mcp["servers"][0]["command"] = json!(["node", "changed.mjs"]);
    assert!(vendor::servers(&workspace, &config).is_empty());
}
