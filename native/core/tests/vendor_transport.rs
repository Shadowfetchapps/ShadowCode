//! Real subprocess transport regressions, with isolated fake vendor CLIs.
//! No network, real accounts or provider allowance are used.
mod vendor_support;

use serde_json::json;
use shadowcode_core::{
    cli_agent::{acp_probe, codex_probe},
    config::Config,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{fs, os::unix::fs::PermissionsExt, time::Duration};
use vendor_support::{cli_agents, FakeCodex};

async fn run_case(mode: &str, expected_status: &str, expected_text: &str) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt","turn":mode}));
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({"cli_agents":cli_agents(&fake),"trusted_workspaces":[project]}),
    )
    .unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let start = service
        .dispatch(Request {
            method: "POST".into(),
            path: "/api/jobs".into(),
            body: json!({"workspace":project,"task":"transport fixture","model":"cli:codex:gpt-6-astra"}),
        })
        .await
        .unwrap();
    let id = start["id"].as_str().unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), service.engine.wait(id)).await;
    if result.is_err() {
        let _ = tokio::time::timeout(Duration::from_secs(5), service.engine.cancel(id)).await;
    }
    service.engine.shutdown().await.unwrap();
    let done = result
        .expect("vendor transport must finish promptly")
        .unwrap();
    assert_eq!(done.status, expected_status, "{}", done.summary);
    assert!(done.summary.contains(expected_text), "{}", done.summary);
    let stored = service.engine.store().job(id).unwrap().unwrap();
    assert_eq!(stored["status"], expected_status);
    let events = service
        .engine
        .store()
        .events_after(&done.session_id, 0, None, 1000)
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|e| e["type"] == "agent.completed")
            .count(),
        1,
        "one durable terminal result"
    );
    #[cfg(target_os = "linux")]
    if let Some(pid) = fake.marker("transport.pid") {
        let proc = std::path::PathBuf::from(format!("/proc/{}", pid.trim()));
        for _ in 0..100 {
            if !proc.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert!(!proc.exists(), "overflowing vendor process was not reaped");
    }
}

#[tokio::test]
async fn partial_provider_frame_survives_cancellation_poll_boundaries() {
    run_case("fragmented", "completed", "complete fragmented reply").await;
}

#[tokio::test]
async fn oversized_unterminated_provider_frame_fails_without_waiting_for_stall_timeout() {
    run_case("oversized_stdout", "failed", "exceeded").await;
}

#[tokio::test]
async fn discovery_probes_also_reject_unterminated_oversized_frames() {
    let root = tempfile::tempdir().unwrap();
    let fake = root.path().join("oversized-probe");
    fs::write(
        &fake,
        "#!/usr/bin/env python3\nimport sys,time\nsys.stdin.readline()\nsys.stdout.write('x'*4_000_001)\nsys.stdout.flush()\ntime.sleep(120)\n",
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let codex = tokio::time::timeout(
        Duration::from_secs(5),
        codex_probe::probe(&fake, None, Duration::from_secs(30)),
    )
    .await
    .expect("Codex must reject the frame before the probe deadline")
    .unwrap_err();
    assert!(codex.to_string().contains("exceeded"), "{codex}");
    let acp = tokio::time::timeout(
        Duration::from_secs(5),
        acp_probe::probe(&fake, &["acp"], root.path(), None, Duration::from_secs(30)),
    )
    .await
    .expect("ACP must reject the frame before the probe deadline")
    .unwrap_err();
    assert!(acp.to_string().contains("exceeded"), "{acp}");
}

#[tokio::test]
async fn ordinary_acp_discovery_completes_without_a_stderr_sign_in_channel() {
    let root = tempfile::tempdir().unwrap();
    let fake = root.path().join("ordinary-acp");
    fs::write(
        &fake,
        r#"#!/usr/bin/env python3
import json,sys
for raw in sys.stdin:
    message = json.loads(raw)
    if message.get('method') == 'initialize':
        result = {'protocolVersion':1,'authMethods':[], 'agentCapabilities':{}}
    elif message.get('method') == 'session/new':
        result = {'sessionId':'ordinary-session'}
    else:
        continue
    print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}), flush=True)
"#,
    )
    .unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let probe = acp_probe::probe(&fake, &["acp"], root.path(), None, Duration::from_secs(5))
        .await
        .unwrap();
    assert!(probe.session_started);
    assert_eq!(probe.protocol_version, Some(1));
}
