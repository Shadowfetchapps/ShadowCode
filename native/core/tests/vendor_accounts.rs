//! Subscription accounts: official sign-in/out, catalog states, usage
//! persistence, and the vendor runner's safety rules. Every vendor process
//! here is a fake script; no real vendor CLI is started.
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    approvals::ApprovalHub,
    cli_agent::{
        auth,
        catalog::VendorCatalog,
        picker::Availability,
        resolve_vendor,
        runner::{self, LimitReached},
        CliAgentsConfig, LaunchOptions, Vendor,
    },
    config::Config,
    events::TaskEvents,
    paths::AppPaths,
    service::{Request, Service},
    steering::SteerControl,
    store::Store,
};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;
use vendor_support::{cli_agents, eventually, FakeCodex};

fn config(fake: &FakeCodex) -> CliAgentsConfig {
    CliAgentsConfig::from_value(&cli_agents(fake)).unwrap()
}

async fn login_done(catalog: &VendorCatalog, vendor: Vendor) -> Value {
    for _ in 0..400 {
        let status = catalog.logins().status(vendor);
        if !status["done"].is_null() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("login never finished");
}

#[tokio::test]
async fn connect_runs_the_official_login_and_relays_the_url() {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt","login_sleep":0.5}));
    let catalog = Arc::new(VendorCatalog::new());
    let cfg = config(&fake);
    let started = auth::connect(&catalog, Vendor::Codex, &cfg).await.unwrap();
    assert_eq!(started["state"], "started");
    // One login per vendor at a time.
    let again = auth::connect(&catalog, Vendor::Codex, &cfg).await.unwrap();
    assert_eq!(again["state"], "already_running");
    let status = login_done(&catalog, Vendor::Codex).await;
    assert_eq!(status["done"]["ok"], true, "{status}");
    let lines = status["lines"].as_array().unwrap();
    let url_line = lines
        .iter()
        .find(|l| !l["url"].is_null())
        .expect("the printed URL is relayed");
    assert_eq!(
        url_line["url"],
        "https://auth.example.invalid/oauth/authorize?client_id=fake&code_challenge=abc123&state=xyz"
    );
    assert!(fake.marker("login_ran").unwrap().starts_with("login"));
    // Signed in: the re-probe after login shows Ready.
    assert_eq!(status["done"]["availability"], "ready");
    // Antigravity without its agent server installed: Connect says to
    // install it first.
    std::env::set_var(
        "SHADOWCODE_ANTIGRAVITY_HOME",
        tempfile::tempdir().unwrap().keep(),
    );
    let agy = auth::connect(&catalog, Vendor::Antigravity, &cfg).await;
    assert!(format!("{:#}", agy.unwrap_err()).contains("Install the Antigravity agent"));
}

#[tokio::test]
async fn connect_can_be_cancelled_and_times_out() {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":null,"login_sleep":30}));
    let catalog = Arc::new(VendorCatalog::new());
    let cfg = config(&fake);
    auth::connect(&catalog, Vendor::Codex, &cfg).await.unwrap();
    eventually(
        || catalog.logins().running(Vendor::Codex).then_some(()),
        "login start",
    )
    .await;
    assert!(catalog.logins().cancel(Vendor::Codex));
    let cancelled = login_done(&catalog, Vendor::Codex).await;
    assert_eq!(cancelled["done"]["ok"], false);
    assert!(cancelled["done"]["detail"]
        .as_str()
        .unwrap()
        .contains("cancelled"));
    assert!(!catalog.logins().cancel(Vendor::Codex));

    let started = std::time::Instant::now();
    auth::connect_with_timeout(&catalog, Vendor::Codex, &cfg, Duration::from_secs(1))
        .await
        .unwrap();
    let timed_out = login_done(&catalog, Vendor::Codex).await;
    assert!(timed_out["done"]["detail"]
        .as_str()
        .unwrap()
        .contains("timed out"));
    assert!(started.elapsed() < Duration::from_secs(15));
    // Still signed out: Sign in, never Ready.
    assert_eq!(timed_out["done"]["availability"], "sign_in");
}

#[tokio::test]
async fn catalog_states_are_sign_in_setup_required_and_api_key() {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":null}));
    let catalog = VendorCatalog::new();
    let cfg = config(&fake);
    // Expired / signed-out login: Sign in, even though the binary exists.
    let expired = catalog.refresh(Vendor::Codex, &cfg, true).await;
    assert_eq!(expired.availability, Availability::SignIn);
    assert_eq!(expired.availability.label(), "Sign in");
    // Missing runtime: Setup required.
    let claude = catalog.refresh(Vendor::Claude, &cfg, true).await;
    assert_eq!(claude.availability, Availability::SetupRequired);
    // API-key login: labelled, billed per token, never plan usage.
    fake.configure(json!({"auth":"apiKey"}));
    catalog.clear(Vendor::Codex).await;
    let status = catalog.refresh(Vendor::Codex, &cfg, true).await;
    assert_eq!(status.availability, Availability::Ready);
    assert!(status.api_key_login());
    let usage = status.usage_for("default", shadowcode_core::now());
    assert_eq!(usage.label, "API key login · billed per token");
    assert!(usage.remaining_percent.is_none());
    let rows = catalog.picker_rows(&cfg, false).await;
    let codex: Vec<_> = rows.iter().filter(|r| r.provider == "cli:codex").collect();
    assert!(!codex.is_empty());
    assert!(codex
        .iter()
        .all(|r| r.subtitle == "API key login · billed per token"));
    assert!(status.to_doctor_json()["billing"] == "api_key");
}

#[tokio::test]
async fn codex_failed_probe_does_not_label_an_api_key_login_as_subscription() {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"apiKey","exit_on_start":true}));
    let catalog = VendorCatalog::new();
    let cfg = config(&fake);
    let status = catalog.refresh(Vendor::Codex, &cfg, true).await;
    // The documented login-status fallback establishes authentication only.
    // Losing the structured account mode must not claim subscription billing.
    assert_eq!(status.availability, Availability::Ready);
    assert!(status.error.is_some());
    assert_eq!(status.to_doctor_json()["billing"], "unknown");
    let usage = status.usage_for("default", shadowcode_core::now());
    assert!(usage.remaining_percent.is_none());
    assert!(usage
        .detail
        .iter()
        .any(|line| line.contains("API charges may apply")));
    let rows = catalog.picker_rows(&cfg, false).await;
    let codex: Vec<_> = rows
        .iter()
        .filter(|row| row.provider == "cli:codex")
        .collect();
    assert!(!codex.is_empty());
    assert!(codex
        .iter()
        .all(|row| row.subtitle == "Cloud · billing unverified"));
    assert!(codex
        .iter()
        .all(|row| row.to_json()["billing"] == "unknown"));
    assert!(codex
        .iter()
        .all(|row| row.availability == Availability::Ready));
    assert!(
        fake.marker("exec_ran").is_none(),
        "a catalog probe cannot send a turn"
    );
}

#[tokio::test]
async fn codex_failed_probe_does_not_reuse_previous_subscription_quota_for_unknown_login() {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt","used":42}));
    let catalog = VendorCatalog::new();
    let cfg = config(&fake);
    let original = catalog.refresh(Vendor::Codex, &cfg, true).await;
    assert_eq!(original.to_doctor_json()["billing"], "subscription");
    assert_eq!(
        original
            .usage_for("default", shadowcode_core::now())
            .remaining_percent,
        Some(58.0)
    );
    fake.configure(json!({"auth":"apiKey","exit_on_start":true}));
    let fallback = catalog.refresh(Vendor::Codex, &cfg, true).await;
    assert_eq!(fallback.availability, Availability::Ready);
    assert_eq!(fallback.to_doctor_json()["billing"], "unknown");
    let usage = fallback.usage_for("default", shadowcode_core::now());
    assert!(
        usage.remaining_percent.is_none(),
        "old account quota is not the unknown login's allowance"
    );
    assert!(usage.windows.is_empty());
    assert!(!usage.limit_reached);
    let rows = catalog.picker_rows(&cfg, false).await;
    assert!(rows
        .iter()
        .filter(|row| row.provider == "cli:codex")
        .all(|row| row.subtitle == "Cloud · billing unverified"
            && row.usage.remaining_percent.is_none()
            && row.to_json()["billing"] == "unknown"));

    // Successful structured probes still preserve the two known billing routes.
    catalog.clear(Vendor::Codex).await;
    fake.configure(json!({"auth":"apiKey"}));
    let api_key = catalog.refresh(Vendor::Codex, &cfg, true).await;
    assert!(api_key.api_key_login());
    assert_eq!(api_key.to_doctor_json()["billing"], "api_key");
    assert_eq!(
        api_key.usage_for("default", shadowcode_core::now()).label,
        "API key login · billed per token"
    );
    catalog.clear(Vendor::Codex).await;
    fake.configure(json!({"auth":"chatgpt","used":12}));
    let subscription = catalog.refresh(Vendor::Codex, &cfg, true).await;
    assert_eq!(subscription.to_doctor_json()["billing"], "subscription");
    assert_eq!(
        subscription
            .usage_for("default", shadowcode_core::now())
            .remaining_percent,
        Some(88.0)
    );
    assert!(fake.marker("exec_ran").is_none());
}

#[tokio::test]
async fn replacing_vendor_binary_refreshes_image_capabilities_before_cache_expiry() {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt","images":true}));
    let catalog = VendorCatalog::new();
    let cfg = config(&fake);
    let initial = catalog.refresh(Vendor::Codex, &cfg, false).await;
    assert_eq!(initial.availability, Availability::Ready);
    assert!(initial.models.iter().all(|model| model.vision));

    fake.configure(json!({"auth":"chatgpt","images":false}));
    assert!(
        catalog
            .refresh(Vendor::Codex, &cfg, false)
            .await
            .models
            .iter()
            .all(|model| model.vision),
        "unchanged binaries keep the bounded cache"
    );

    let binary = fake.dir.join("codex");
    let replacement = fake.dir.join("codex.next");
    fs::write(&replacement, fs::read(&binary).unwrap()).unwrap();
    fs::set_permissions(&replacement, fs::metadata(&binary).unwrap().permissions()).unwrap();
    fs::rename(&replacement, &binary).unwrap();
    let refreshed = catalog.refresh(Vendor::Codex, &cfg, false).await;
    assert_eq!(refreshed.availability, Availability::Ready);
    assert!(refreshed.models.iter().all(|model| !model.vision));
    let rows = catalog.picker_rows(&cfg, false).await;
    assert!(rows
        .iter()
        .filter(|row| row.provider == "cli:codex")
        .all(|row| !row.vision));
}

#[tokio::test]
async fn codex_usage_pools_persist_go_stale_and_reset_on_account_switch() {
    let root = tempfile::tempdir().unwrap();
    let fake = FakeCodex::new(
        root.path(),
        json!({"auth":"chatgpt","email":"a@example.invalid","used":40}),
    );
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let store = Arc::new(Store::open(&paths.database()).unwrap());
    let (sender, _) = tokio::sync::broadcast::channel(16);
    let cfg = config(&fake);
    let catalog = VendorCatalog::with_store(store.clone(), sender.clone());
    let status = catalog.refresh(Vendor::Codex, &cfg, false).await;
    assert_eq!(status.availability, Availability::Ready);
    let now = shadowcode_core::now();
    let shared = status.usage_for("gpt-6-astra", now);
    assert_eq!(shared.state, "ok");
    assert_eq!(shared.remaining_percent, Some(60.0));
    assert!(shared.pool_shared);
    let dedicated = status.usage_for("gpt-5.6-luna", now);
    assert_eq!(dedicated.pool.as_deref(), Some("gpt-reserve"));
    assert_eq!(dedicated.remaining_percent, Some(95.0));
    let rows = store.usage_snapshots().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account, "a@example.invalid");
    // A fresh catalog (app restart) shows the persisted numbers as
    // "Last checked …" before any probe.
    let restarted = VendorCatalog::with_store(store.clone(), sender.clone());
    let cached = restarted.status_cached_json().await;
    assert_eq!(cached["codex"]["usage"]["state"], "stale");
    assert!(cached["codex"]["usage"]["label"]
        .as_str()
        .unwrap()
        .contains("Last checked"));
    assert_eq!(cached["codex"]["usage"]["remaining_percent"], 60.0);
    assert_eq!(cached["cursor"]["usage"]["state"], "unavailable");
    // Account switch: the old account's numbers are dropped.
    fake.configure(json!({"auth":"chatgpt","email":"b@example.invalid","used":10}));
    restarted.clear(Vendor::Codex).await;
    let switched = restarted.refresh(Vendor::Codex, &cfg, true).await;
    assert_eq!(
        switched.usage_for("default", now).remaining_percent,
        Some(90.0)
    );
    let rows = store.usage_snapshots().unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].account, "b@example.invalid");
    // Provider reports nothing: unavailable, never an invented number.
    fake.configure(json!({"auth":"chatgpt","email":"b@example.invalid","rate_error":true}));
    restarted.clear(Vendor::Codex).await;
    let missing = restarted.refresh(Vendor::Codex, &cfg, true).await;
    let usage = missing.usage_for("default", now);
    assert_eq!(usage.state, "unavailable");
    assert!(usage.remaining_percent.is_none());
    // Signed out: stored plan numbers are dropped.
    fake.configure(json!({"auth":null}));
    restarted.clear(Vendor::Codex).await;
    restarted.refresh(Vendor::Codex, &cfg, true).await;
    assert!(store.usage_snapshots().unwrap().is_empty());
}

async fn call(service: &Service, method: &str, path: &str, body: Value) -> Value {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn disconnect_needs_confirmation_and_clears_usage_and_native_sessions() {
    // Never touch the real Antigravity profile from a test.
    std::env::set_var(
        "SHADOWCODE_ANTIGRAVITY_HOME",
        tempfile::tempdir().unwrap().keep(),
    );
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt"}));
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"cli_agents": cli_agents(&fake)})).unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let store = service.engine.store();
    let refreshed = call(&service, "POST", "/api/accounts/codex/refresh", json!({})).await;
    assert_eq!(refreshed["state"], "ready");
    assert_eq!(refreshed["usage"]["state"], "ok");
    assert_eq!(store.usage_snapshots().unwrap().len(), 1);
    let session = store.create_session(&project, "cli:codex", "").unwrap();
    let sid = session["id"].as_str().unwrap();
    store
        .set_session_meta(sid, "native_session:codex", "thr-1")
        .unwrap();
    store
        .set_session_meta(sid, "native_session:cursor", "acp-1")
        .unwrap();
    let unconfirmed = call(
        &service,
        "POST",
        "/api/accounts/codex/disconnect",
        json!({}),
    )
    .await;
    assert_eq!(unconfirmed["needs_confirm"], true);
    assert!(unconfirmed["note"]
        .as_str()
        .unwrap()
        .contains("everywhere on this computer"));
    assert!(fake.marker("logout_ran").is_none());
    fake.configure(json!({"auth":null}));
    let done = call(
        &service,
        "POST",
        "/api/accounts/codex/disconnect",
        json!({"confirm":true}),
    )
    .await;
    assert_eq!(done["ok"], true, "{done}");
    assert_eq!(done["ran"], json!(["codex", "logout"]));
    assert!(fake.marker("logout_ran").is_some());
    assert_eq!(done["availability"], "sign_in");
    assert!(store.usage_snapshots().unwrap().is_empty());
    assert!(store
        .session_meta(sid, "native_session:codex")
        .unwrap()
        .is_none());
    // Other vendors' sessions are untouched.
    assert_eq!(
        store
            .session_meta(sid, "native_session:cursor")
            .unwrap()
            .as_deref(),
        Some("acp-1")
    );
    let login = call(&service, "GET", "/api/accounts/codex/login", json!({})).await;
    assert_eq!(login["running"], false);
    let agy = call(
        &service,
        "POST",
        "/api/accounts/antigravity/disconnect",
        json!({"confirm":true}),
    )
    .await;
    // Disconnect deletes ShadowCode's private Antigravity profile only.
    assert_eq!(agy["ok"], true);
    assert!(agy["note"]
        .as_str()
        .unwrap()
        .contains("private Antigravity profile"));
}

struct Run {
    _root: tempfile::TempDir,
    fake: FakeCodex,
    workspace: std::path::PathBuf,
    events: TaskEvents,
    receiver: tokio::sync::broadcast::Receiver<Value>,
}

fn run_setup(config: Value) -> Run {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("project");
    fs::create_dir(&workspace).unwrap();
    let fake = FakeCodex::new(root.path(), config);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let store = Arc::new(Store::open(&paths.database()).unwrap());
    let session = store.create_session(&workspace, "cli:codex", "").unwrap();
    let session_id = session["id"].as_str().unwrap().to_owned();
    let task_id = store.create_task(&session_id, "t").unwrap();
    let (sender, receiver) = tokio::sync::broadcast::channel(256);
    Run {
        _root: root,
        fake,
        workspace,
        events: TaskEvents {
            store,
            session_id,
            task_id,
            sender,
        },
        receiver,
    }
}

async fn run_fake(
    run: &Run,
    read_only: bool,
    approvals_required: bool,
    catalog: Option<Arc<VendorCatalog>>,
) -> anyhow::Result<runner::RunOutcome> {
    let config = config(&run.fake);
    let approvals = ApprovalHub::default();
    let steer = SteerControl::default();
    tokio::time::timeout(
        Duration::from_secs(30),
        runner::run(runner::Request {
            vendor: Vendor::Codex,
            options: LaunchOptions {
                binary: run.fake.binary(),
                workspace: run.workspace.clone(),
                model: "default".into(),
                read_only,
                resume: None,
                effort: None,
                mcp_servers: Vec::new(),
            },
            config: &config,
            prompt: "hello".into(),
            images: Vec::new(),
            session_id: run.events.session_id.clone(),
            task_id: run.events.task_id.clone(),
            job_id: "job".into(),
            events: &run.events,
            approvals: &approvals,
            cancel: CancellationToken::new(),
            steer: &steer,
            approvals_required,
            catalog,
        }),
    )
    .await
    .expect("vendor run timed out")
}

fn kinds(receiver: &mut tokio::sync::broadcast::Receiver<Value>) -> Vec<Value> {
    let mut out = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        out.push(event);
    }
    out
}

#[tokio::test]
async fn subscription_runs_never_see_api_keys_and_count_per_turn_tokens() {
    // Values are fake; they only prove the variables never reach the CLI.
    for name in shadowcode_core::cli_agent::API_KEY_VARIABLES {
        std::env::set_var(name, "sk-test-not-a-real-key-000000000000");
    }
    std::env::set_var("SHADOWCODE_FAKE_MARKER", "inherited");
    let mut run = run_setup(json!({"auth":"chatgpt","turn":"env"}));
    let outcome = run_fake(&run, false, true, None).await.unwrap();
    assert!(
        outcome
            .text
            .contains("API keys visible: none; marker=inherited"),
        "{}",
        outcome.text
    );
    // Two tokenUsage notifications with last={10,5}: 20/10, not the
    // cumulative totals (3000/1500).
    assert!(outcome.usage_reported);
    assert_eq!(outcome.usage.prompt_tokens, 20);
    assert_eq!(outcome.usage.completion_tokens, 10);
    let events = kinds(&mut run.receiver);
    let usage = events
        .iter()
        .find(|e| e["type"] == "usage.updated")
        .expect("rate-limit push becomes usage.updated");
    assert_eq!(usage["payload"]["vendor"], "codex");
    assert_eq!(usage["payload"]["usage"]["remaining_percent"], 59.0);
    // Sign-in children get the same scrubbing.
    let catalog = Arc::new(VendorCatalog::new());
    auth::connect(&catalog, Vendor::Codex, &config(&run.fake))
        .await
        .unwrap();
    login_done(&catalog, Vendor::Codex).await;
    assert!(run
        .fake
        .marker("login_ran")
        .unwrap()
        .trim_end()
        .ends_with("keys="));
}

#[tokio::test]
async fn plan_limit_stops_the_turn_without_retry() {
    let mut run = run_setup(json!({"auth":"chatgpt","turn":"limit"}));
    let started = std::time::Instant::now();
    let error = run_fake(&run, false, false, None).await.unwrap_err();
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "the turn is stopped, not waited out"
    );
    let limit = error
        .downcast_ref::<LimitReached>()
        .expect("typed limit error");
    assert_eq!(limit.vendor, Vendor::Codex);
    assert!(limit.usage.limit_reached);
    let events = kinds(&mut run.receiver);
    assert!(events.iter().any(|e| e["type"] == "limit.reached"));
    // No second attempt of any kind.
    assert_eq!(run.fake.marker("prompts.log").unwrap().lines().count(), 1);
    assert!(run.fake.marker("exec_ran").is_none());
}

#[tokio::test]
async fn exec_fallback_only_before_a_turn_and_never_under_approvals() {
    // The turn started, then failed with "No such file…": no rerun.
    let run = run_setup(json!({"auth":"chatgpt","turn":"fail"}));
    let error = run_fake(&run, false, false, None).await.unwrap_err();
    assert!(format!("{error:#}").contains("No such file"));
    assert!(run.fake.marker("exec_ran").is_none());
    assert_eq!(run.fake.marker("prompts.log").unwrap().lines().count(), 1);
    // app-server rejects initialize while shell commands require approval: refused.
    let refused = run_setup(json!({"reject_initialize":true}));
    let error = run_fake(&refused, false, true, None).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("require approval"),
        "{error:#}"
    );
    assert!(refused.fake.marker("exec_ran").is_none());
    // Same failure without approvals: the documented exec path runs once.
    let allowed = run_setup(json!({"reject_initialize":true}));
    let outcome = run_fake(&allowed, false, false, None).await.unwrap();
    assert!(outcome.text.contains("exec answer"));
    assert!(allowed.fake.marker("exec_ran").is_some());
}

#[tokio::test]
async fn input_failure_after_thread_readiness_does_not_launch_exec_fallback() {
    let run = run_setup(json!({"auth":"chatgpt","close_stdin_after_thread":true}));
    let error = tokio::time::timeout(Duration::from_secs(2), run_fake(&run, false, false, None))
        .await
        .expect("a broken input pipe must fail promptly")
        .unwrap_err();
    assert!(
        error
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe),
        "{error:#}"
    );
    assert!(run.fake.marker("threads.log").is_some());
    assert!(
        run.fake.marker("exec_ran").is_none(),
        "do not resubmit after a prompt may have started"
    );
}

#[tokio::test]
async fn read_only_tasks_auto_deny_vendor_prompts() {
    let mut run = run_setup(json!({"auth":"chatgpt","turn":"approval"}));
    let outcome = run_fake(&run, true, true, None).await.unwrap();
    assert!(
        outcome.text.contains("decision=decline"),
        "{}",
        outcome.text
    );
    let events = kinds(&mut run.receiver);
    assert!(events.iter().any(|e| e["type"] == "agent.warning"
        && e["payload"]["text"]
            .as_str()
            .unwrap_or("")
            .starts_with("Denied automatically")));
    assert!(!events.iter().any(|e| e["type"] == "approval.requested"));
}

#[test]
fn routing_ids_are_strict_cli_ids() {
    // Bare "grok" is the xAI API preset, never the Grok CLI.
    assert!(resolve_vendor("grok").is_none());
    assert!(resolve_vendor("Claude Code").is_none());
    assert!(resolve_vendor("Codex (vendor agent)").is_none());
    assert!(resolve_vendor("cli:unknown").is_none());
    assert!(resolve_vendor("cli:codex:").is_none());
    let cursor =
        resolve_vendor("cli:cursor:gpt-5.5[context=272k,reasoning=medium,fast=false]").unwrap();
    assert_eq!(cursor.provider, "cli:cursor");
    assert_eq!(
        cursor.name,
        "gpt-5.5[context=272k,reasoning=medium,fast=false]"
    );
    // The exact picker id survives into routing records.
    assert_eq!(
        cursor.default,
        "cli:cursor:gpt-5.5[context=272k,reasoning=medium,fast=false]"
    );
    assert_eq!(resolve_vendor("cli:codex").unwrap().default, "cli:codex");
}

#[test]
fn migration_24_to_25_backs_up_and_keeps_data() {
    let root = tempfile::tempdir().unwrap();
    let db = root.path().join("state").join("shadow.db");
    let project = root.path().join("p");
    fs::create_dir_all(&project).unwrap();
    let sid = {
        let store = Store::open(&db).unwrap();
        let session = store.create_session(&project, "cli:codex", "kept").unwrap();
        session["id"].as_str().unwrap().to_owned()
    };
    // Rewind to a 0.27 database: schema 24 without the usage table.
    {
        let connection = rusqlite::Connection::open(&db).unwrap();
        connection
            .execute_batch("DROP TABLE usage_snapshots; PRAGMA user_version=24;")
            .unwrap();
    }
    let backups = |dir: &Path| {
        fs::read_dir(dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                name.contains("pre-native-") && name.ends_with(".sqlite")
            })
            .map(|e| e.path())
            .collect::<Vec<_>>()
    };
    assert!(backups(db.parent().unwrap()).is_empty());
    let store = Store::open(&db).unwrap();
    assert_eq!(store.session(&sid).unwrap().unwrap()["title"], "kept");
    store
        .upsert_usage_snapshot(&shadowcode_core::store::UsageRow {
            vendor: "codex".into(),
            account: "a".into(),
            pool: "*".into(),
            fetched_at: 1.0,
            payload: json!({"rateLimits":{}}),
        })
        .unwrap();
    assert_eq!(store.usage_snapshots().unwrap().len(), 1);
    drop(store);
    let saved = backups(db.parent().unwrap());
    assert_eq!(saved.len(), 1, "one backup before migrating");
    let backup = rusqlite::Connection::open(&saved[0]).unwrap();
    let version: i64 = backup
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 24);
    let kept: String = backup
        .query_row("SELECT title FROM sessions WHERE id=?", [&sid], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kept, "kept");
    let migrated = rusqlite::Connection::open(&db).unwrap();
    let version: i64 = migrated
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, shadowcode_core::store::SCHEMA_VERSION);
    drop(migrated);
    // Reopening a current database neither migrates nor backs up again.
    drop(Store::open(&db).unwrap());
    assert_eq!(backups(db.parent().unwrap()).len(), 1);
}
