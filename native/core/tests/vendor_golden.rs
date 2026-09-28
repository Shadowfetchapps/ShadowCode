//! Golden transcripts from the current vendor CLIs, captured live on
//! 2026-09-28 through ShadowCode (Codex 0.158.0, Claude Code 2.1.284,
//! Cursor Agent 2026.09.26, Grok 1.0.41) and stripped of personal data
//! (paths, e-mail, host and account ids). Each test replays the vendor's
//! stdout into the adapter exactly as the runner would.
use serde_json::{json, Value};
use shadowcode_core::cli_agent::{
    adapter_for, claude_probe, usage::UsageSnapshot, CliAdapter, LaunchOptions, Update, Vendor,
};
use std::path::{Path, PathBuf};

fn fixture(name: &str) -> Vec<String> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vendors")
        .join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .map(str::to_owned)
        .collect()
}

fn options(model: &str) -> LaunchOptions {
    LaunchOptions {
        binary: "vendor".into(),
        workspace: PathBuf::from("/tmp/shadowcode-fixture/project"),
        model: model.into(),
        ..Default::default()
    }
}

/// Everything the adapter sent and reported while the transcript played.
/// Approvals are allowed, as the user would in the live run.
fn replay(adapter: &mut dyn CliAdapter, lines: &[String]) -> (Vec<Value>, Vec<Update>) {
    let mut sent = Vec::new();
    let mut updates = Vec::new();
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
                .map(|line| serde_json::from_str::<Value>(line).unwrap()),
        );
        updates.extend(step.updates);
    }
    (sent, updates)
}

fn text(updates: &[Update]) -> String {
    updates
        .iter()
        .filter_map(|u| match u {
            Update::Text(t) | Update::TurnCompleted { text: Some(t), .. } => Some(t.as_str()),
            _ => None,
        })
        .collect()
}

fn warnings(updates: &[Update]) -> Vec<&str> {
    updates
        .iter()
        .filter_map(|u| match u {
            Update::Warning(w) => Some(w.as_str()),
            _ => None,
        })
        .collect()
}

fn usages(updates: &[Update]) -> Vec<(u64, u64, u64)> {
    updates
        .iter()
        .filter_map(|u| match u {
            Update::Usage {
                input,
                output,
                cached,
            } => Some((*input, *output, *cached)),
            _ => None,
        })
        .collect()
}

fn completed(updates: &[Update]) -> bool {
    updates.iter().any(|u| {
        matches!(
            u,
            Update::TurnCompleted {
                interrupted: false,
                ..
            }
        )
    })
}

#[test]
fn codex_resumed_thread_takes_effort_per_turn_and_old_usage_quietly() {
    // Codex 0.158 keeps a resumed thread's stored effort even when the
    // app-server runs with `-c model_reasoning_effort`, and repeats the
    // previous turn's token usage right after `thread/resume`.
    let thread = "01a0e970-274a-7692-a570-26574f719e62";
    let mut adapter = adapter_for(Vendor::Codex, false);
    let launch = LaunchOptions {
        resume: Some(thread.into()),
        effort: Some("medium".into()),
        ..options("gpt-6-luna")
    };
    let (_, args) = adapter.command(&launch);
    assert_eq!(
        args,
        ["-c", "model_reasoning_effort=\"medium\"", "app-server"]
    );
    let mut sent: Vec<Value> = adapter
        .on_start(&launch)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    adapter
        .prompt("What single word did you reply with last time?", &[])
        .unwrap();
    let (more, updates) = replay(&mut *adapter, &fixture("codex-0.158.0-resume.jsonl"));
    sent.extend(more);
    let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
    assert_eq!(
        methods,
        ["initialize", "initialized", "thread/resume", "turn/start"]
    );
    assert_eq!(sent[2]["params"]["threadId"], thread);
    assert_eq!(sent[2]["params"]["model"], "gpt-6-luna");
    assert_eq!(sent[3]["params"]["effort"], "medium");
    assert_eq!(warnings(&updates), Vec::<&str>::new());
    // Only the new turn's usage counts; the replayed snapshot is dropped.
    assert_eq!(usages(&updates), [(21368, 6, 18176)]);
    assert_eq!(text(&updates), "ALPHA");
    assert!(completed(&updates));
    assert!(updates
        .iter()
        .any(|u| matches!(u, Update::RateLimits(v) if v["limitId"] == "codex")));
}

#[test]
fn codex_commentary_and_final_answer_are_separate_paragraphs() {
    let mut adapter = adapter_for(Vendor::Codex, false);
    adapter.on_start(&options("gpt-6-luna"));
    adapter
        .prompt("Run curl and reply with its output", &[])
        .unwrap();
    let (sent, updates) = replay(&mut *adapter, &fixture("codex-0.158.0-approval.jsonl"));
    let approval = updates
        .iter()
        .find_map(|u| match u {
            Update::Approval(p) => Some(p.clone()),
            _ => None,
        })
        .expect("the escalated command asks ShadowCode");
    assert_eq!(approval.kind, "command");
    assert!(approval.command.contains("curl -s -o /dev/null"));
    assert!(sent
        .iter()
        .any(|m| m["id"] == 0 && m["result"]["decision"] == "accept"));
    assert_eq!(
        text(&updates),
        "I\u{2019}ll request approval to run that exact network command outside the sandbox.\n\n200"
    );
    assert_eq!(warnings(&updates), Vec::<&str>::new());
    assert!(completed(&updates));
}

#[test]
fn claude_initialize_lists_the_accounts_models_and_plan() {
    let line: Value = serde_json::from_str(&fixture("claude-2.1.284-initialize.jsonl")[0]).unwrap();
    assert_eq!(line["response"]["request_id"], claude_probe::INITIALIZE_ID);
    let probe = claude_probe::from_initialize(&line["response"]["response"]);
    let ids: Vec<&str> = probe.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids[0], "default");
    for id in [
        "opus",
        "sonnet",
        "haiku",
        "claude-fable-5-1",
        "claude-opus-4-6",
    ] {
        assert!(ids.contains(&id), "{id} missing from {ids:?}");
    }
    let haiku = probe.models.iter().find(|m| m.id == "haiku").unwrap();
    assert_eq!(haiku.label, "Haiku 4.5");
    assert!(!haiku.effort, "Haiku takes no --effort");
    assert!(probe.models.iter().find(|m| m.id == "opus").unwrap().effort);
    assert_eq!(probe.subscription.as_deref(), Some("Claude Max"));
    assert_eq!(probe.email.as_deref(), Some("person@example.com"));
}

#[test]
fn claude_rate_limit_event_reports_plan_windows_and_effort_flag() {
    let mut adapter = adapter_for(Vendor::Claude, false);
    let launch = LaunchOptions {
        effort: Some("low".into()),
        ..options("sonnet")
    };
    let (_, args) = adapter.command(&launch);
    let at = args.iter().position(|a| a == "--effort").unwrap();
    assert_eq!(args[at + 1], "low");
    assert!(adapter.env(&launch).is_empty());
    adapter
        .prompt("Reply with exactly the word ALPHA", &[])
        .unwrap();
    adapter.on_start(&launch);
    let (_, updates) = replay(&mut *adapter, &fixture("claude-2.1.284-turn.jsonl"));
    assert_eq!(text(&updates), "ALPHA");
    assert_eq!(warnings(&updates), Vec::<&str>::new());
    assert!(completed(&updates));
    assert!(!updates.iter().any(|u| matches!(u, Update::LimitReached(_))));
    let info = updates
        .iter()
        .find_map(|u| match u {
            Update::RateLimits(info) => Some(info.clone()),
            _ => None,
        })
        .expect("rate_limit_event");
    let now = 1_790_623_543.0;
    let usage = UsageSnapshot::from_claude(&info, Some("Claude Max"), now);
    assert_eq!(usage.state, "ok");
    assert_eq!(usage.label, "83% remaining · 5-hour");
    let windows: Vec<(&str, f64, Option<u64>)> = usage
        .windows
        .iter()
        .map(|w| (w.label.as_str(), w.used_percent.round(), w.window_minutes))
        .collect();
    assert_eq!(
        windows,
        [("5-hour", 17.0, Some(300)), ("Weekly", 8.0, Some(10080))]
    );
    assert_eq!(usage.plan.as_deref(), Some("Claude Max"));
    assert!(usage.pool_shared && !usage.limit_reached);
    assert_eq!(usage.last_refresh, Some(now));

    // The same event with the plan refused stops the job at the limit.
    let mut refused = info.clone();
    refused["status"] = json!("rejected");
    let frame = json!({"type":"rate_limit_event","rate_limit_info":refused}).to_string();
    let step = adapter.on_line(&frame).unwrap();
    assert!(step
        .updates
        .iter()
        .any(|u| matches!(u, Update::LimitReached(d) if d == "Claude reported its 5-hour limit")));
    let limited = UsageSnapshot::from_claude(&refused, None, now);
    assert!(limited.limit_reached);
    assert!(limited.label.starts_with("Plan limit reached · 5-hour"));
    // Extra usage keeps a refused plan window running.
    refused["isUsingOverage"] = json!(true);
    let frame = json!({"type":"rate_limit_event","rate_limit_info":refused}).to_string();
    assert!(!adapter
        .on_line(&frame)
        .unwrap()
        .updates
        .iter()
        .any(|u| matches!(u, Update::LimitReached(_))));
}

#[test]
fn claude_file_write_goes_through_a_shadowcode_approval() {
    let mut adapter = adapter_for(Vendor::Claude, false);
    let launch = options("haiku");
    adapter.prompt("Create hello.txt", &[]).unwrap();
    adapter.on_start(&launch);
    let (sent, updates) = replay(
        &mut *adapter,
        &fixture("claude-2.1.284-file-approval.jsonl"),
    );
    let approval = updates
        .iter()
        .find_map(|u| match u {
            Update::Approval(p) => Some(p.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(approval.kind, "file_change");
    assert_eq!(approval.tool, "claude.Write");
    let answer = sent
        .iter()
        .find(|m| m["type"] == "control_response")
        .unwrap();
    assert_eq!(answer["response"]["response"]["behavior"], "allow");
    assert_eq!(
        answer["response"]["response"]["updatedInput"]["file_path"],
        "/tmp/shadowcode-fixture/project/hello.txt"
    );
    assert!(updates.iter().any(|u| matches!(
        u,
        Update::FilesChanged { paths, .. } if paths == &["/tmp/shadowcode-fixture/project/hello.txt"]
    )));
    assert_eq!(text(&updates), "DONE");
    assert!(completed(&updates));
}

#[test]
fn claude_text_after_a_tool_starts_a_new_paragraph() {
    let mut adapter = adapter_for(Vendor::Claude, false);
    adapter.prompt("edit", &[]).unwrap();
    adapter.on_start(&options("default"));
    let delta = |t: &str| {
        json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":t}}}).to_string()
    };
    let lines = [
        delta("I'll create it."),
        json!({"type":"assistant","message":{"content":[{"type":"text","text":"I'll create it."}]}}).to_string(),
        json!({"type":"assistant","message":{"content":[{"type":"tool_use","id":"t1","name":"Write","input":{"file_path":"a.txt"}}]}}).to_string(),
        json!({"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}).to_string(),
        delta("Done"),
        delta("."),
        json!({"type":"result","subtype":"success","result":"Done."}).to_string(),
    ];
    let (_, updates) = replay(&mut *adapter, &lines);
    assert_eq!(text(&updates), "I'll create it.\n\nDone.");
}

#[test]
fn cursor_plan_notice_ends_the_job_at_the_plan_limit() {
    // Cursor's Free plan answers with the notice as the whole reply and an
    // ordinary `end_turn`; it used to be reported as a completed answer.
    let session = "38b697f1-e0e6-47ed-9c95-66c3d20f6217";
    let mut adapter = adapter_for(Vendor::Cursor, false);
    let launch = LaunchOptions {
        resume: Some(session.into()),
        ..options("composer-2.5[fast=true]")
    };
    let mut sent: Vec<Value> = adapter
        .on_start(&launch)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    adapter
        .prompt("What single word did you reply with?", &[])
        .unwrap();
    let (more, updates) = replay(
        &mut *adapter,
        &fixture("cursor-2026.09.26-plan-limit.jsonl"),
    );
    sent.extend(more);
    let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
    assert_eq!(
        methods,
        [
            "initialize",
            "authenticate",
            "session/load",
            "session/set_model",
            "session/prompt"
        ]
    );
    // The replayed history is not shown again.
    assert!(!text(&updates).contains("ALPHA"));
    assert!(updates.iter().any(|u| matches!(
        u,
        Update::LimitReached(d) if d == "Cursor: Upgrade your plan to continue"
    )));
    assert!(!completed(&updates));
}

#[test]
fn cursor_ordinary_replies_that_mention_limits_still_complete() {
    for reply in [
        "The API returns 429 when the rate limit reached its ceiling; retry with backoff after reading the Retry-After header, and keep the batch size below the documented maximum so the usage limit is never hit.",
        "ALPHA",
    ] {
        let mut adapter = adapter_for(Vendor::Cursor, false);
        adapter.prompt("question", &[]).unwrap();
        adapter.on_start(&options("auto"));
        let lines = [
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}}).to_string(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s"}}).to_string(),
            json!({"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"s","update":{"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":reply}}}}).to_string(),
            json!({"jsonrpc":"2.0","id":3,"result":{"stopReason":"end_turn"}}).to_string(),
        ];
        let (_, updates) = replay(&mut *adapter, &lines);
        assert!(completed(&updates), "{reply}");
        assert!(!updates
            .iter()
            .any(|u| matches!(u, Update::LimitReached(_))));
    }
}

#[test]
fn grok_resumed_session_switches_model_and_reports_tokens() {
    // Grok's `--model` picks the model of a new session only: a loaded
    // session keeps its stored model unless `session/set_config_option`
    // changes it. Token counts come in the prompt result's `_meta.usage`.
    let session = "01a0e991-3240-7172-aaa4-f7bb414fe55e";
    let mut adapter = adapter_for(Vendor::Grok, false);
    let launch = LaunchOptions {
        resume: Some(session.into()),
        effort: Some("medium".into()),
        ..options("grok-4.7-build-fast")
    };
    let (_, args) = adapter.command(&launch);
    assert_eq!(args, ["agent", "--model", "grok-4.7-build-fast", "stdio"]);
    let mut sent: Vec<Value> = adapter
        .on_start(&launch)
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    adapter
        .prompt("What single word did you reply with last time?", &[])
        .unwrap();
    let (more, updates) = replay(&mut *adapter, &fixture("grok-1.0.41-resume-switch.jsonl"));
    sent.extend(more);
    let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
    // The session already runs at medium effort, so only the model moves.
    assert_eq!(
        methods,
        [
            "initialize",
            "session/load",
            "session/set_config_option",
            "session/prompt"
        ]
    );
    assert_eq!(
        sent[2]["params"],
        json!({"sessionId":session,"configId":"model","value":"grok-4.7-build-fast"})
    );
    assert_eq!(text(&updates), "ALPHA");
    assert_eq!(usages(&updates), [(19913, 37, 1152)]);
    assert_eq!(warnings(&updates), Vec::<&str>::new());
    assert!(completed(&updates));
}

#[test]
fn acp_effort_uses_the_sessions_thought_level_option() {
    let config = json!([
        {"id":"model","category":"model","type":"select","currentValue":"grok-4.7",
         "options":[{"value":"grok-4.7"},{"value":"grok-4.7-build-fast"}]},
        {"id":"reasoning_effort","category":"thought_level","type":"select","currentValue":"high",
         "options":[{"value":"xhigh"},{"value":"high"},{"value":"medium"},{"value":"low"}]}
    ]);
    for (answer, warned) in [
        (json!({"jsonrpc":"2.0","id":3,"result":{}}), false),
        (
            json!({"jsonrpc":"2.0","id":3,"error":{"code":-32602,"message":"no"}}),
            true,
        ),
    ] {
        let mut adapter = adapter_for(Vendor::Grok, false);
        let launch = LaunchOptions {
            effort: Some("low".into()),
            ..options("grok-4.7")
        };
        adapter.on_start(&launch);
        adapter.prompt("hi", &[]).unwrap();
        let lines = [
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}})
                .to_string(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s","configOptions":config}})
                .to_string(),
        ];
        let (sent, _) = replay(&mut *adapter, &lines);
        // The model already matches; only the effort is set, then the
        // prompt waits for the answer.
        let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
        assert_eq!(methods, ["session/new", "session/set_config_option"]);
        assert_eq!(
            sent[1]["params"],
            json!({"sessionId":"s","configId":"reasoning_effort","value":"low"})
        );
        assert!(!adapter.ready());
        let step = adapter.on_line(&answer.to_string()).unwrap();
        assert!(step.send[0].contains("session/prompt"));
        assert_eq!(!warnings(&step.updates).is_empty(), warned);
    }
    // Without the option (Cursor, older agents) nothing is sent.
    let mut adapter = adapter_for(Vendor::Grok, false);
    adapter.on_start(&LaunchOptions {
        effort: Some("low".into()),
        ..options("default")
    });
    adapter.prompt("hi", &[]).unwrap();
    let (sent, _) = replay(
        &mut *adapter,
        &[
            json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1,"agentCapabilities":{}}})
                .to_string(),
            json!({"jsonrpc":"2.0","id":2,"result":{"sessionId":"s"}}).to_string(),
        ],
    );
    let methods: Vec<&str> = sent.iter().filter_map(|m| m["method"].as_str()).collect();
    assert_eq!(methods, ["session/new", "session/prompt"]);
}

/// A private fake `claude`: `auth status`, `--help` (padded past 4 KB, as
/// the real one is) and the SDK `initialize` answer from the golden capture.
#[cfg(unix)]
mod claude_catalog {
    use super::fixture;
    use serde_json::json;
    use shadowcode_core::cli_agent::{catalog::VendorCatalog, CliAgentsConfig, Vendor};
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    const FAKE: &str = r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
root = Path(__file__).parent
modern = (root / 'modern').exists()
args = sys.argv[1:]
if args == ['--version']:
    print('2.1.284 (Claude Code)' if modern else '1.0.0 (Claude Code)'); sys.exit(0)
if args[:2] == ['auth', 'status']:
    print(json.dumps({'loggedIn': True, 'authMethod': 'claude.ai'})); sys.exit(0)
if args == ['--help']:
    print('Usage: claude [options]\n' + '  --padding-option' + ' ' * 5000)
    if modern:
        print('  --effort <level>  Effort level for the current session (low, medium, high, xhigh, max)')
    print("  --model <model>  Provide an alias for the latest model (e.g. 'fable', 'opus', or 'sonnet')")
    sys.exit(0)
if '-p' in args and modern:
    request = json.loads(sys.stdin.readline())
    assert request['request']['subtype'] == 'initialize'
    print((root / 'initialize.jsonl').read_text().strip(), flush=True)
    sys.stdin.read()
sys.exit(1)
"#;

    fn fake(root: &Path, modern: bool) -> CliAgentsConfig {
        let binary = root.join("claude");
        fs::write(&binary, FAKE).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        fs::write(
            root.join("initialize.jsonl"),
            fixture("claude-2.1.284-initialize.jsonl").join("\n"),
        )
        .unwrap();
        if modern {
            fs::write(root.join("modern"), "").unwrap();
        }
        CliAgentsConfig {
            claude_binary: binary.display().to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn claude_rows_come_from_the_cli_with_effort_support_and_plan_usage() {
        let root = tempfile::tempdir().unwrap();
        let config = fake(root.path(), true);
        let catalog = VendorCatalog::new();
        let status = catalog.refresh(Vendor::Claude, &config, true).await;
        assert_eq!(status.effort_flag, Some(true));
        let ids: Vec<&str> = status.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids[0], "default");
        assert!(ids.contains(&"haiku") && ids.contains(&"claude-fable-5-1"));
        let account = status.account.clone().unwrap();
        assert_eq!(account.plan.as_deref(), Some("Claude Max"));
        assert_eq!(account.email.as_deref(), Some("person@example.com"));
        let (rows, _) = catalog.picker_cached(&config).await;
        let row = |id: &str| rows.iter().find(|r| r.id == id).unwrap().clone();
        assert_eq!(row("cli:claude:opus").name, "Claude Code · Opus 5.5");
        assert_eq!(row("cli:claude:opus").reasoning, Some(true));
        assert_eq!(row("cli:claude:haiku").reasoning, Some(false));
        assert_eq!(row("cli:claude").name, "Claude Code · Default");
        let json = row("cli:claude:haiku").to_json();
        assert_eq!(json["reasoning"], false);
        assert!(!shadowcode_core::effort::row_supports(&json));
        assert!(shadowcode_core::effort::row_supports(
            &row("cli:claude:opus").to_json()
        ));
        // Before a task: no figures, and the reason.
        let usage = status.usage_for("opus", shadowcode_core::now());
        assert_eq!(usage.state, "unavailable");
        assert!(usage.detail[0].contains("after the next Claude Code task"));

        // A task's rate_limit_event fills the Allowance windows, and a later
        // account check keeps them (Claude reports them only during tasks).
        let info = json!({"status":"allowed","resetsAt":1790637000,"rateLimitType":"five_hour",
            "unifiedWindows":{"five_hour":{"utilization":0.17,"resetsAt":1790637000},
                              "seven_day":{"utilization":0.08,"resetsAt":1790776800}}});
        let pushed = catalog
            .apply_rate_limits(Vendor::Claude, &info, "opus")
            .await;
        assert_eq!(pushed.windows.len(), 2);
        assert_eq!(pushed.plan.as_deref(), Some("Claude Max"));
        let again = catalog.refresh(Vendor::Claude, &config, true).await;
        let kept = again.usage_for("opus", shadowcode_core::now());
        assert_eq!(kept.state, "ok");
        assert_eq!(kept.remaining_percent.map(f64::round), Some(83.0));
        let doctor = again.to_doctor_json();
        assert_eq!(doctor["usage"]["windows"][1]["label"], "Weekly");
    }

    #[tokio::test]
    async fn older_claude_keeps_help_aliases_and_the_thinking_budget() {
        let root = tempfile::tempdir().unwrap();
        let config = fake(root.path(), false);
        let status = VendorCatalog::new()
            .refresh(Vendor::Claude, &config, true)
            .await;
        // The aliases sit past the first 4 KB of `--help`.
        let ids: Vec<&str> = status.models.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["default", "fable", "opus", "sonnet"]);
        assert_eq!(status.effort_flag, Some(false));
    }
}
