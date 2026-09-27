//! ACP catalog negotiation with private fake CLIs; never a real provider/login.
#![cfg(unix)]

use serde_json::{json, Value};
use shadowcode_core::cli_agent::{
    acp_probe, catalog::VendorCatalog, picker::Availability, CliAgentsConfig, Vendor,
};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, time::Duration};

const FAKE: &str = r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
root = Path(__file__).parent
cfg = json.loads((root / 'fixture.json').read_text())
args = sys.argv[1:]
with (root / 'invocations.jsonl').open('a') as log:
    log.write(json.dumps(args) + '\n')
if args == ['--version']:
    print('fake-acp 1.0'); sys.exit(0)
if args == ['models']:
    print('You are logged in with grok.com.\nDefault model: fallback-model\nAvailable models:\n  * fallback-model (default)')
    sys.exit(0)
if args not in (['acp'], ['agent', 'stdio']):
    sys.exit(2)
for raw in sys.stdin:
    frame = json.loads(raw)
    method = frame.get('method')
    with (root / 'methods.jsonl').open('a') as log:
        log.write(json.dumps(method) + '\n')
    error = None
    if method == 'initialize':
        result = {'agentCapabilities':{'promptCapabilities':{'image':True}},
                  'authMethods':[{'id':'cursor_login'}],
                  '_meta':{'modelState':{'currentModelId':'fixture-model',
                       'availableModels':[{'modelId':'fixture-model','name':'Fixture model'}]}}}
        if 'version' in cfg: result['protocolVersion'] = cfg['version']
        if cfg.get('initialize_error'): error = 'fixture handshake failure'
    elif method == 'authenticate':
        result = {}
        if cfg.get('auth_error'): error = 'authentication required'
    elif method == 'session/new':
        result = {'sessionId':'fixture-session','models':{'currentModelId':'fixture-model',
                  'availableModels':[{'modelId':'fixture-model','name':'Fixture model'}]}}
        if cfg.get('session_error'): error = 'fixture session unavailable'
    else:
        result = {}
    reply = {'jsonrpc':'2.0','id':frame['id']}
    if error: reply['error'] = {'code':-1,'message':error}
    else: reply['result'] = result
    print(json.dumps(reply), flush=True)
"#;

struct Fixture {
    root: tempfile::TempDir,
    binary: PathBuf,
}

impl Fixture {
    fn new(config: Value) -> Self {
        let root = tempfile::tempdir().unwrap();
        let binary = root.path().join("fake-acp");
        fs::write(&binary, FAKE).unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self { root, binary };
        fixture.configure(config);
        fixture
    }

    fn configure(&self, config: Value) {
        fs::write(
            self.root.path().join("fixture.json"),
            serde_json::to_vec(&config).unwrap(),
        )
        .unwrap();
    }

    fn config(&self, vendor: Vendor) -> CliAgentsConfig {
        let absent = self.root.path().join("missing").display().to_string();
        let mut config = CliAgentsConfig {
            codex_binary: absent.clone(),
            claude_binary: absent.clone(),
            cursor_binary: absent.clone(),
            grok_binary: absent.clone(),
            antigravity_binary: absent,
            ..Default::default()
        };
        match vendor {
            Vendor::Cursor => config.cursor_binary = self.binary.display().to_string(),
            Vendor::Grok => config.grok_binary = self.binary.display().to_string(),
            _ => panic!("fixture supports only the ordinary ACP CLI launch paths"),
        }
        config
    }

    fn log(&self, name: &str) -> Vec<Value> {
        fs::read_to_string(self.root.path().join(name))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn clear_logs(&self) {
        for name in ["methods.jsonl", "invocations.jsonl"] {
            fs::write(self.root.path().join(name), b"").unwrap();
        }
    }

    fn assert_only_initialize(&self) {
        assert_eq!(self.log("methods.jsonl"), vec![json!("initialize")]);
        assert!(!self
            .log("invocations.jsonl")
            .iter()
            .any(|args| args == &json!(["models"])));
    }
}

#[tokio::test]
async fn acp_discovery_rejects_unsupported_versions_before_auth_or_session() {
    for config in [
        json!({}),
        json!({"version":null}),
        json!({"version":0}),
        json!({"version":2}),
        json!({"version":"1"}),
        json!({"version":-1}),
        json!({"version":1.0}),
    ] {
        let fake = Fixture::new(config);
        let error = acp_probe::probe(
            &fake.binary,
            &["acp"],
            fake.root.path(),
            None,
            Duration::from_secs(3),
        )
        .await
        .expect_err("an incompatible initialize must not continue discovery");
        assert!(error.to_string().contains("supports version 1"), "{error}");
        fake.assert_only_initialize();
    }
}

#[tokio::test]
async fn acp_catalog_rejects_incompatible_runtime_even_when_grok_models_is_logged_in() {
    for vendor in [Vendor::Cursor, Vendor::Grok] {
        for config in [json!({}), json!({"version":2})] {
            let fake = Fixture::new(config);
            let catalog = VendorCatalog::new();
            let cfg = fake.config(vendor);
            let status = catalog.refresh(vendor, &cfg, true).await;
            assert_eq!(status.availability, Availability::Unavailable, "{status:?}");
            assert!(!status.accepts_images);
            assert!(status.models.is_empty());
            assert!(status
                .error
                .as_deref()
                .unwrap()
                .contains("supports version 1"));
            assert!(status.detail.contains("No session was started"));
            assert!(status.detail.contains("runtime is incompatible"));
            let rows = catalog.picker_rows(&cfg, false).await;
            let rows: Vec<_> = rows
                .iter()
                .filter(|r| r.provider == vendor.provider())
                .collect();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].availability, Availability::Unavailable);
            assert!(!rows[0].vision);
            fake.assert_only_initialize();
        }
    }
}

#[tokio::test]
async fn acp_catalog_runtime_version_change_removes_ready_and_vision() {
    for vendor in [Vendor::Cursor, Vendor::Grok] {
        let fake = Fixture::new(json!({"version":1}));
        let catalog = VendorCatalog::new();
        let cfg = fake.config(vendor);
        let ready = catalog.refresh(vendor, &cfg, true).await;
        assert_eq!(ready.availability, Availability::Ready);
        assert!(ready.accepts_images);
        assert_eq!(ready.models[0].id, "fixture-model");
        fake.configure(json!({"version":2}));
        fake.clear_logs();
        let unsupported = catalog.refresh(vendor, &cfg, true).await;
        assert_eq!(unsupported.availability, Availability::Unavailable);
        assert!(!unsupported.accepts_images);
        // Historical model names may remain in the cache, but do not become
        // selectable or retain a live Vision affordance after incompatibility.
        let rows = catalog.picker_rows(&cfg, false).await;
        let rows: Vec<_> = rows
            .iter()
            .filter(|r| r.provider == vendor.provider())
            .collect();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].availability, Availability::Unavailable);
        assert!(!rows[0].vision);
        fake.assert_only_initialize();
    }
}

#[tokio::test]
async fn acp_catalog_supported_auth_states_remain_distinct() {
    for vendor in [Vendor::Cursor, Vendor::Grok] {
        for (config, expected, methods) in [
            (
                json!({"version":1}),
                Availability::Ready,
                vec!["initialize", "authenticate", "session/new"],
            ),
            (
                json!({"version":1,"auth_error":true}),
                Availability::SignIn,
                vec!["initialize", "authenticate"],
            ),
            (
                json!({"version":1,"session_error":true}),
                Availability::Unavailable,
                vec!["initialize", "authenticate", "session/new"],
            ),
        ] {
            let fake = Fixture::new(config);
            let catalog = VendorCatalog::new();
            let status = catalog.refresh(vendor, &fake.config(vendor), true).await;
            assert_eq!(status.availability, expected, "{status:?}");
            assert_eq!(
                fake.log("methods.jsonl"),
                methods.into_iter().map(Value::from).collect::<Vec<_>>()
            );
            assert!(!fake
                .log("invocations.jsonl")
                .iter()
                .any(|args| args == &json!(["models"])));
        }
    }
}

#[tokio::test]
async fn grok_other_probe_failure_retains_existing_models_fallback() {
    let fake = Fixture::new(json!({"version":1,"initialize_error":true}));
    let catalog = VendorCatalog::new();
    let status = catalog
        .refresh(Vendor::Grok, &fake.config(Vendor::Grok), true)
        .await;
    assert_eq!(status.availability, Availability::Ready);
    assert!(!status.accepts_images);
    assert_eq!(status.models[0].id, "fallback-model");
    assert_eq!(fake.log("methods.jsonl"), vec![json!("initialize")]);
    assert!(fake
        .log("invocations.jsonl")
        .iter()
        .any(|args| args == &json!(["models"])));
}
