//! Official-login supervision using an isolated fake CLI, never an account.
#![cfg(target_os = "linux")]

use serde_json::Value;
use shadowcode_core::{
    cli_agent::{auth, catalog::VendorCatalog, CliAgentsConfig, Vendor},
    store::Store,
};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, sync::Arc, time::Duration};

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
from pathlib import Path
root = Path(__file__).parent
args = sys.argv[1:]
if args == ["--version"]:
    print("codex-cli 0.155.0-fake"); sys.exit(0)
if args == ["--help"]:
    print("Commands: app-server login logout exec"); sys.exit(0)
if args == ["login", "status"]:
    print("Not logged in"); sys.exit(1)
if args == ["login"]:
    (root / "parent.pid").write_text(str(os.getpid()))
    child = os.fork()
    if child:
        until = time.monotonic() + 4
        while not (root / "release_parent").exists() and time.monotonic() < until:
            time.sleep(0.005)
        os._exit(0)
    (root / "helper.pid").write_text(str(os.getpid()))
    until = time.monotonic() + 6
    count = 0
    while not (root / "stop_helper").exists() and time.monotonic() < until:
        if (root / "tail_only").exists():
            # Arrive after the parent exits; ordinary tail output stays visible.
            while not (root / "release_tail").exists() and time.monotonic() < until:
                time.sleep(0.005)
            if not (root / "release_tail").exists(): break
            time.sleep(0.080)
            print("https://auth.example.invalid/authorize?client_id=fake", flush=True)
            print("OPENAI_API_KEY=sk-test-fixture-private-123456789", flush=True)
            break
        for stream in (sys.stdout, sys.stderr):
            try: print("fake browser helper output", file=stream, flush=True)
            except BrokenPipeError: (root / "pipes_closed").write_text("closed")
        count += 1
        (root / "heartbeat").write_text(str(count))
        time.sleep(0.010)
    (root / "helper.done").write_text("done")
    os._exit(0)
if args and args[0] == "app-server":
    for raw in sys.stdin:
        m = json.loads(raw)
        if "id" not in m: continue
        method = m.get("method")
        result = {"userAgent":"fixture"}
        if method == "account/read": result = {"account":None,"requiresOpenaiAuth":True}
        if method == "account/rateLimits/read": result = {"rateLimits":None}
        if method == "model/list": result = {"data":[]}
        print(json.dumps({"id":m["id"],"result":result}), flush=True)
    sys.exit(0)
sys.exit(2)
"#;

async fn wait_for(mut condition: impl FnMut() -> bool, timeout: Duration) -> bool {
    tokio::time::timeout(timeout, async {
        while !condition() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .is_ok()
}

struct HelperStop<'a>(&'a Path);
impl Drop for HelperStop<'_> {
    fn drop(&mut self) {
        // The fake helper owns a finite lifetime too. Never signal a browser or
        // an arbitrary PID; this private fixture marker asks it to finish.
        let _ = fs::write(self.0.join("stop_helper"), b"");
        let _ = fs::write(self.0.join("release_parent"), b"");
        let _ = fs::write(self.0.join("release_tail"), b"");
    }
}

#[derive(Clone, Copy)]
enum Case {
    Drain,
    Cancel,
    Deadline,
    Tail,
}

async fn exercise(case: Case) {
    let root = tempfile::tempdir().unwrap();
    let _cleanup = HelperStop(root.path());
    let script = root.path().join("fake-codex");
    fs::write(&script, FAKE).unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o700)).unwrap();
    if matches!(case, Case::Tail) {
        fs::write(root.path().join("tail_only"), b"").unwrap();
    }
    let store = Arc::new(Store::open(&root.path().join("fixture.db")).unwrap());
    let (sender, mut events) = tokio::sync::broadcast::channel::<Value>(1024);
    let catalog = Arc::new(VendorCatalog::with_store(store, sender));
    let config = CliAgentsConfig {
        codex_binary: script.display().to_string(),
        ..Default::default()
    };
    let timeout = if matches!(case, Case::Deadline) {
        Duration::from_millis(750)
    } else {
        Duration::from_secs(5)
    };
    let start = std::time::Instant::now();
    auth::connect_with_timeout(&catalog, Vendor::Codex, &config, timeout)
        .await
        .unwrap();
    assert!(
        wait_for(
            || root.path().join("helper.pid").exists(),
            Duration::from_secs(1)
        )
        .await
    );
    if matches!(case, Case::Deadline) {
        // Make the absolute login deadline fall inside the post-exit drain.
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            start + Duration::from_millis(550),
        ))
        .await;
    }
    fs::write(root.path().join("release_parent"), b"").unwrap();
    let pid: u32 = fs::read_to_string(root.path().join("parent.pid"))
        .unwrap()
        .parse()
        .unwrap();
    // /proc disappearance proves the supervised parent was reaped. Its helper
    // still owns both inherited output pipes while the drain is in progress.
    assert!(
        wait_for(
            || !Path::new(&format!("/proc/{pid}")).exists(),
            Duration::from_millis(150)
        )
        .await
    );
    if matches!(case, Case::Tail) {
        // Start the tail's delay only after reaping is observed, so scheduler
        // delay before that observation cannot make the tail finish early.
        fs::write(root.path().join("release_tail"), b"").unwrap();
    } else {
        assert!(catalog.logins().status(Vendor::Codex)["done"].is_null());
    }
    if matches!(case, Case::Cancel) {
        assert!(catalog.logins().cancel(Vendor::Codex));
    }
    let finished_in_bound = wait_for(
        || !catalog.logins().status(Vendor::Codex)["done"].is_null(),
        Duration::from_secs(1),
    )
    .await;
    let observed = catalog.logins().status(Vendor::Codex);
    let helper_still_alive = !root.path().join("helper.done").exists();
    let pipes_closed = if finished_in_bound && !matches!(case, Case::Tail) {
        wait_for(
            || root.path().join("pipes_closed").exists(),
            Duration::from_millis(500),
        )
        .await
    } else {
        false
    };
    // Clean up even on the before-fix failure, then assert the saved observation.
    fs::write(root.path().join("stop_helper"), b"").unwrap();
    assert!(
        wait_for(
            || root.path().join("helper.done").exists(),
            Duration::from_secs(1)
        )
        .await
    );
    assert!(
        wait_for(
            || !catalog.logins().status(Vendor::Codex)["done"].is_null(),
            Duration::from_secs(2)
        )
        .await
    );
    assert!(
        finished_in_bound,
        "login stayed running after its parent exited: running={}, done={}, lines={}",
        observed["running"],
        observed["done"],
        observed["lines"].as_array().unwrap().len()
    );
    let expected = match case {
        Case::Cancel => "cancelled",
        Case::Deadline => "timed out",
        Case::Drain | Case::Tail => "finished",
    };
    assert_eq!(
        observed["done"]["ok"],
        matches!(case, Case::Drain | Case::Tail),
        "{observed}"
    );
    assert!(
        observed["done"]["detail"]
            .as_str()
            .unwrap()
            .contains(expected),
        "{observed}"
    );
    assert_eq!(
        observed["done"]["availability"],
        if matches!(case, Case::Cancel | Case::Deadline) {
            "unavailable"
        } else {
            "sign_in"
        }
    );
    if matches!(case, Case::Tail) {
        let text = observed["lines"].to_string();
        assert!(
            text.contains("https://auth.example.invalid/authorize?client_id=fake"),
            "{text}"
        );
        assert!(
            !text.contains("sk-test-fixture-private-123456789"),
            "{text}"
        );
    } else {
        assert!(
            helper_still_alive,
            "login completion must not kill the browser-like helper"
        );
        assert!(pipes_closed, "owned output readers must close after login");
    }
    let mut terminal = 0;
    while let Ok(event) = events.try_recv() {
        if event["type"] == "account.login.done" {
            terminal += 1;
        }
    }
    assert_eq!(terminal, 1, "exactly one login outcome is broadcast");
}

#[tokio::test]
async fn post_exit_login_drain_is_bounded_despite_continuous_helper_output() {
    exercise(Case::Drain).await;
}

#[tokio::test]
async fn post_exit_login_drain_honors_cancellation() {
    exercise(Case::Cancel).await;
}

#[tokio::test]
async fn post_exit_login_drain_honors_original_deadline() {
    exercise(Case::Deadline).await;
}

#[tokio::test]
async fn post_exit_login_drain_retains_delayed_redacted_tail() {
    exercise(Case::Tail).await;
}
