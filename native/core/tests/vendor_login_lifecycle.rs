//! Private fake login/account-check lifecycle barriers; no real authentication.
#![cfg(target_os = "linux")]
mod vendor_support;

use serde_json::{json, Value};
use shadowcode_core::{
    cli_agent::{auth, CliAgentsConfig, Vendor},
    config::Config,
    paths::AppPaths,
    service::Service,
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};
use vendor_support::{cli_agents, FakeCodex};

#[derive(Clone, Copy, Debug)]
enum Case {
    CancelCheck,
    DeadlineCheck,
    CancelChild,
    ShutdownCheck,
    ShutdownVersion,
    ShutdownFallback,
    Ready,
}

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = fs::write(self.0.join("release_login"), b"");
        let _ = fs::write(self.0.join("release_probe"), b"");
    }
}
async fn until(mut ready: impl FnMut() -> bool, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, async {
        while !ready() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok()
}
fn pid_from(file: &Path) -> Option<u32> {
    fs::read_to_string(file).ok()?.trim().parse().ok()
}
fn process_snapshot(pid: u32) -> Value {
    let proc = PathBuf::from(format!("/proc/{pid}"));
    match fs::read_to_string(proc.join("stat")) {
        Ok(stat) => {
            let fields = stat
                .rsplit_once(')')
                .map(|(_, tail)| tail.split_whitespace().collect::<Vec<_>>())
                .unwrap_or_default();
            json!({"pid":pid,"present":true,"state":fields.first(),"start_ticks":fields.get(19),
                "exe":fs::read_link(proc.join("exe")).ok().map(|p|p.display().to_string())})
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            json!({"pid":pid,"present":false})
        }
        Err(error) => json!({"pid":pid,"observation_error":error.to_string()}),
    }
}
fn observed_identity(file: &Path) -> Option<Value> {
    let observed = process_snapshot(pid_from(file)?);
    (observed["present"] == true
        && observed["start_ticks"].is_string()
        && observed["exe"].is_string())
    .then_some(observed)
}
fn same_owned_process(expected: &Value, current: &Value) -> bool {
    current["pid"] == expected["pid"]
        && current["present"] == true
        && current["start_ticks"] == expected["start_ticks"]
}
fn ownership_gone(expected: &Value, current: &Value) -> bool {
    current["present"] == false
        || (current["start_ticks"].is_string() && !same_owned_process(expected, current))
}
fn process_gone(file: &Path) -> bool {
    pid_from(file).is_some_and(|pid| process_snapshot(pid)["present"] == false)
}
async fn exercise(case: Case) {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(
        root.path(),
        json!({"auth":"chatgpt", "hold_login":matches!(case, Case::CancelChild), "hold_probe":!matches!(case,Case::Ready), "hold_version":matches!(case,Case::ShutdownVersion), "hold_status":matches!(case,Case::ShutdownFallback), "reject_initialize":matches!(case,Case::ShutdownFallback)}),
    );
    let source = fs::read_to_string(fake.binary()).unwrap()
        .replace("if args[:1] == [\"login\"]:\n", "if args[:1] == [\"login\"]:\n    mark('login.pid', str(os.getpid()))\n    mark('login_entered')\n    until = time.monotonic() + 8\n    while C.get('hold_login') and not os.path.exists(os.path.join(HERE, 'release_login')) and time.monotonic() < until: time.sleep(0.005)\n")
        .replace("    elif method == \"account/read\":\n", "    elif method == \"account/read\":\n        mark('probe.pid', str(os.getpid()))\n        mark('probe_entered')\n        until = time.monotonic() + 8\n        while C.get('hold_probe') and not os.path.exists(os.path.join(HERE, 'release_probe')) and time.monotonic() < until: time.sleep(0.005)\n");
    let source = source
        .replace("if \"--version\" in args:\n", "if \"--version\" in args:\n    if C.get('hold_version'):\n        mark('helper.pid', str(os.getpid()))\n        mark('helper_entered')\n        until = time.monotonic() + 8\n        while not os.path.exists(os.path.join(HERE, 'release_probe')) and time.monotonic() < until: time.sleep(0.005)\n")
        .replace("if args[:2] == [\"login\", \"status\"]:\n", "if args[:2] == [\"login\", \"status\"]:\n    if C.get('hold_status'):\n        mark('helper.pid', str(os.getpid()))\n        mark('helper_entered')\n        until = time.monotonic() + 8\n        while not os.path.exists(os.path.join(HERE, 'release_probe')) and time.monotonic() < until: time.sleep(0.005)\n");
    fs::write(fake.binary(), source).unwrap();
    let _release = Release(fake.dir.clone());
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let agents = cli_agents(&fake);
    Config::patch(&paths, json!({"cli_agents":agents})).unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let service = Service::open(paths, Some(project)).unwrap();
    let catalog = service.engine.vendors();
    let config = CliAgentsConfig::from_value(&agents).unwrap();
    let mut events = service.engine.subscribe();
    let timeout = if matches!(case, Case::DeadlineCheck) {
        Duration::from_secs(2)
    } else {
        Duration::from_secs(5)
    };
    auth::connect_with_timeout(&catalog, Vendor::Codex, &config, timeout)
        .await
        .unwrap();
    let barrier = match case {
        Case::CancelChild => "login_entered",
        Case::ShutdownVersion | Case::ShutdownFallback => "helper_entered",
        _ => "probe_entered",
    };
    let entered = until(|| fake.marker(barrier).is_some(), Duration::from_secs(1)).await;
    let owned_pid = fake.dir.join(match case {
        Case::CancelChild => "login.pid",
        Case::ShutdownVersion | Case::ShutdownFallback => "helper.pid",
        _ => "probe.pid",
    });
    let ownership_before = observed_identity(&owned_pid);
    // Pin the PID once; a later CLI probe may overwrite its marker file.
    let owned_number = pid_from(&owned_pid);
    let mut shutdown_observation = None;
    let mut shutdown_returned = true;
    if entered {
        match case {
            Case::CancelCheck | Case::CancelChild => {
                assert!(catalog.logins().cancel(Vendor::Codex));
            }
            Case::ShutdownCheck | Case::ShutdownVersion | Case::ShutdownFallback => {
                shutdown_returned =
                    tokio::time::timeout(Duration::from_secs(2), service.engine.shutdown())
                        .await
                        .is_ok_and(|r| r.is_ok());
                // No intervening await: this is the owned process at the
                // actual shutdown return, not after a cleanup grace interval.
                shutdown_observation = owned_number.map(process_snapshot);
            }
            Case::DeadlineCheck | Case::Ready => {}
        }
    }
    let watchdog = if matches!(case, Case::DeadlineCheck) {
        Duration::from_secs(3)
    } else {
        Duration::from_secs(1)
    };
    let completed_without_release = until(
        || !catalog.logins().status(Vendor::Codex)["done"].is_null(),
        watchdog,
    )
    .await;
    let observed = catalog.logins().status(Vendor::Codex);
    let cached_at_completion = catalog.cached(Vendor::Codex).await;
    let probe_started = fake.marker("probe_entered").is_some();
    let reaped_without_release = until(
        || owned_number.is_some_and(|pid| process_snapshot(pid)["present"] == false),
        Duration::from_millis(500),
    )
    .await;
    eprintln!(
        "{}",
        json!({"case":format!("{case:?}"),"owned_before":ownership_before,
        "immediate_shutdown_return":shutdown_observation,"eventual_gone_500ms":reaped_without_release})
    );
    // Preserve the failing observations but always release/reap the old source.
    fs::write(fake.dir.join("release_login"), b"").unwrap();
    fs::write(fake.dir.join("release_probe"), b"").unwrap();
    assert!(
        until(
            || !catalog.logins().status(Vendor::Codex)["done"].is_null(),
            Duration::from_secs(2)
        )
        .await
    );
    let mut terminal_count = 0;
    while let Ok(event) = events.try_recv() {
        if event["type"] == "account.login.done" {
            terminal_count += 1;
        }
    }
    let after_close_refused = if matches!(
        case,
        Case::ShutdownCheck | Case::ShutdownVersion | Case::ShutdownFallback
    ) {
        let attempt = auth::connect_with_timeout(&catalog, Vendor::Codex, &config, timeout).await;
        if attempt.is_ok() {
            let _ = catalog.logins().cancel(Vendor::Codex);
            assert!(
                until(
                    || !catalog.logins().status(Vendor::Codex)["done"].is_null(),
                    Duration::from_secs(2)
                )
                .await
            );
        }
        attempt.is_err()
    } else {
        true
    };
    service.engine.shutdown().await.unwrap();
    assert!(
        entered,
        "private login/account probe reached its stable barrier"
    );
    assert!(
        completed_without_release,
        "post-login check ignored cancellation/deadline/shutdown: {observed}"
    );
    assert!(
        reaped_without_release,
        "owned process survived cancellation or completed check"
    );
    assert!(shutdown_returned, "engine login drain did not finish");
    if !matches!(case, Case::Ready) {
        assert!(
            ownership_before.is_some(),
            "fixture observed live PID/start/exe before cancellation"
        );
    }
    if matches!(
        case,
        Case::ShutdownCheck | Case::ShutdownVersion | Case::ShutdownFallback
    ) {
        assert!(
            ownership_gone(
                ownership_before.as_ref().unwrap(),
                shutdown_observation.as_ref().unwrap()
            ),
            "shutdown returned before the owned probe was reaped: {shutdown_observation:?}"
        );
    }
    assert!(after_close_refused, "closed engine admitted another login");
    assert_eq!(terminal_count, 1, "one terminal per admitted login");
    assert_eq!(observed["done"]["ok"], matches!(case, Case::Ready));
    if !matches!(case, Case::Ready) {
        assert!(
            cached_at_completion
                .as_ref()
                .is_none_or(|status| status.availability
                    != shadowcode_core::cli_agent::picker::Availability::Ready),
            "interrupted check cannot publish Ready"
        );
    }
    if matches!(case, Case::CancelChild | Case::ShutdownVersion) {
        assert!(
            !probe_started,
            "cancelled login launched a new account check"
        );
    }
    if matches!(case, Case::Ready) {
        assert_eq!(observed["done"]["availability"], "ready");
    } else {
        assert!(observed["done"]["detail"].as_str().unwrap().contains(
            if matches!(case, Case::DeadlineCheck) {
                "timed out"
            } else {
                "cancelled"
            }
        ));
    }
}
#[tokio::test]
async fn cancel_owns_the_post_login_account_check() {
    exercise(Case::CancelCheck).await;
}
#[tokio::test]
async fn original_login_deadline_owns_the_account_check() {
    exercise(Case::DeadlineCheck).await;
}
#[tokio::test]
async fn cancelled_login_does_not_start_an_account_check() {
    exercise(Case::CancelChild).await;
}
#[tokio::test]
async fn engine_shutdown_drains_login_and_closes_admission() {
    exercise(Case::ShutdownCheck).await;
}
#[tokio::test]
async fn ordinary_login_still_confirms_ready_once() {
    exercise(Case::Ready).await;
}

static AG_ENV: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
const AG_FAKE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
root = Path(__file__).parent
(root / 'agent.pid').write_text(str(os.getpid()))
if (root / 'invalid_output').exists() or (root / 'output_eof').exists():
    (root / 'invalid_entered').write_text('entered')
    until = time.monotonic() + 8
    while not (root / 'emit_invalid').exists() and not (root / 'release_login').exists() and time.monotonic() < until: time.sleep(.005)
    if (root / 'invalid_output').exists(): os.write(1, b'\xff\n')
    os.close(1)
    os.close(2)
    until = time.monotonic() + 8
    while not (root / 'release_login').exists() and time.monotonic() < until: time.sleep(.005)
    sys.exit(0)
for raw in sys.stdin:
    message = json.loads(raw)
    if (root / 'authenticated').exists():
        (root / 'probe.pid').write_text(str(os.getpid()))
        (root / 'probe_entered').write_text('entered')
        until = time.monotonic() + 8
        while not (root / 'release_probe').exists() and time.monotonic() < until: time.sleep(.005)
    if message.get('method') == 'initialize': result = {'protocolVersion':1,'authMethods':[],'agentCapabilities':{}}
    elif message.get('method') == 'authenticate':
        if (root / 'hold_auth').exists():
            until = time.monotonic() + 8
            while not (root / 'release_login').exists() and time.monotonic() < until: time.sleep(.005)
        (root / 'authenticated').write_text('fixture only')
        result = {}
    elif message.get('method') == 'session/new': result = {'sessionId':'fixture-session'}
    else: continue
    print(json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result}), flush=True)
"#;

#[derive(Clone, Copy, Debug)]
enum AgCase {
    SetupFailure,
    DroppedCaller,
    CancelCheck,
    InvalidOutput,
    OutputEof,
}
async fn exercise_ag(case: AgCase) {
    use futures_util::FutureExt;
    use shadowcode_core::cli_agent::antigravity_server;
    use std::os::unix::fs::PermissionsExt;
    let _environment = AG_ENV.lock().await;
    let root = tempfile::tempdir().unwrap();
    let old_home = std::env::var_os("SHADOWCODE_ANTIGRAVITY_HOME");
    let home = root.path().join("private-home");
    if matches!(case, AgCase::SetupFailure) {
        fs::write(&home, b"ordinary file blocks profile setup").unwrap();
    } else {
        fs::create_dir(&home).unwrap();
    }
    std::env::set_var("SHADOWCODE_ANTIGRAVITY_HOME", &home);
    let bin = root.path().join("bin");
    fs::create_dir(&bin).unwrap();
    let server = bin.join(antigravity_server::SERVER_FILE);
    fs::write(&server, AG_FAKE).unwrap();
    fs::set_permissions(&server, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(bin.join(antigravity_server::HARNESS_FILE), b"fixture").unwrap();
    if matches!(case, AgCase::DroppedCaller) {
        fs::write(bin.join("hold_auth"), b"").unwrap();
    }
    if matches!(case, AgCase::InvalidOutput | AgCase::OutputEof) {
        fs::write(
            bin.join(if matches!(case, AgCase::InvalidOutput) {
                "invalid_output"
            } else {
                "output_eof"
            }),
            b"",
        )
        .unwrap();
    }
    let _release = Release(bin.clone());
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let service = Service::open(paths, Some(project)).unwrap();
    let catalog = service.engine.vendors();
    let config = CliAgentsConfig {
        antigravity_binary: server.display().to_string(),
        ..Default::default()
    };
    let mut events = service.engine.subscribe();
    let start = auth::connect_with_timeout(
        &catalog,
        Vendor::Antigravity,
        &config,
        Duration::from_secs(5),
    );
    let (setup_failed, transfer) = if matches!(case, AgCase::DroppedCaller) {
        // Poll once and drop a suspended request. Record which branch actually
        // happened; this is not proof of a particular old handshake await.
        match start.now_or_never() {
            Some(result) => (result.is_err(), "returned"),
            None => (false, "caller_dropped_pending"),
        }
    } else {
        (start.await.is_err(), "returned")
    };
    let entered = if matches!(case, AgCase::CancelCheck) {
        until(
            || bin.join("probe_entered").exists(),
            Duration::from_secs(1),
        )
        .await
    } else if !matches!(case, AgCase::SetupFailure) && transfer == "returned" && !setup_failed {
        until(
            || observed_identity(&bin.join("agent.pid")).is_some(),
            Duration::from_secs(1),
        )
        .await
    } else {
        true
    };
    let owned_pid = bin.join(if matches!(case, AgCase::CancelCheck) {
        "probe.pid"
    } else {
        "agent.pid"
    });
    let ownership_before = observed_identity(&owned_pid);
    // Pin the PID once; a later CLI probe may overwrite its marker file.
    let owned_number = pid_from(&owned_pid);
    let failed_output_promptly = if matches!(case, AgCase::InvalidOutput | AgCase::OutputEof) {
        fs::write(bin.join("emit_invalid"), b"").unwrap();
        until(
            || !catalog.logins().running(Vendor::Antigravity),
            Duration::from_secs(1),
        )
        .await
    } else {
        true
    };
    if matches!(
        case,
        AgCase::CancelCheck | AgCase::InvalidOutput | AgCase::OutputEof
    ) && catalog.logins().running(Vendor::Antigravity)
    {
        let _ = catalog.logins().cancel(Vendor::Antigravity);
    }
    let shutdown_finished = tokio::time::timeout(Duration::from_secs(2), service.engine.shutdown())
        .await
        .is_ok_and(|r| r.is_ok());
    let shutdown_observation = owned_number.map(process_snapshot);
    let done_before_release = catalog.logins().status(Vendor::Antigravity);
    let reaped = if bin.join("agent.pid").exists() {
        until(
            || process_gone(&bin.join("agent.pid")),
            Duration::from_secs(1),
        )
        .await
    } else {
        true
    };
    // A before-fix failure is released before asserting and must not leave an
    // owned server alive; a dropped pre-worker request can have no done writer.
    fs::write(bin.join("release_login"), b"").unwrap();
    fs::write(bin.join("release_probe"), b"").unwrap();
    if matches!(
        case,
        AgCase::CancelCheck | AgCase::InvalidOutput | AgCase::OutputEof
    ) {
        let _ = until(
            || !catalog.logins().running(Vendor::Antigravity),
            Duration::from_secs(2),
        )
        .await;
    }
    let mut terminal_count = 0;
    while let Ok(event) = events.try_recv() {
        if event["type"] == "account.login.done" {
            terminal_count += 1;
        }
    }
    let eventually_gone_after_release = if let Some(before) = ownership_before.as_ref() {
        until(
            || owned_number.is_some_and(|pid| ownership_gone(before, &process_snapshot(pid))),
            Duration::from_secs(2),
        )
        .await
    } else {
        true
    };
    let run_dirs = fs::read_dir(home.join("runs"))
        .map(|entries| entries.count())
        .unwrap_or(0);
    drop(catalog);
    drop(service);
    match old_home {
        Some(value) => std::env::set_var("SHADOWCODE_ANTIGRAVITY_HOME", value),
        None => std::env::remove_var("SHADOWCODE_ANTIGRAVITY_HOME"),
    }
    eprintln!(
        "{}",
        json!({"case":format!("{case:?}"),"request_poll":transfer,"owned_before":ownership_before,
        "immediate_shutdown_return":shutdown_observation,"eventual_gone_1s":reaped})
    );
    assert!(
        entered,
        "private ACP reached required owned-process barrier"
    );
    if !matches!(case, AgCase::SetupFailure) && transfer == "returned" && !setup_failed {
        assert!(
            ownership_before.is_some(),
            "transferred worker must have a real observed child identity"
        );
    }
    if let Some(before) = ownership_before.as_ref() {
        assert!(
            shutdown_observation
                .as_ref()
                .is_some_and(|current| ownership_gone(before, current)),
            "shutdown returned before the owned ACP process was reaped: {shutdown_observation:?}"
        );
    }
    assert!(
        eventually_gone_after_release,
        "fixture cleanup failed after releasing its own barrier"
    );
    assert!(
        failed_output_promptly,
        "ACP EOF/read error did not terminate promptly without cancellation"
    );
    if matches!(case, AgCase::InvalidOutput | AgCase::OutputEof) {
        let detail = done_before_release["done"]["detail"].as_str().unwrap_or("");
        assert!(
            detail.contains(if matches!(case, AgCase::InvalidOutput) {
                "Could not read Antigravity sign-in response"
            } else {
                "stopped before signing in"
            }),
            "typed stream failure required, not panic/drop fallback: {detail}"
        );
    }
    assert!(
        shutdown_finished && reaped,
        "shutdown owns the admitted ACP process"
    );
    assert!(
        !done_before_release["done"].is_null(),
        "admitted ACP setup/check remained busy: {done_before_release}"
    );
    assert_eq!(done_before_release["done"]["ok"], false);
    assert_eq!(terminal_count, 1);
    assert_eq!(run_dirs, 0, "owned ACP run directory is removed");
    if matches!(case, AgCase::SetupFailure) {
        assert!(setup_failed);
    }
}
#[tokio::test]
async fn antigravity_setup_error_releases_admission() {
    exercise_ag(AgCase::SetupFailure).await;
}
#[tokio::test]
async fn dropped_antigravity_connect_request_cannot_strand_login() {
    exercise_ag(AgCase::DroppedCaller).await;
}
#[tokio::test]
async fn antigravity_final_check_is_cancelled_and_drained() {
    exercise_ag(AgCase::CancelCheck).await;
}

#[tokio::test]
async fn antigravity_invalid_utf8_and_closed_stderr_finish_without_panic() {
    exercise_ag(AgCase::InvalidOutput).await;
}
#[tokio::test]
async fn antigravity_stdout_eof_finishes_without_panic() {
    exercise_ag(AgCase::OutputEof).await;
}

#[tokio::test]
async fn engine_shutdown_reaps_post_login_version_helper() {
    exercise(Case::ShutdownVersion).await;
}
#[tokio::test]
async fn engine_shutdown_reaps_post_login_status_fallback() {
    exercise(Case::ShutdownFallback).await;
}

/// Cancelling a sign-in whose CLI is a wrapper (like the npm `codex` script,
/// which runs the real program as its child and passes SIGTERM on) also
/// stops the real login program, instead of leaving it listening.
#[tokio::test]
async fn cancelled_login_stops_the_program_behind_a_wrapper() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt"}));
    let real = fake.dir.join("codex-real");
    fs::rename(fake.dir.join("codex"), &real).unwrap();
    let pidfile = fake.dir.join("login-child.pid");
    fs::write(
        fake.dir.join("codex"),
        format!(
            "#!/bin/bash\nif [ \"$1\" = login ] && [ \"$2\" != status ]; then\n  sleep 600 &\n  child=$!\n  echo $child > '{}'\n  trap 'kill -TERM $child 2>/dev/null; wait $child; exit 143' TERM INT HUP\n  wait $child\n  exit 0\nfi\nexec '{}' \"$@\"\n",
            pidfile.display(),
            real.display()
        ),
    )
    .unwrap();
    fs::set_permissions(fake.dir.join("codex"), fs::Permissions::from_mode(0o755)).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let agents = cli_agents(&fake);
    Config::patch(&paths, json!({"cli_agents":agents})).unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let service = Service::open(paths, Some(project)).unwrap();
    let catalog = service.engine.vendors();
    let config = CliAgentsConfig::from_value(&agents).unwrap();
    auth::connect_with_timeout(&catalog, Vendor::Codex, &config, Duration::from_secs(30))
        .await
        .unwrap();
    assert!(
        until(|| pid_from(&pidfile).is_some(), Duration::from_secs(10)).await,
        "the wrapped login program started"
    );
    let pid = pid_from(&pidfile).unwrap();
    let before = observed_identity(&pidfile).expect("login program running");
    assert!(catalog.logins().cancel(Vendor::Codex));
    assert!(
        until(
            || ownership_gone(&before, &process_snapshot(pid)),
            Duration::from_secs(8)
        )
        .await,
        "the login program behind the wrapper survived cancellation"
    );
    assert!(
        until(
            || !catalog.logins().status(Vendor::Codex)["done"].is_null(),
            Duration::from_secs(8)
        )
        .await
    );
    service.engine.shutdown().await.unwrap();
}
