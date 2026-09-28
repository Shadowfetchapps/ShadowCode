//! Code intelligence: the LSP pool against a fake language server, new-error
//! attachment on edits, and the repo_map / search_code tools.
use serde_json::{json, Value};
use shadowcode_core::{
    approvals::ApprovalHub,
    config::Config,
    events::TaskEvents,
    lsp::{self, Env, Locate, Pool},
    models::ToolCall,
    store::Store,
    tools::ToolExecutor,
    workspace::Workspace,
};
use std::{fs, path::Path, sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

fn fake_server() -> String {
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/fake_lsp.py").to_owned()
}

fn config_with(args: &[&str], extra: Value) -> Config {
    let mut all = vec![json!(fake_server())];
    all.extend(args.iter().map(|a| json!(a)));
    let mut section = json!({
        "servers": {"python": {"command": "python3", "args": all}},
        "diagnostics_wait_ms": 5000,
    });
    for (key, value) in extra.as_object().unwrap() {
        section[key] = value.clone();
    }
    let mut config = Config::default();
    config.extra.insert("code_intel".into(), section);
    config
}

fn private_pool() -> &'static Pool {
    Box::leak(Box::new(Pool::new()))
}

fn env_for(root: &Path, config: &Config, pool: &'static Pool) -> Env {
    Env::new(root, config, None).with_pool(pool)
}

fn messages(diagnostics: &Value) -> Vec<String> {
    diagnostics
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["message"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn lsp_client_reports_diagnostics_and_answers_server_requests() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("a.py"),
        "x = 1  # ERROR:alpha\ny = 2  # WARN:beta\nSHOWCONFIG\n",
    )
    .unwrap();
    let config = config_with(&[], json!({}));
    let env = env_for(root.path(), &config, private_pool());
    let out = lsp::file_diagnostics(&env, "a.py", Duration::from_secs(10))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out["ok"], true, "{out}");
    assert_eq!(out["errors"], 1);
    let shown = &out["diagnostics"];
    assert_eq!(shown[0]["message"], "bad alpha");
    assert_eq!(shown[0]["line"], 1);
    assert_eq!(shown[0]["column"], 10);
    assert_eq!(shown[0]["severity"], "error");
    assert!(messages(shown).contains(&"bad beta".to_owned()));
    // The server asked for "python.analysis" and got the hardening settings.
    // (Its reply may land after the first publish, so change the file once.)
    fs::write(
        root.path().join("a.py"),
        "x = 1  # ERROR:alpha\ny = 2  # WARN:beta\nSHOWCONFIG\n\n",
    )
    .unwrap();
    let out = lsp::file_diagnostics(&env, "a.py", Duration::from_secs(10))
        .await
        .unwrap()
        .unwrap();
    let config_line = messages(&out["diagnostics"])
        .into_iter()
        .find(|m| m.starts_with("config"))
        .unwrap();
    assert_eq!(
        config_line,
        r#"config [{"autoSearchPaths": true, "diagnosticMode": "openFilesOnly"}]"#
    );
}

#[tokio::test]
async fn edits_attach_only_the_errors_they_introduce() {
    let root = tempfile::tempdir().unwrap();
    let log = root.path().join("methods.log");
    let before = "a = 1  # ERROR:old\n";
    fs::write(root.path().join("a.py"), before).unwrap();
    let config = config_with(&["--log", log.to_str().unwrap()], json!({}));
    let env = env_for(root.path(), &config, private_pool());
    // The edit moves the old error down and adds a new one.
    let after = "\n\na = 1  # ERROR:old\nb = 2  # ERROR:new\n";
    fs::write(root.path().join("a.py"), after).unwrap();
    let report = lsp::check_edits(&env, &[("a.py".into(), Some(before.into()))])
        .await
        .unwrap();
    assert_eq!(report["checked"], json!(["a.py"]), "{report}");
    assert_eq!(messages(&report["new_errors"]), ["bad new"], "{report}");
    assert_eq!(report["new_errors"][0]["line"], 4);
    assert_eq!(report["new_errors"][0]["path"], "a.py");
    // A second edit reuses the open document as its baseline (no reopen).
    let third = "\n\na = 1  # ERROR:old\nb = 2  # ERROR:new\nc = 3  # ERROR:third\n";
    fs::write(root.path().join("a.py"), third).unwrap();
    let report = lsp::check_edits(&env, &[("a.py".into(), Some(after.into()))])
        .await
        .unwrap();
    assert_eq!(messages(&report["new_errors"]), ["bad third"], "{report}");
    // Fixing errors reports nothing new.
    fs::write(root.path().join("a.py"), "a = 1\n").unwrap();
    let report = lsp::check_edits(&env, &[("a.py".into(), Some(third.into()))])
        .await
        .unwrap();
    assert!(
        report["new_errors"].as_array().unwrap().is_empty(),
        "{report}"
    );
    assert!(report["note"].as_str().unwrap().contains("no new errors"));
    // A brand-new file: every error in it is new.
    fs::write(root.path().join("b.py"), "z  # ERROR:fresh\n").unwrap();
    let report = lsp::check_edits(&env, &[("b.py".into(), None)])
        .await
        .unwrap();
    assert_eq!(messages(&report["new_errors"]), ["bad fresh"]);
    let methods = fs::read_to_string(&log).unwrap();
    let opens = methods
        .lines()
        .filter(|m| *m == "textDocument/didOpen")
        .count();
    assert_eq!(opens, 2, "one didOpen per file:\n{methods}");
    assert!(methods.contains("textDocument/didSave"));
    // Files no server handles produce no report at all.
    assert!(lsp::check_edits(&env, &[("notes.md".into(), None)])
        .await
        .is_none());
}

#[tokio::test]
async fn a_slow_server_leaves_the_edit_pending_instead_of_blocking() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.py"), "x = 1\n").unwrap();
    let config = config_with(&["--delay-ms", "1500"], json!({"diagnostics_wait_ms": 400}));
    let env = env_for(root.path(), &config, private_pool());
    let started = std::time::Instant::now();
    let report = lsp::check_edits(&env, &[("a.py".into(), None)])
        .await
        .unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
    assert_eq!(report["pending"], json!(["a.py"]), "{report}");
    assert!(report["note"].as_str().unwrap().contains("get_diagnostics"));
}

#[tokio::test]
async fn definition_and_references_come_from_the_server() {
    let root = tempfile::tempdir().unwrap();
    fs::write(
        root.path().join("a.py"),
        "def helper():\n    pass\n\nhelper()\nhelper()\n",
    )
    .unwrap();
    let config = config_with(&[], json!({}));
    let env = env_for(root.path(), &config, private_pool());
    let found = lsp::locate(&env, Locate::Definition, "a.py", 4, 3)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found["ok"], true, "{found}");
    assert_eq!(found["locations"][0]["path"], "a.py");
    assert_eq!(found["locations"][0]["line"], 1);
    assert_eq!(found["locations"][0]["column"], 5);
    assert_eq!(found["locations"][0]["preview"], "def helper():");
    assert_eq!(found["source"], "lsp:python3");
    let refs = lsp::locate(&env, Locate::References, "a.py", 1, 5)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(refs["count"], 3, "{refs}");
    assert!(lsp::locate(&env, Locate::Definition, "x.md", 1, 1)
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn a_crashing_server_backs_off_then_restarts() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("a.py"), "x = 1\n").unwrap();
    let config = config_with(&["--crash-on-open"], json!({}));
    let pool = private_pool();
    let env = env_for(root.path(), &config, pool);
    let first = lsp::file_diagnostics(&env, "a.py", Duration::from_secs(3))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first["ok"], false, "{first}");
    // The next use sees the crash and refuses to restart right away.
    let second = lsp::file_diagnostics(&env, "a.py", Duration::from_secs(3)).await;
    let error = format!("{:#}", second.unwrap_err());
    assert!(error.contains("restarts in"), "{error}");
    let status = pool.status();
    assert_eq!(status[0]["state"], "backoff", "{status:?}");
    assert_eq!(status[0]["failures"], 1);
    assert_eq!(status[0]["starts"], 1);
    tokio::time::sleep(lsp::backoff(1) + Duration::from_millis(200)).await;
    let _ = lsp::file_diagnostics(&env, "a.py", Duration::from_secs(3)).await;
    assert_eq!(pool.status()[0]["starts"], 2);
    pool.stop_all(None).await;
}

#[tokio::test]
async fn the_pool_caps_servers_and_stops_idle_ones() {
    let one = tempfile::tempdir().unwrap();
    let two = tempfile::tempdir().unwrap();
    for root in [&one, &two] {
        fs::write(root.path().join("a.py"), "x = 1\n").unwrap();
    }
    let config = config_with(&[], json!({"max_servers": 1}));
    let pool = private_pool();
    let first = env_for(one.path(), &config, pool);
    let second = env_for(two.path(), &config, pool);
    lsp::file_diagnostics(&first, "a.py", Duration::from_secs(5))
        .await
        .unwrap();
    lsp::file_diagnostics(&second, "a.py", Duration::from_secs(5))
        .await
        .unwrap();
    let running: Vec<_> = pool
        .status()
        .into_iter()
        .filter(|s| s["state"] == "ready")
        .collect();
    assert_eq!(running.len(), 1, "{running:?}");
    assert_eq!(running[0]["root"], json!(two.path()));
    // Stopped to make room is not a crash: the first project restarts at once.
    lsp::file_diagnostics(&first, "a.py", Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(pool.stop_idle_after(Duration::ZERO).await, 1);
    assert!(pool.status().iter().all(|s| s["state"] != "ready"));
}

fn fixture(config: Config) -> (tempfile::TempDir, ToolExecutor) {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    let store = Arc::new(Store::open(&root.path().join("db")).unwrap());
    let session_id = store.create_session(&project, "mock", "").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let task_id = store.create_task(&session_id, "test").unwrap();
    let (sender, _) = tokio::sync::broadcast::channel(100);
    let events = TaskEvents {
        store,
        session_id,
        task_id,
        sender,
    };
    let tools = ToolExecutor::new(
        Arc::new(Workspace::open(&project).unwrap()),
        config,
        ApprovalHub::default(),
        events,
        CancellationToken::new(),
    )
    .unwrap();
    (root, tools)
}

async fn call(tools: &ToolExecutor, name: &str, args: Value) -> shadowcode_core::tools::ToolResult {
    tools
        .execute(ToolCall {
            id: shadowcode_core::id(),
            name: name.into(),
            arguments: args,
        })
        .await
        .unwrap()
}

#[tokio::test]
async fn edit_tools_carry_new_errors_and_lsp_tools_answer() {
    // The default permission mode allows edits without a prompt.
    let (_root, tools) = fixture(config_with(&[], json!({})));
    let project = tools.workspace.path.clone();
    fs::write(
        project.join("app.py"),
        "def run():\n    return 1  # ERROR:old\n",
    )
    .unwrap();
    let read = call(&tools, "read_file", json!({"path": "app.py"})).await;
    assert!(read.success, "{}", read.error);
    let edit = call(
        &tools,
        "edit_file",
        json!({"path": "app.py", "old_string": "return 1", "new_string": "return 2  # ERROR:boom\n    return 1"}),
    )
    .await;
    assert!(edit.success, "{}", edit.error);
    let diagnostics = &edit.output["diagnostics"];
    assert_eq!(
        messages(&diagnostics["new_errors"]),
        ["bad boom"],
        "{diagnostics}"
    );
    // The model sees them in the tool message.
    let message = edit.message("edit_file", 50_000);
    assert!(message["content"].as_str().unwrap().contains("bad boom"));
    // A new file written whole.
    let write = call(
        &tools,
        "write_file",
        json!({"path": "new.py", "content": "x = 1  # ERROR:fresh\n"}),
    )
    .await;
    assert!(write.success, "{}", write.error);
    assert_eq!(
        messages(&write.output["diagnostics"]["new_errors"]),
        ["bad fresh"]
    );
    // Non-code edits carry no diagnostics.
    let notes = call(
        &tools,
        "write_file",
        json!({"path": "notes.md", "content": "hi\n"}),
    )
    .await;
    assert!(notes.success);
    assert!(notes.output.get("diagnostics").is_none());
    let diag = call(&tools, "get_diagnostics", json!({"path": "app.py"})).await;
    assert!(diag.success, "{} {}", diag.error, diag.output);
    assert_eq!(diag.output["errors"], 2);
    let definition = call(
        &tools,
        "goto_definition",
        json!({"path": "app.py", "line": 1, "column": 5}),
    )
    .await;
    assert!(definition.success, "{}", definition.error);
    assert_eq!(definition.output["source"], "lsp:python3");
    // Without a position the tree-sitter index answers.
    let by_name = call(&tools, "goto_definition", json!({"symbol": "run"})).await;
    assert!(by_name.success, "{}", by_name.error);
    assert_eq!(by_name.output["definitions"][0]["path"], "app.py");
}

#[tokio::test]
async fn repo_map_and_search_code_tools_work_without_a_language_server() {
    let (_root, tools) = fixture(Config::default());
    let project = tools.workspace.path.clone();
    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("src/db.go"),
        "package db\n\n// OpenPool opens the connection pool.\nfunc OpenPool(dsn string) error { return nil }\n",
    )
    .unwrap();
    fs::write(
        project.join("src/main.go"),
        "package main\n\nfunc main() { _ = OpenPool(\"x\"); _ = OpenPool(\"y\") }\n",
    )
    .unwrap();
    let map = call(
        &tools,
        "repo_map",
        json!({"query": "OpenPool", "max_tokens": 512}),
    )
    .await;
    assert!(map.success, "{}", map.error);
    let text = map.output["map"].as_str().unwrap();
    assert!(text.starts_with("src/db.go:\n"), "{text}");
    assert!(text.contains("func OpenPool(dsn string) error"), "{text}");
    let found = call(&tools, "search_code", json!({"query": "connection pool"})).await;
    assert!(found.success, "{}", found.error);
    assert_eq!(found.output["mode"], "bm25");
    assert_eq!(found.output["hits"][0]["path"], "src/db.go");
    let scoped = call(
        &tools,
        "search_code",
        json!({"query": "OpenPool", "path": "src/main.go"}),
    )
    .await;
    assert_eq!(
        scoped.output["hits"].as_array().unwrap().len(),
        1,
        "{}",
        scoped.output
    );
    let schemas: Vec<String> = tools
        .schemas()
        .iter()
        .map(|s| s["function"]["name"].as_str().unwrap().to_owned())
        .collect();
    assert!(
        schemas.contains(&"repo_map".to_owned()) && schemas.contains(&"search_code".to_owned())
    );
    assert!(shadowcode_core::permissions::read_only("search_code"));
    assert!(shadowcode_core::permissions::parallel_safe(
        "repo_map",
        &json!({})
    ));
}

async fn dispatch(
    service: &shadowcode_core::service::Service,
    method: &str,
    path: &str,
    body: Value,
) -> anyhow::Result<Value> {
    service
        .dispatch(shadowcode_core::service::Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}

#[tokio::test]
async fn code_intel_routes_report_status_and_change_settings() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("lib.py"),
        "def parse_settings(text):\n    return text\n",
    )
    .unwrap();
    let paths = shadowcode_core::paths::AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths, json!({"network": {"mode": "offline"}})).unwrap();
    let service = shadowcode_core::service::Service::open(paths.clone(), Some(project)).unwrap();
    let status = dispatch(&service, "GET", "/api/code-intel/status", json!({}))
        .await
        .unwrap();
    let languages: Vec<&str> = status["languages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["language"].as_str().unwrap())
        .collect();
    assert_eq!(languages, ["rust", "typescript", "python", "go", "c"]);
    assert_eq!(status["managed"].as_array().unwrap().len(), 2);
    assert_eq!(status["managed"][0]["installed"], false);
    assert!(status["managed"][0]["approx_bytes"].as_u64().unwrap() > 1_000_000);
    let models = status["embeddings"]["models"].as_array().unwrap();
    assert!(models
        .iter()
        .all(|m| m["installed"] == false && m["bytes"].as_u64().unwrap() > 0));
    assert!(status["embeddings"]["active"].is_null());
    assert_eq!(status["config"]["repo_map_tokens"], 1024);
    assert_eq!(status["offline"], true);
    // Downloads refuse in offline mode, and nothing is fetched on its own.
    let refused = dispatch(
        &service,
        "POST",
        "/api/code-intel/install",
        json!({"package": "python"}),
    )
    .await
    .unwrap_err();
    assert!(format!("{refused:#}").contains("offline"));
    let refused = dispatch(
        &service,
        "POST",
        "/api/code-intel/embeddings/install",
        json!({"model": "bge-small-en-v1.5-q8"}),
    )
    .await
    .unwrap_err();
    assert!(format!("{refused:#}").contains("offline"));
    assert!(!paths.data.join("code-intel/models").exists());
    // Settings are validated and saved under code_intel.
    let changed = dispatch(
        &service,
        "POST",
        "/api/code-intel/config",
        json!({"repo_map_tokens": 0, "lsp": false}),
    )
    .await
    .unwrap();
    assert_eq!(changed["config"]["repo_map_tokens"], 0);
    let saved = Config::load(&paths, None).unwrap();
    assert_eq!(saved.extra["code_intel"]["lsp"], false);
    assert!(dispatch(
        &service,
        "POST",
        "/api/code-intel/config",
        json!({"max_servers": 0})
    )
    .await
    .is_err());
    let index = dispatch(&service, "POST", "/api/code-intel/reindex", json!({}))
        .await
        .unwrap();
    assert_eq!(index["index"]["languages"]["python"], 1, "{index}");
    let found = dispatch(
        &service,
        "POST",
        "/api/code-intel/search",
        json!({"query": "parse settings"}),
    )
    .await
    .unwrap();
    assert_eq!(found["hits"][0]["path"], "lib.py", "{found}");
    let map = dispatch(
        &service,
        "GET",
        "/api/code-intel/repo-map?tokens=256",
        json!({}),
    )
    .await
    .unwrap();
    assert!(map["map"]
        .as_str()
        .unwrap()
        .contains("def parse_settings(text):"));
    assert!(dispatch(&service, "GET", "/api/code-intel/nope", json!({}))
        .await
        .is_err());
}

/// Downloads for real (npm packages, an embedding GGUF) into
/// `$SHADOWCODE_LIVE_DIR`: `cargo test --test code_intel live_ -- --ignored`.
#[tokio::test]
#[ignore]
async fn live_managed_install_and_semantic_search() {
    let dir = std::path::PathBuf::from(
        std::env::var("SHADOWCODE_LIVE_DIR").expect("set SHADOWCODE_LIVE_DIR"),
    );
    let managed = lsp::managed_dir(&dir);
    let package = lsp::install::package("typescript").unwrap();
    if !managed
        .join("typescript/node_modules/.bin/typescript-language-server")
        .exists()
    {
        assert!(lsp::install::start(managed.clone(), package).unwrap());
        loop {
            tokio::time::sleep(Duration::from_millis(500)).await;
            let state = lsp::install::state("typescript").unwrap();
            if state.state != "installing" {
                assert_eq!(state.state, "installed", "{state:?}");
                break;
            }
        }
    }
    eprintln!("managed: {:#}", lsp::install::status(&managed));
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    config
        .extra
        .insert("code_intel".into(), json!({"diagnostics_wait_ms": 20000}));
    let env = Env::new(root.path(), &config, Some(&dir)).with_pool(private_pool());
    let before = "export function add(a: number, b: number): number { return a + b; }\n";
    fs::write(
        root.path().join("m.ts"),
        "export function add(a: number, b: number): number { return a + b; }\nconst n: number = add(1, \"2\");\n",
    )
    .unwrap();
    let report = lsp::check_edits(&env, &[("m.ts".into(), Some(before.into()))])
        .await
        .unwrap();
    eprintln!("typescript: {report:#}");
    assert_eq!(
        report["new_errors"].as_array().unwrap().len(),
        1,
        "{report}"
    );
    env.pool.stop_all(None).await;

    let entry =
        shadowcode_core::code_intel::embeddings::catalog_entry("bge-small-en-v1.5-q8").unwrap();
    if !shadowcode_core::code_intel::embeddings::installed(&dir, entry) {
        let (tx, rx) = tokio::sync::oneshot::channel();
        assert!(shadowcode_core::code_intel::embeddings::start_install(
            dir.clone(),
            entry,
            move |r| {
                let _ = tx.send(r.map_err(|e| format!("{e:#}")));
            }
        ));
        rx.await.unwrap().unwrap();
    }
    fs::write(root.path().join("net.py"), "def open_socket(host, port):\n    import socket\n    return socket.create_connection((host, port))\n").unwrap();
    fs::write(
        root.path().join("colors.py"),
        "def blend(a, b):\n    return [(x + y) / 2 for x, y in zip(a, b)]\n",
    )
    .unwrap();
    fs::write(
        root.path().join("notes.md"),
        "# Notes\nShopping list: apples, pears.\n",
    )
    .unwrap();
    let semantic = shadowcode_core::code_intel::embeddings::Semantic::resolve(&dir, &config)
        .expect("llama-server and model");
    let out = shadowcode_core::code_intel::search::search_code(
        root.path().to_owned(),
        shadowcode_core::code_intel::search::Request {
            query: "connect to a remote machine over the network".into(),
            path: String::new(),
            max_hits: 3,
        },
        Some(semantic),
    )
    .await
    .unwrap();
    eprintln!("semantic: {out:#}");
    assert_eq!(out["mode"], "hybrid", "{out}");
    assert_eq!(out["hits"][0]["path"], "net.py");
    shadowcode_core::code_intel::embeddings::stop().await;
}

/// Real servers, when installed: `cargo test --test code_intel -- --ignored`.
#[tokio::test]
#[ignore]
async fn real_servers_smoke() {
    let root = tempfile::tempdir().unwrap();
    let mut config = Config::default();
    // First starts of real servers are slower than later edits.
    config
        .extra
        .insert("code_intel".into(), json!({"diagnostics_wait_ms": 20000}));
    let env = env_for(root.path(), &config, private_pool());
    let mut ran = Vec::new();
    if lsp::servers::resolve(lsp::servers::ServerLang::Python, &env.config, None).is_ok() {
        let before = "def add(a, b):\n    return a + b\n";
        fs::write(
            root.path().join("m.py"),
            "def add(a, b):\n    return a + b\n\nprint(add(1, 2))\nprint(missing_name)\n",
        )
        .unwrap();
        let report = lsp::check_edits(&env, &[("m.py".into(), Some(before.into()))])
            .await
            .unwrap();
        eprintln!("python: {report:#}");
        assert!(report.to_string().contains("missing_name"), "{report}");
        let found = lsp::locate(&env, Locate::Definition, "m.py", 4, 7)
            .await
            .unwrap()
            .unwrap();
        eprintln!("python definition: {found:#}");
        assert_eq!(found["locations"][0]["line"], 1);
        ran.push("python");
    }
    if lsp::servers::resolve(lsp::servers::ServerLang::Rust, &env.config, None).is_ok() {
        fs::write(
            root.path().join("Cargo.toml"),
            "[package]\nname = \"smoke\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        fs::create_dir_all(root.path().join("src")).unwrap();
        let before = "pub fn one() -> u32 { 1 }\n";
        fs::write(root.path().join("src/lib.rs"), before).unwrap();
        // Let rust-analyzer load the project first.
        let mut out = Value::Null;
        for _ in 0..30 {
            out = lsp::file_diagnostics(&env, "src/lib.rs", Duration::from_secs(5))
                .await
                .unwrap()
                .unwrap();
            if out["ok"] == true {
                break;
            }
        }
        eprintln!("rust baseline: {out:#}");
        fs::write(
            root.path().join("src/lib.rs"),
            "pub fn one() -> u32 { \"text\" }\npub fn two( {}\n",
        )
        .unwrap();
        let report = lsp::check_edits(&env, &[("src/lib.rs".into(), Some(before.into()))])
            .await
            .unwrap();
        eprintln!("rust: {report:#}");
        assert!(
            !report["new_errors"].as_array().unwrap().is_empty(),
            "{report}"
        );
        // An edit that changes no diagnostics: rust-analyzer stays silent.
        let broken = "pub fn one() -> u32 { \"text\" }\npub fn two( {}\n";
        fs::write(
            root.path().join("src/lib.rs"),
            "pub fn one() -> u32 { \"text\" }\npub fn two( {}\n// comment\n",
        )
        .unwrap();
        let started = std::time::Instant::now();
        let report = lsp::check_edits(&env, &[("src/lib.rs".into(), Some(broken.into()))])
            .await
            .unwrap();
        eprintln!("rust unchanged ({:?}): {report:#}", started.elapsed());
        assert_eq!(report["checked"], json!(["src/lib.rs"]), "{report}");
        assert!(
            report["new_errors"].as_array().unwrap().is_empty(),
            "{report}"
        );
        ran.push("rust");
    }
    eprintln!("ran: {ran:?}");
    env.pool.stop_all(None).await;
}

/// A language server that stops reading its input cannot block an edit (or
/// Stop) forever: the write gives up, the file is reported as unchecked and
/// the server is stopped.
#[tokio::test]
async fn a_server_that_stops_reading_cannot_block_an_edit() {
    let root = tempfile::tempdir().unwrap();
    // Larger than a pipe buffer, so writing it needs a reader.
    let before = "x = 1\n".repeat(60_000);
    let after = format!("{before}y = 2  # ERROR:new\n");
    fs::write(root.path().join("big.py"), &after).unwrap();
    let config = config_with(&["--stop-reading"], json!({}));
    let env = env_for(root.path(), &config, private_pool());
    let started = std::time::Instant::now();
    let report = tokio::time::timeout(
        Duration::from_secs(45),
        lsp::check_edits(&env, &[("big.py".into(), Some(before))]),
    )
    .await
    .expect("the edit check gave up on the stuck server")
    .unwrap();
    assert!(started.elapsed() < Duration::from_secs(40));
    assert_eq!(report["checked"], json!([]), "{report}");
    assert!(report.to_string().contains("stopped reading"), "{report}");
}
