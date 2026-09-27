//! Baseline-ready proposal: cached picker options must not await a held vendor.
//! Private subprocesses only; no real login/model inference.
#![cfg(unix)]
mod vendor_support;

use serde_json::{json, Value};
use shadowcode_core::{
    cli_agent::{picker::Availability, CliAgentsConfig, Vendor},
    config::Config,
    gguf::test_support::{write_gguf, V},
    local_engine,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};
use vendor_support::{cli_agents, FakeCodex};

const HELD_ACP: &str = r#"#!/usr/bin/env python3
import json, sys, time
from pathlib import Path
root = Path(__file__).parent
args = sys.argv[1:]
if args == ['--version']:
    print('held-acp 1.0'); sys.exit(0)
if args != ['agent', 'stdio']: sys.exit(2)
for raw in sys.stdin:
    frame = json.loads(raw)
    if frame.get('method') == 'initialize':
        with (root / 'entered').open('a') as marker: marker.write('initialize\n')
        deadline = time.monotonic() + 8
        while not (root / 'release').exists() and time.monotonic() < deadline:
            time.sleep(0.005)
        if not (root / 'release').exists(): sys.exit(3)
        result = {'protocolVersion':1,'authMethods':[],'agentCapabilities':{}}
    elif frame.get('method') == 'session/new':
        result = {'sessionId':'held-fixture'}
    else: sys.exit(2)
    print(json.dumps({'jsonrpc':'2.0','id':frame['id'],'result':result}), flush=True)
"#;

struct Release(PathBuf);
impl Drop for Release {
    fn drop(&mut self) {
        let _ = fs::write(&self.0, b"release");
    }
}

fn get(path: &str) -> Request {
    Request {
        method: "GET".into(),
        path: path.into(),
        body: Value::Null,
    }
}

fn put_config(values: Value) -> Request {
    Request {
        method: "PUT".into(),
        path: "/api/config".into(),
        body: json!({"values":values}),
    }
}

static ENVIRONMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn cached_picker_publishes_ready_options_while_another_provider_is_held() {
    let _environment = ENVIRONMENT.lock().await;
    snapshot_while_held(true, false).await;
}
#[tokio::test]
async fn cold_picker_publishes_a_newly_finished_provider_before_its_held_peer() {
    let _environment = ENVIRONMENT.lock().await;
    snapshot_while_held(false, false).await;
}
#[tokio::test]
async fn final_picker_observes_a_newer_refresh_completed_while_a_peer_was_held() {
    let _environment = ENVIRONMENT.lock().await;
    snapshot_while_held(false, true).await;
}
async fn snapshot_while_held(warm: bool, newer: bool) {
    let root = tempfile::tempdir().unwrap();
    // Metadata fixtures serialize environment overrides and use only private
    // fake data, without reading or changing user credential files.
    std::env::remove_var("OPENROUTER_API_KEY");
    let ollama = root.path().join("ollama");
    let manifest = ollama.join("manifests/registry.ollama.ai/library/fixture/latest");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(&manifest, br#"{"schemaVersion":2,"layers":[]}"#).unwrap();
    std::env::set_var("OLLAMA_MODELS", &ollama);
    let runtime = root.path().join("llama-server");
    fs::write(&runtime, "#!/bin/sh\ncase \"$1\" in --version) echo 'version: 9.9.9-fake (build 1, commit fakecommit)' ;; --list-devices) echo 'Available devices:' ;; *) exit 2 ;; esac\n").unwrap();
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.path().join("architectures.txt"), "qwen3\n").unwrap();
    let model = root.path().join("fixture.gguf");
    write_gguf(
        &model,
        &[
            ("general.architecture", V::Str("qwen3")),
            ("qwen3.context_length", V::U32(40960)),
            ("qwen3.embedding_length", V::U32(1024)),
            ("qwen3.block_count", V::U32(8)),
            ("qwen3.attention.head_count", V::U32(16)),
            ("qwen3.attention.head_count_kv", V::U32(4)),
            (
                "tokenizer.chat_template",
                V::Str("<|im_start|>user\n{{ messages }}<|im_end|>"),
            ),
        ],
        &["token_embd.weight", "output.weight"],
    );
    let local_id = local_engine::entry_id(&model);
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let fake = FakeCodex::new(root.path(), json!({"auth":"chatgpt"}));
    let source = fs::read_to_string(fake.binary()).unwrap().replace(
        "\"id\":\"gpt-6-astra\"",
        "\"id\":C.get('model','gpt-6-astra')",
    );
    fs::write(fake.binary(), source).unwrap();
    let held = root.path().join("held-grok");
    fs::write(&held, HELD_ACP).unwrap();
    fs::set_permissions(&held, fs::Permissions::from_mode(0o700)).unwrap();
    let release = root.path().join("release");
    let _release_on_panic = Release(release.clone());
    let mut agents = cli_agents(&fake);
    agents["grok_binary"] = json!(held);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "cli_agents":agents,
            "local_engine":{"llama_binary":runtime,"directories":[],"files":[model],"imports":[]}
        }),
    )
    .unwrap();
    let cfg = CliAgentsConfig::from_value(&agents).unwrap();
    let service = Service::open(paths, Some(project)).unwrap();
    if warm {
        let ready = service
            .engine
            .vendors()
            .refresh(Vendor::Codex, &cfg, true)
            .await;
        assert_eq!(ready.availability, Availability::Ready);
    }
    // Warm only the local metadata path; no inference or provider probing.
    let local = service.dispatch(get("/api/local-models")).await.unwrap();
    assert!(local["models"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == local_id && row["availability"] == "ready"));
    let full_service = service.clone();
    let full =
        tokio::spawn(async move { full_service.dispatch(get("/api/picker?refresh=1")).await });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !root.path().join("entered").exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("fake provider entered its explicit barrier");
    if !warm {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if service
                    .engine
                    .vendors()
                    .cached(Vendor::Codex)
                    .await
                    .is_some_and(|s| s.availability == Availability::Ready)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the unheld Codex probe completed while its peer remains held");
    }
    if newer {
        fake.configure(json!({"auth":"chatgpt", "model":"current-model"}));
        let status = service
            .engine
            .vendors()
            .refresh(Vendor::Codex, &cfg, true)
            .await;
        assert!(status
            .models
            .iter()
            .any(|model| model.id == "current-model"));
    }
    let snapshot_service = service.clone();
    let mut snapshot =
        tokio::spawn(async move { snapshot_service.dispatch(get("/api/picker?cached=1")).await });
    // The held mock cannot finish until after this observation. The deadline
    // is a watchdog, not a sleep used to guess provider progress.
    let early = tokio::time::timeout(Duration::from_millis(750), &mut snapshot).await;
    let returned_before_release = early.is_ok();
    fs::write(&release, b"release").unwrap();
    let result = match early {
        Ok(result) => result.unwrap().unwrap(),
        Err(_) => tokio::time::timeout(Duration::from_secs(4), snapshot)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
    };
    let final_result = tokio::time::timeout(Duration::from_secs(4), full)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    service.engine.shutdown().await.unwrap();
    if newer {
        for response in [&result, &final_result] {
            let codex: Vec<_> = response["targets"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["provider"] == "cli:codex")
                .collect();
            assert!(
                codex.iter().any(|row| row["model"] == "current-model"),
                "final picker overwrote a newer provider snapshot: {response}"
            );
            assert!(codex.iter().all(|row| row["model"] != "gpt-6-astra"));
        }
    }
    assert!(
        returned_before_release,
        "cached ready rows waited for an unrelated provider handshake"
    );
    assert!(result["targets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["provider"] == "cli:codex" && row["availability"] == "ready"));
    assert!(result["targets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == local_id && row["availability"] == "ready"));
    assert_eq!(
        fs::read_to_string(root.path().join("entered"))
            .unwrap()
            .lines()
            .count(),
        1,
        "reading cached picker data must not start a second probe"
    );
    assert!(
        fake.marker("exec_ran").is_none(),
        "catalog work never submits a paid task"
    );
}

#[derive(Clone, Copy)]
enum Invalidation {
    Clear,
    Forget,
    Offline,
    Disabled,
    Binary,
}

async fn invalidated_probe(cause: Invalidation) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let fake = FakeCodex::new(
        root.path(),
        json!({"auth":"chatgpt","email":"old@example.invalid"}),
    );
    let script = fs::read_to_string(fake.binary()).unwrap().replace(
        "    elif method == \"account/read\":\n",
        "    elif method == \"account/read\":\n        if C.get('hold'):\n            mark('entered')\n            deadline = time.monotonic() + 8\n            while not os.path.exists(os.path.join(HERE, 'release')) and time.monotonic() < deadline: time.sleep(0.005)\n            if not os.path.exists(os.path.join(HERE, 'release')): sys.exit(3)\n",
    );
    fs::write(fake.binary(), script).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    let agents = cli_agents(&fake);
    Config::patch(&paths, json!({"cli_agents":agents})).unwrap();
    let cfg = CliAgentsConfig::from_value(&agents).unwrap();
    let service = Service::open(paths.clone(), Some(project)).unwrap();
    service.dispatch(get("/api/config")).await.unwrap();
    let catalog = service.engine.vendors();
    assert_eq!(
        catalog
            .refresh(Vendor::Codex, &cfg, true)
            .await
            .availability,
        Availability::Ready
    );
    fake.configure(json!({"auth":"chatgpt","email":"late@example.invalid","hold":true}));
    let held_catalog = catalog.clone();
    let held_cfg = cfg.clone();
    let held =
        tokio::spawn(async move { held_catalog.refresh(Vendor::Codex, &held_cfg, true).await });
    let release = fake.dir.join("release");
    let _release_on_panic = Release(release.clone());
    tokio::time::timeout(Duration::from_secs(2), async {
        while fake.marker("entered").is_none() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("old probe entered its explicit account-read barrier");
    let mut changed_runtime_ready = None;
    match cause {
        Invalidation::Clear => catalog.clear(Vendor::Codex).await,
        Invalidation::Forget => catalog.forget(Vendor::Codex).await.unwrap(),
        Invalidation::Offline => {
            service
                .dispatch(put_config(json!({"network":{"mode":"offline"}})))
                .await
                .unwrap();
        }
        Invalidation::Disabled => {
            service
                .dispatch(put_config(json!({"cli_agents":{"enabled":false}})))
                .await
                .unwrap();
        }
        Invalidation::Binary => {
            let new_root = root.path().join("new-runtime");
            let new = FakeCodex::new(
                &new_root,
                json!({"auth":"chatgpt","email":"current@example.invalid"}),
            );
            service
                .dispatch(put_config(
                    json!({"cli_agents":{"codex_binary":new.binary()}}),
                ))
                .await
                .unwrap();
            let current = CliAgentsConfig::from_value(&cli_agents(&new)).unwrap();
            changed_runtime_ready = Some(
                catalog
                    .refresh(Vendor::Codex, &current, true)
                    .await
                    .availability,
            );
        }
    }
    fs::write(&release, b"release").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(4), held)
        .await
        .unwrap()
        .unwrap();
    let persisted = service.engine.store().usage_snapshots().unwrap();
    let cached = catalog.status_cached_json().await;
    // Also attempt a stale request's later phase: it must not reinstall an
    // old configured runtime after a newer request established authority.
    let stale = if matches!(cause, Invalidation::Binary | Invalidation::Disabled) {
        Some(catalog.refresh(Vendor::Codex, &cfg, true).await)
    } else {
        None
    };
    service.engine.shutdown().await.unwrap();
    assert_ne!(
        result.availability,
        Availability::Ready,
        "invalidated probe returned Ready: {result:?}"
    );
    assert!(
        persisted
            .iter()
            .all(|row| row.account != "late@example.invalid"),
        "invalidated probe persisted its old account"
    );
    if matches!(cause, Invalidation::Forget) {
        assert!(persisted.is_empty(), "disconnect must remain forgotten");
        assert!(cached["codex"]["account"].is_null());
    }
    if matches!(cause, Invalidation::Binary) {
        assert_eq!(changed_runtime_ready, Some(Availability::Ready));
        assert_eq!(persisted.len(), 1);
        assert_eq!(persisted[0].account, "current@example.invalid");
        assert_eq!(
            cached["codex"]["account"]["email"],
            "current@example.invalid"
        );
    } else {
        assert_ne!(cached["codex"]["state"], "ready");
    }
    if let Some(stale) = stale {
        assert_ne!(stale.availability, Availability::Ready);
    }
    assert_eq!(
        fake.marker("entered").unwrap().lines().count(),
        1,
        "old captured configuration restarted an invalidated runtime"
    );
}
#[tokio::test]
async fn clearing_a_held_probe_prevents_late_publication() {
    invalidated_probe(Invalidation::Clear).await;
}
#[tokio::test]
async fn forgetting_a_held_probe_prevents_late_persisted_usage() {
    invalidated_probe(Invalidation::Forget).await;
}
#[tokio::test]
async fn going_offline_invalidates_a_held_probe() {
    invalidated_probe(Invalidation::Offline).await;
}
#[tokio::test]
async fn disabling_a_runtime_invalidates_a_held_probe() {
    invalidated_probe(Invalidation::Disabled).await;
}
#[tokio::test]
async fn a_changed_runtime_cannot_be_overwritten_or_reinstalled_by_an_old_request() {
    invalidated_probe(Invalidation::Binary).await;
}

#[tokio::test]
async fn cached_picker_never_fetches_missing_or_stale_openrouter_metadata() {
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let _environment = ENVIRONMENT.lock().await;
    let root = tempfile::tempdir().unwrap();
    let ollama = root.path().join("ollama");
    let manifest = ollama.join("manifests/registry.ollama.ai/library/fixture/latest");
    fs::create_dir_all(manifest.parent().unwrap()).unwrap();
    fs::write(manifest, br#"{"schemaVersion":2,"layers":[]}"#).unwrap();
    std::env::set_var("OLLAMA_MODELS", &ollama);
    std::env::remove_var("OPENROUTER_API_KEY");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    std::env::set_var(
        "SHADOWCODE_OPENROUTER_BASE",
        format!("http://{}", listener.local_addr().unwrap()),
    );
    let requests = Arc::new(AtomicUsize::new(0));
    let observed = requests.clone();
    let (stop, mut stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = &mut stopped => break,
                connection = listener.accept() => {
                    let (mut socket, _) = connection.unwrap();
                    let mut request = [0; 4096];
                    let _ = tokio::time::timeout(Duration::from_secs(1), socket.read(&mut request)).await;
                    observed.fetch_add(1, Ordering::SeqCst);
                    let body = r#"{"data":[]}"#;
                    let reply = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                    let _ = socket.write_all(reply.as_bytes()).await;
                }
            }
        }
    });
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    shadowcode_core::config::set_secret(
        &paths,
        "OPENROUTER_API_KEY",
        "fixture-key-not-a-real-credential",
    )
    .unwrap();
    let fake = FakeCodex::new(root.path(), json!({}));
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    Config::patch(&paths, json!({"cli_agents":{"enabled":false},"local_engine":{"llama_binary":fake.binary(),"directories":[],"files":[],"imports":[]}})).unwrap();
    let service = Service::open(paths.clone(), Some(project)).unwrap();
    let missing = service.dispatch(get("/api/picker?cached=1")).await.unwrap();
    let after_missing = requests.load(Ordering::SeqCst);
    fs::write(paths.state.join("openrouter-models.json"), serde_json::to_vec(&json!({"fetched_at":1,"models":[{"id":"fixture/model","name":"Fixture model","context_length":4096,"prompt_price":0.01,"completion_price":0.01,"tools":true,"vision":false}]})).unwrap()).unwrap();
    let stale = service.dispatch(get("/api/picker?cached=1")).await.unwrap();
    // Give an incorrectly detached refresh the chance to reach the private
    // listener. No external endpoint/key or inference is used.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let count = requests.load(Ordering::SeqCst);
    let _ = stop.send(());
    server.await.unwrap();
    service.engine.shutdown().await.unwrap();
    std::env::remove_var("SHADOWCODE_OPENROUTER_BASE");
    assert!(missing["targets"].is_array());
    assert!(stale["targets"]
        .as_array()
        .unwrap()
        .iter()
        .any(|row| row["id"] == "api:openrouter:fixture/model"));
    assert_eq!(
        after_missing, 0,
        "cached picker fetched missing metadata inline"
    );
    assert_eq!(count, 0, "cached picker scheduled a stale metadata refresh");
}
