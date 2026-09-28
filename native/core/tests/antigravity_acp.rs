//! Antigravity through a fake `agy_acp_server.par` that behaves like Google's
//! ACP server: without a stored sign-in it prints a Google sign-in link on
//! stderr and waits in `authenticate`; with one it answers `session/new` with
//! a `model` config option. It records the launch environment. Nothing here
//! starts the real server.
use serde_json::Value;
use shadowcode_core::{
    approvals::ApprovalHub,
    cli_agent::{
        antigravity_server, auth, catalog::VendorCatalog, picker::Availability, runner,
        CliAgentsConfig, LaunchOptions, Vendor,
    },
    events::TaskEvents,
    paths::AppPaths,
    steering::SteerControl,
    store::Store,
};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
HOME = os.environ.get("GEMINI_HOME", "")
TOKEN = os.path.join(HOME, "antigravity-acp", "oauth_token.json")
with open(os.path.join(os.path.dirname(os.path.abspath(__file__)), "launches.jsonl"), "a") as f:
    f.write(json.dumps({"argv": sys.argv[1:], "browser": os.environ.get("BROWSER"),
        "gemini_home": HOME, "tmpdir": os.environ.get("TMPDIR"),
        "tmp_exists": os.path.isdir(os.environ.get("TMPDIR", "/nonexistent")),
        "harness": os.environ.get("ANTIGRAVITY_HARNESS_PATH"),
        "file_storage": os.environ.get("AGY_ACP_FORCE_FILE_STORAGE"),
        "google_key": "GOOGLE_API_KEY" in os.environ or "GEMINI_API_KEY" in os.environ}) + "\n")
def send(obj):
    sys.stdout.write(json.dumps(obj) + "\n"); sys.stdout.flush()
for line in sys.stdin:
    msg = json.loads(line)
    method, rid = msg.get("method"), msg.get("id")
    if method == "initialize":
        send({"jsonrpc": "2.0", "id": rid, "result": {"protocolVersion": 1,
            "agentCapabilities": {"loadSession": True, "promptCapabilities": {"image": True}},
            "authMethods": [{"id": "oauth-personal"}, {"id": "gemini-api-key"}],
            "agentInfo": {"name": "antigravity-acp", "version": "1.2.1-fake"}}})
    elif method == "authenticate":
        if os.path.exists(TOKEN):
            send({"jsonrpc": "2.0", "id": rid, "result": {}})
            continue
        sys.stderr.write("Open the following link to authenticate the ACP server: https://accounts.google.com/o/oauth2/v2/auth?response_type=code&client_id=fake&state=s\n")
        sys.stderr.flush()
        if os.environ.get("BROWSER") == "true":
            time.sleep(60)  # waits for a callback that never comes
        else:
            time.sleep(0.3)  # the user signs in in the browser
            os.makedirs(os.path.dirname(TOKEN), exist_ok=True)
            open(TOKEN, "w").write("{}")
            send({"jsonrpc": "2.0", "id": rid, "result": {}})
    elif method == "session/new":
        send({"jsonrpc": "2.0", "id": rid, "result": {"sessionId": "agy-sess-1", "configOptions": [
            {"id": "model", "type": "select", "category": "model", "currentValue": "gemini-fast",
             "options": [{"value": "gemini-fast", "name": "Gemini Fast"}, {"value": "gemini-pro", "name": "Gemini Pro"}]}]}})
    elif method == "session/set_config_option":
        send({"jsonrpc": "2.0", "id": rid, "result": {}})
    elif method == "session/prompt":
        sid = msg["params"]["sessionId"]
        send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid,
            "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "OK from Antigravity"}}}})
        send({"jsonrpc": "2.0", "id": rid, "result": {"stopReason": "end_turn"}})
"#;

fn launches(dir: &Path) -> Vec<Value> {
    fs::read_to_string(dir.join("launches.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn install_sign_in_models_run_and_sign_out_through_the_acp_server() {
    let root = tempfile::tempdir().unwrap();
    // ShadowCode's Antigravity home (install, profile, run dirs) for this test.
    let home = root.path().join("agy-home");
    std::env::set_var("SHADOWCODE_ANTIGRAVITY_HOME", &home);
    std::env::set_var("GOOGLE_API_KEY", "must-not-leak");

    // Not installed yet: setup required, and Connect says to install.
    let catalog = Arc::new(VendorCatalog::new());
    let cfg = CliAgentsConfig::default();
    let status = catalog.refresh(Vendor::Antigravity, &cfg, true).await;
    assert_eq!(status.availability, Availability::SetupRequired);
    assert!(status.detail.contains("Install the Antigravity agent"));

    // "Install" the fake server where the managed installation lives.
    let dir = antigravity_server::install_dir();
    fs::create_dir_all(&dir).unwrap();
    let server = dir.join(antigravity_server::SERVER_FILE);
    fs::write(&server, FAKE).unwrap();
    fs::write(dir.join(antigravity_server::HARNESS_FILE), "#!/bin/sh\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(antigravity_server::installation("agy").is_some());
    assert!(
        antigravity_server::installation("/elsewhere/agy").is_none(),
        "a custom setting never falls back to the managed install"
    );

    // Temp directories a crash left behind: a stale one is removed at the
    // next launch, a recent one (another running launch) is kept.
    let stale = home.join("runs/left-by-a-crash");
    let recent = home.join("runs/still-running");
    fs::create_dir_all(stale.join("tmp")).unwrap();
    fs::create_dir_all(&recent).unwrap();
    let aged = std::process::Command::new("touch")
        .args(["-d", "3 days ago"])
        .arg(&stale)
        .status()
        .unwrap();
    assert!(aged.success());

    // Installed, not signed in: the status probe sees the printed link and
    // stops; no browser is opened.
    let status = catalog.refresh(Vendor::Antigravity, &cfg, true).await;
    assert!(
        !stale.exists(),
        "a crash's leftover temp directory is removed"
    );
    assert!(recent.exists());
    assert_eq!(
        status.availability,
        Availability::SignIn,
        "{}",
        status.detail
    );
    let probe = launches(&dir).pop().unwrap();
    assert_eq!(probe["browser"], "true");
    assert_eq!(probe["file_storage"], "1");
    assert_eq!(probe["google_key"], false, "Google credentials are removed");
    assert_eq!(probe["argv"][0], "--uid=");
    assert!(probe["gemini_home"]
        .as_str()
        .unwrap()
        .starts_with(home.to_str().unwrap()));
    assert_eq!(probe["tmp_exists"], true);
    assert!(
        !Path::new(probe["tmpdir"].as_str().unwrap()).exists(),
        "each launch's temp directory is removed"
    );

    // Connect: the browser may open, the link is relayed, the sign-in lands
    // in the private profile, and the re-probe lists the models.
    let started = auth::connect(&catalog, Vendor::Antigravity, &cfg)
        .await
        .unwrap();
    assert_eq!(started["state"], "started");
    let mut done = Value::Null;
    for _ in 0..200 {
        let login = catalog.logins().status(Vendor::Antigravity);
        if !login["done"].is_null() {
            done = login;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(done["done"]["ok"], true, "{done}");
    assert_eq!(done["done"]["availability"], "ready");
    assert!(done["lines"].as_array().unwrap().iter().any(|l| l["url"]
        .as_str()
        .is_some_and(|u| u.starts_with("https://accounts.google.com/"))));
    let connect_launch = launches(&dir)
        .into_iter()
        .rev()
        .find(|l| l["browser"] != "true")
        .expect("Connect lets the browser open");
    assert!(connect_launch["browser"].is_null());
    let status = catalog.refresh(Vendor::Antigravity, &cfg, true).await;
    assert_eq!(status.availability, Availability::Ready);
    assert!(status.accepts_images);
    assert!(status.asks_approval);
    assert_eq!(status.version.as_deref(), Some("1.2.1-fake"));
    let ids: Vec<&str> = status.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["gemini-fast", "gemini-pro"]);
    assert!(status.models[0].is_default);

    // A task runs through the server with the chosen model.
    let workspace = root.path().join("project");
    fs::create_dir(&workspace).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let store = Arc::new(Store::open(&paths.database()).unwrap());
    let session = store
        .create_session(&workspace, "cli:antigravity", "")
        .unwrap();
    let session_id = session["id"].as_str().unwrap().to_owned();
    let task_id = store.create_task(&session_id, "t").unwrap();
    let (sender, _receiver) = tokio::sync::broadcast::channel(256);
    let events = TaskEvents {
        store,
        session_id: session_id.clone(),
        task_id,
        sender,
    };
    let approvals = ApprovalHub::default();
    let steer = SteerControl::default();
    let run = |model: &str| runner::Request {
        vendor: Vendor::Antigravity,
        options: LaunchOptions {
            binary: server.display().to_string(),
            workspace: workspace.clone(),
            model: model.into(),
            read_only: false,
            resume: None,
            effort: None,
            mcp_servers: Vec::new(),
        },
        config: &cfg,
        prompt: "Say OK".into(),
        images: Vec::new(),
        session_id: session_id.clone(),
        task_id: events.task_id.clone(),
        job_id: "job".into(),
        events: &events,
        approvals: &approvals,
        cancel: CancellationToken::new(),
        steer: &steer,
        approvals_required: true,
        catalog: None,
    };
    let outcome = tokio::time::timeout(Duration::from_secs(30), runner::run(run("gemini-pro")))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(outcome.text.trim(), "OK from Antigravity");
    assert_eq!(outcome.native_session.as_deref(), Some("agy-sess-1"));
    let task_launch = launches(&dir).pop().unwrap();
    assert_eq!(task_launch["browser"], "true", "tasks never open a browser");
    assert!(!Path::new(task_launch["tmpdir"].as_str().unwrap()).exists());

    // Disconnect deletes the private profile; the next task stops at once
    // with a sign-in message instead of waiting for a browser.
    let out = auth::disconnect(&catalog, Vendor::Antigravity, &cfg)
        .await
        .unwrap();
    assert_eq!(out["ok"], true);
    assert!(!antigravity_server::profile_dir()
        .join("antigravity-acp/oauth_token.json")
        .exists());
    let started = std::time::Instant::now();
    let error = tokio::time::timeout(Duration::from_secs(30), runner::run(run("gemini-pro")))
        .await
        .unwrap()
        .unwrap_err()
        .to_string();
    assert!(error.contains("Settings › Accounts"), "{error}");
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "no waiting for a callback"
    );
    std::env::remove_var("GOOGLE_API_KEY");
}

/// Opt-in: install the real archive with ShadowCode's installer, then check
/// the real server's status without a sign-in. Set
/// `SHADOWCODE_AGY_ARCHIVE_URL` to a mirror of the pinned archive (for
/// example a local copy served on loopback); it installs into the real
/// managed location unless `SHADOWCODE_ANTIGRAVITY_HOME` is set.
#[tokio::test]
#[ignore]
async fn live_install_and_status_of_the_real_server() {
    let url = std::env::var("SHADOWCODE_AGY_ARCHIVE_URL")
        .unwrap_or_else(|_| antigravity_server::DOWNLOAD_URL.to_owned());
    if antigravity_server::installation("agy").is_none() {
        antigravity_server::install(
            &url,
            antigravity_server::ARCHIVE_SHA256,
            antigravity_server::ARCHIVE_BYTES,
            &antigravity_server::install_dir(),
        )
        .await
        .unwrap();
    }
    let installed = antigravity_server::installation("agy").expect("installed");
    println!("installed: {}", installed.server.display());
    let catalog = Arc::new(VendorCatalog::new());
    let started = std::time::Instant::now();
    let status = catalog
        .refresh(Vendor::Antigravity, &CliAgentsConfig::default(), true)
        .await;
    println!(
        "status: {:?} · {} · version {:?} · {:.1}s",
        status.availability,
        status.detail,
        status.version,
        started.elapsed().as_secs_f64()
    );
    for model in &status.models {
        println!("  model {} = {}", model.id, model.label);
    }
}
