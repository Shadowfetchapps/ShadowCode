//! Built-in local engine against a fake `llama-server` (a small Python
//! script). The fake answers `--version` / `--list-devices`, serves `/health`,
//! `/props`, and `/v1/chat/completions` (canned tool calls), requires the
//! per-launch bearer key, records every launch and request, and exits on
//! SIGTERM. Nothing here touches a GPU or a real model.
mod vendor_support;
use serde_json::{json, Value};
use shadowcode_core::{
    config::{Config, ModelConfig},
    engine::Engine,
    gguf::test_support::{write_gguf, V},
    local_engine,
    paths::AppPaths,
    service::{Request, Service},
};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

const FAKE: &str = r#"#!/usr/bin/env python3
import json, os, signal, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]

def log(name, value):
    with open(os.path.join(HERE, name), "a") as f:
        f.write(json.dumps(value) + "\n")

if args == ["--version"]:
    print("version: 9.9.9-fake (build 1, commit fakecommit)")
    sys.exit(0)
if args == ["--list-devices"]:
    path = os.path.join(HERE, "devices.txt")
    print("Available devices:")
    if os.path.exists(path):
        print(open(path).read().rstrip())
    sys.exit(0)

def opt(name, default=None):
    return args[args.index(name) + 1] if name in args else default

model = opt("-m", "")
port = int(opt("--port"))
ctx = int(opt("--ctx-size", "0"))
mmproj = opt("--mmproj")
key = os.environ.get("LLAMA_API_KEY", "")
cpu = opt("--device") == "none"
log("launches.jsonl", {"argv": args, "pid": os.getpid(), "key_env": bool(key),
                       "key_in_argv": key != "" and key in " ".join(args),
                       "proxy_env": any(k.lower().endswith("_proxy") for k in os.environ)})
signal.signal(signal.SIGTERM, lambda *_: os._exit(0))
name = os.path.basename(model)
if "crash" in name:
    sys.stderr.write("llama_model_load: error: fake crash loading model\n")
    sys.stderr.flush()
    sys.exit(1)
if "gpufail" in name and not cpu:
    sys.stderr.write("ggml_vulkan: fake device lost\n")
    sys.stderr.flush()
    sys.exit(1)
if "slow" in name:
    time.sleep(60)

def sse(handler, chunks):
    handler.send_response(200)
    handler.send_header("Content-Type", "text/event-stream")
    handler.end_headers()
    for chunk in chunks:
        handler.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
    handler.wfile.write(b"data: [DONE]\n\n")

def tool_call(name, arguments):
    return [
        {"choices": [{"delta": {"role": "assistant", "content": None, "tool_calls": [
            {"index": 0, "id": "call_" + name, "type": "function",
             "function": {"name": name, "arguments": json.dumps(arguments)}}]}, "finish_reason": None}]},
        {"choices": [{"delta": {}, "finish_reason": "tool_calls"}],
         "usage": {"prompt_tokens": 50, "completion_tokens": 10}},
    ]

def text(content):
    return [
        {"choices": [{"delta": {"role": "assistant", "content": content}, "finish_reason": None}]},
        {"choices": [{"delta": {}, "finish_reason": "stop"}],
         "usage": {"prompt_tokens": 60, "completion_tokens": 8}},
    ]

class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"
    def log_message(self, *a):
        sys.stderr.write("request " + self.path + "\n")
    def authorized(self):
        return self.headers.get("Authorization", "") == "Bearer " + key
    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
    def do_GET(self):
        if self.path == "/health":
            return self.reply(200, {"status": "ok"})
        if not self.authorized():
            return self.reply(401, {"error": "Invalid API Key"})
        if self.path == "/props":
            return self.reply(200, {"default_generation_settings": {"n_ctx": ctx},
                                    "modalities": {"vision": mmproj is not None}})
        self.reply(404, {})
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        wire = json.dumps(body)
        messages = body.get("messages", [])
        has_image = "image_url" in wire
        log("requests.jsonl", {"auth": self.authorized(), "tools": [t["function"]["name"] for t in body.get("tools", [])],
                               "image": has_image, "kwargs": body.get("chat_template_kwargs"),
                               "max_tokens": body.get("max_tokens"), "chars": len(wire),
                               "type_list": '"type": [' in json.dumps(body.get("tools", []))})
        if not self.authorized():
            return self.reply(401, {"error": "Invalid API Key"})
        if len(wire) // 4 > ctx:
            return self.reply(400, {"error": {"type": "exceed_context_size_error", "n_ctx": ctx}})
        tool_results = [m for m in messages if m.get("role") == "tool"]
        if "vision" in name:
            if not any(m.get("tool_call_id") == "call_view_image" for m in tool_results):
                return sse(self, tool_call("view_image", {"path": "red.png"}))
            return sse(self, text("The image is red." if has_image else "No image arrived."))
        if not body.get("tools"):
            return sse(self, text("Chat only reply."))
        if not tool_results:
            return sse(self, tool_call("read_file", {"path": "hello.txt"}))
        return sse(self, text("The file says hello from the fake model."))

ThreadingHTTPServer(("127.0.0.1", port), Handler).serve_forever()
"#;

struct Fixture {
    _root: tempfile::TempDir,
    bin: PathBuf,
    models: PathBuf,
    project: PathBuf,
    paths: AppPaths,
}

fn qwen_like(path: &Path, arch: &str, template: &str) {
    let ctx = format!("{arch}.context_length");
    let emb = format!("{arch}.embedding_length");
    let blk = format!("{arch}.block_count");
    let heads = format!("{arch}.attention.head_count");
    let kv = format!("{arch}.attention.head_count_kv");
    write_gguf(
        path,
        &[
            ("general.architecture", V::Str(arch)),
            (ctx.as_str(), V::U32(40960)),
            (emb.as_str(), V::U32(1024)),
            (blk.as_str(), V::U32(8)),
            (heads.as_str(), V::U32(16)),
            (kv.as_str(), V::U32(4)),
            ("tokenizer.chat_template", V::Str(template)),
        ],
        &["token_embd.weight", "output.weight"],
    );
}

fn projector(path: &Path) {
    write_gguf(
        path,
        &[
            ("general.architecture", V::Str("clip")),
            ("clip.has_vision_encoder", V::Bool(true)),
            ("clip.vision.projection_dim", V::U32(1024)),
        ],
        &["v.patch_embd.weight"],
    );
}

const TOOLS_TEMPLATE: &str =
    "{% if tools %}<tool_call>{% endif %}{% if enable_thinking %}{% endif %}";

fn fixture(devices: &str) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    let bin = root.path().join("runtime");
    fs::create_dir_all(&bin).unwrap();
    let server = bin.join("llama-server");
    fs::write(&server, FAKE).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&server, fs::Permissions::from_mode(0o755)).unwrap();
    }
    fs::write(
        bin.join("architectures.txt"),
        "qwen3\nllama\ngemma4\ngpt-oss\n",
    )
    .unwrap();
    fs::write(
        bin.join("COMMIT"),
        "commit=fakecommit\nbackend=vulkan+cpu\nbuilt=2026-09-23T00:00:00Z\n",
    )
    .unwrap();
    if !devices.is_empty() {
        fs::write(bin.join("devices.txt"), devices).unwrap();
    }
    let models = root.path().join("models");
    fs::create_dir_all(&models).unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(&project).unwrap();
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"fixture","context_limit":16384},
            "permissions":{"approve_shell":false},
            "trusted_workspaces":[project.clone()],
            "local_engine":{"llama_binary": server.display().to_string()}
        }),
    )
    .unwrap();
    Fixture {
        _root: root,
        bin,
        models,
        project,
        paths,
    }
}

const GPU: &str = "  Vulkan0: Fake GPU 16 (16384 MiB, 16000 MiB free)";

fn lines(path: &Path) -> Vec<Value> {
    fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

async fn call(service: &Service, method: &str, path: &str, body: Value) -> anyhow::Result<Value> {
    service
        .dispatch(Request {
            method: method.into(),
            path: path.into(),
            body,
        })
        .await
}

fn pid_alive(pid: u64) -> bool {
    // A zombie still has a /proc entry; check its state.
    fs::read_to_string(format!("/proc/{pid}/stat"))
        .map(|s| !s.split(") ").nth(1).unwrap_or("").starts_with('Z'))
        .unwrap_or(false)
}

async fn wait_job(service: &Service, id: &str) -> Value {
    let started = Instant::now();
    loop {
        let job = call(service, "GET", &format!("/api/jobs/{id}"), Value::Null)
            .await
            .unwrap();
        if matches!(
            job["status"].as_str(),
            Some("completed" | "failed" | "cancelled" | "limit_reached")
        ) {
            return job;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "{job}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn model_for(id: &str) -> ModelConfig {
    let entry = local_engine::known(id).expect("scanned entry");
    ModelConfig {
        default: id.into(),
        name: entry.name,
        provider: "llamacpp".into(),
        endpoint: String::new(),
        api_key_env: "UNUSED".into(),
        keep_alive: "30m".into(),
        context_limit: entry.context_tokens as usize,
    }
}

#[tokio::test]
async fn catalog_routes_add_list_remove_picker_and_weights_stay() {
    let f = fixture(GPU);
    let qwen = f.models.join("qwen3-14b.gguf");
    qwen_like(&qwen, "qwen3", TOOLS_TEMPLATE);
    let dir = f.models.join("folder");
    fs::create_dir_all(&dir).unwrap();
    let gemma = dir.join("gemma.gguf");
    qwen_like(&gemma, "gemma4", "{{ messages }}");
    projector(&dir.join("gemma.mmproj.gguf"));
    let oss = dir.join("oss.gguf");
    qwen_like(&oss, "gptoss", "tools");
    write_gguf(
        &dir.join("ggml-vocab.gguf"),
        &[("general.architecture", V::Str("llama"))],
        &[],
    );
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    call(
        &service,
        "POST",
        "/api/local-models/add",
        json!({"path": qwen}),
    )
    .await
    .unwrap();
    let added = call(
        &service,
        "POST",
        "/api/local-models/add",
        json!({"path": dir}),
    )
    .await
    .unwrap();
    assert_eq!(added["ok"], true);
    let err = call(
        &service,
        "POST",
        "/api/local-models/add",
        json!({"path": dir.join("gemma.mmproj.gguf")}),
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("projector"), "{err}");

    let catalog = call(&service, "GET", "/api/local-models", Value::Null)
        .await
        .unwrap();
    assert_eq!(catalog["runtime"]["state"], "ready", "{catalog}");
    assert_eq!(catalog["runtime"]["version"], "9.9.9-fake");
    assert_eq!(catalog["runtime"]["origin"], "other");
    assert_eq!(catalog["hardware"]["backend"], "vulkan");
    assert_eq!(catalog["hardware"]["gpu"], "Fake GPU 16");
    assert!(catalog["loaded"].is_null());
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(models.len(), 3, "vocab and projector files are not models");
    let by_name = |n: &str| models.iter().find(|m| m["name"] == n).unwrap().clone();
    let q = by_name("qwen3-14b");
    assert_eq!(q["availability"], "ready");
    assert_eq!(q["tools"], true);
    assert_eq!(q["vision"], false);
    assert_eq!(q["context_train"], 40960);
    assert_eq!(q["context_tokens"], 16384);
    assert_eq!(q["fits"], "gpu");
    assert_eq!(q["source"], "file");
    let g = by_name("gemma");
    assert_eq!(g["vision"], true);
    assert_eq!(g["tools"], false);
    assert!(g["tools_reason"].as_str().unwrap().contains("Chat only"));
    assert_eq!(g["source"], "directory");
    let o = by_name("oss");
    assert_eq!(o["compatible"], false);
    assert_eq!(o["availability"], "unavailable");
    assert_eq!(o["reason"], "unsupported architecture gptoss");

    for route in ["/api/picker", "/api/models?detect=false"] {
        let picker = call(&service, "GET", route, Value::Null).await.unwrap();
        let rows = picker[if route == "/api/picker" {
            "targets"
        } else {
            "picker"
        }]
        .as_array()
        .unwrap()
        .clone();
        let local: Vec<_> = rows.iter().filter(|r| r["group"] == "local").collect();
        assert_eq!(local.len(), 3, "{route}");
        assert!(local.iter().all(|r| r["route"] == "local_llamacpp"
            && r["provider"] == "llamacpp"
            && r["id"].as_str().unwrap().starts_with("local:gguf:")));
        let oss_row = local.iter().find(|r| r["id"] == o["id"]).unwrap();
        assert_eq!(oss_row["availability_label"], "Unavailable");
    }

    // Removing a directory-discovered file excludes it; weights stay.
    let removed = call(
        &service,
        "POST",
        "/api/local-models/remove",
        json!({"id": g["id"]}),
    )
    .await
    .unwrap();
    assert_eq!(removed["deleted_weights"], false);
    let ids: Vec<_> = removed["local_engine"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].clone())
        .collect();
    assert!(!ids.contains(&g["id"]));
    call(
        &service,
        "POST",
        "/api/local-models/remove",
        json!({"path": qwen}),
    )
    .await
    .unwrap();
    assert!(gemma.exists() && qwen.exists(), "weights are never deleted");
    let cfg = Config::load(&f.paths, None).unwrap();
    assert!(cfg.local_engine.files.is_empty());
    assert_eq!(cfg.local_engine.excluded.len(), 1);
}

#[tokio::test]
async fn memory_too_large_is_marked_unavailable_and_refused() {
    let f = fixture("");
    let huge = f.models.join("huge.gguf");
    write_gguf(
        &huge,
        &[
            ("general.architecture", V::Str("llama")),
            ("llama.context_length", V::U32(131072)),
            ("llama.embedding_length", V::U32(65536)),
            ("llama.block_count", V::U32(4000)),
            ("llama.attention.head_count", V::U32(64)),
            ("llama.attention.head_count_kv", V::U32(64)),
            ("tokenizer.chat_template", V::Str("tools")),
        ],
        &["token_embd.weight"],
    );
    let config = local_engine::LocalEngineConfig {
        files: vec![huge.display().to_string()],
        llama_binary: f.bin.join("llama-server").display().to_string(),
        ..Default::default()
    };
    let catalog = local_engine::catalog(&config);
    assert_eq!(catalog["hardware"]["backend"], "cpu");
    let entry = &catalog["models"][0];
    assert_eq!(entry["fits"], "no", "{entry}");
    assert_eq!(entry["availability"], "unavailable");
    assert!(entry["reason"].as_str().unwrap().starts_with("Needs ≈"));
    assert_eq!(entry["context_tokens"], 4096);
    let engine = Engine::open(f.paths.clone()).unwrap();
    let mut cfg = Config::load(&f.paths, None).unwrap();
    cfg.local_engine = config;
    let model = ModelConfig {
        context_limit: 4096,
        ..model_for(entry["id"].as_str().unwrap())
    };
    let error = engine
        .prepare_model_client(&cfg, &model, &CancellationToken::new())
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("Needs ≈"), "{error}");
    assert!(lines(&f.bin.join("launches.jsonl")).is_empty());
}

#[tokio::test]
async fn load_switch_lease_unload_and_shutdown_stop_the_server() {
    let f = fixture(GPU);
    let a = f.models.join("a.gguf");
    qwen_like(&a, "qwen3", TOOLS_TEMPLATE);
    let vdir = f.models.join("v");
    fs::create_dir_all(&vdir).unwrap();
    let b = vdir.join("b.gguf");
    qwen_like(&b, "gemma4", TOOLS_TEMPLATE);
    projector(&vdir.join("mmproj-b.gguf"));
    Config::patch(
        &f.paths,
        json!({"local_engine":{"files":[a.display().to_string(), b.display().to_string()]}}),
    )
    .unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let entries = local_engine::scan(&cfg.local_engine);
    let id_a = entries.iter().find(|e| e.name == "a").unwrap().id.clone();
    let id_b = entries.iter().find(|e| e.name == "b").unwrap().id.clone();
    let engine = Engine::open(f.paths.clone()).unwrap();
    let cancel = CancellationToken::new();

    let first = engine
        .prepare_model_client(&cfg, &model_for(&id_a), &cancel)
        .await
        .unwrap();
    assert_eq!(first.config.context_limit, 16384);
    assert!(first.config.endpoint.starts_with("http://127.0.0.1:"));
    assert_eq!(first.vision, Some(false));
    assert!(first.bearer.as_deref().is_some_and(|k| k.len() == 64));
    let launches = lines(&f.bin.join("launches.jsonl"));
    assert_eq!(launches.len(), 1);
    let argv = launches[0]["argv"].as_array().unwrap();
    let argv: Vec<&str> = argv.iter().map(|v| v.as_str().unwrap()).collect();
    let joined = argv.join(" ");
    assert!(joined.contains("--host 127.0.0.1"), "{joined}");
    assert!(joined.contains("--no-webui --jinja --ctx-size 16384 --parallel 1"));
    assert!(joined.ends_with("-ngl 999"));
    assert!(!joined.contains("--mmproj"));
    assert_eq!(launches[0]["key_env"], true);
    assert_eq!(launches[0]["key_in_argv"], false);
    assert_eq!(launches[0]["proxy_env"], false);
    let pid_a = launches[0]["pid"].as_u64().unwrap();

    // A task holds A: B waits without loading and cancellation removes it.
    let queued_cancel = CancellationToken::new();
    let queued_engine = engine.clone();
    let queued_cfg = cfg.clone();
    let queued_model = model_for(&id_b);
    let token = queued_cancel.clone();
    let queued = tokio::spawn(async move {
        queued_engine
            .prepare_model_client(&queued_cfg, &queued_model, &token)
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!queued.is_finished());
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 1);
    queued_cancel.cancel();
    let error = tokio::time::timeout(Duration::from_secs(2), queued)
        .await
        .unwrap()
        .unwrap()
        .err()
        .unwrap();
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert!(engine.local_runtime().unload().await.is_err());
    // Same model is shared.
    let again = engine
        .prepare_model_client(&cfg, &model_for(&id_a), &cancel)
        .await
        .unwrap();
    assert_eq!(engine.local_runtime().in_use(), 2);
    drop(again);
    let waiting_engine = engine.clone();
    let waiting_cfg = cfg.clone();
    let waiting_model = model_for(&id_b);
    let waiting = tokio::spawn(async move {
        waiting_engine
            .prepare_model_client(&waiting_cfg, &waiting_model, &CancellationToken::new())
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!waiting.is_finished());
    drop(first);
    assert_eq!(engine.local_runtime().in_use(), 0);

    // Now the switch works and loads the projector.
    let second = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(second.vision, Some(true));
    let launches = lines(&f.bin.join("launches.jsonl"));
    assert_eq!(launches.len(), 2);
    let joined: Vec<&str> = launches[1]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(joined.join(" ").contains(&format!(
        "--mmproj {}",
        vdir.join("mmproj-b.gguf").display()
    )));
    assert!(!pid_alive(pid_a), "switching stops the previous server");
    let loaded = engine.local_runtime().loaded_json().unwrap();
    assert_eq!(loaded["id"], id_b.as_str());
    assert_eq!(loaded["context_tokens"], 16384);
    assert_eq!(loaded["backend"], "vulkan");
    assert_eq!(loaded["in_use"], 1);
    drop(second);
    let pid_b = launches[1]["pid"].as_u64().unwrap();
    assert!(pid_alive(pid_b));
    assert!(engine.local_runtime().unload().await.unwrap());
    assert!(engine.local_runtime().loaded_json().is_none());
    assert!(!pid_alive(pid_b));

    // Shutdown stops a loaded server.
    let third = engine
        .prepare_model_client(&cfg, &model_for(&id_a), &cancel)
        .await
        .unwrap();
    let queued_engine = engine.clone();
    let queued_cfg = cfg.clone();
    let queued_model = model_for(&id_b);
    let shutdown_waiter = tokio::spawn(async move {
        queued_engine
            .prepare_model_client(&queued_cfg, &queued_model, &CancellationToken::new())
            .await
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!shutdown_waiter.is_finished());
    let pid_c = lines(&f.bin.join("launches.jsonl"))[2]["pid"]
        .as_u64()
        .unwrap();
    engine.shutdown().await.unwrap();
    assert!(!pid_alive(pid_c), "no orphan llama-server after shutdown");
    assert!(
        tokio::time::timeout(Duration::from_secs(2), shutdown_waiter)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    drop(third);
    assert_eq!(
        lines(&f.bin.join("launches.jsonl")).len(),
        3,
        "shutdown must not start a waiting model"
    );
}

#[tokio::test]
async fn cancel_during_load_early_exit_tail_and_cpu_fallback() {
    let f = fixture(GPU);
    let slow = f.models.join("slow.gguf");
    let crash = f.models.join("crash.gguf");
    let gpufail = f.models.join("gpufail.gguf");
    for p in [&slow, &crash, &gpufail] {
        qwen_like(p, "qwen3", TOOLS_TEMPLATE);
    }
    Config::patch(
        &f.paths,
        json!({"local_engine":{"files":[slow.display().to_string(), crash.display().to_string(), gpufail.display().to_string()]}}),
    )
    .unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let entries = local_engine::scan(&cfg.local_engine);
    let id = |n: &str| entries.iter().find(|e| e.name == n).unwrap().id.clone();
    let engine = Engine::open(f.paths.clone()).unwrap();

    // Cancel while the server is still loading: returns promptly, kills it.
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(700)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    let error = engine
        .prepare_model_client(&cfg, &model_for(&id("slow")), &cancel)
        .await
        .err()
        .unwrap();
    assert!(error.to_string().contains("cancelled"), "{error}");
    assert!(started.elapsed() < Duration::from_secs(8));
    let pid = lines(&f.bin.join("launches.jsonl"))[0]["pid"]
        .as_u64()
        .unwrap();
    assert!(!pid_alive(pid));

    // A server that exits early: stderr tail in the error and on the row;
    // the GPU attempt is retried once on the CPU.
    let error = engine
        .prepare_model_client(&cfg, &model_for(&id("crash")), &CancellationToken::new())
        .await
        .err()
        .unwrap();
    let text = format!("{error:#}");
    assert!(text.contains("fake crash loading model"), "{text}");
    assert!(text.contains("The GPU attempt failed first"));
    let catalog = local_engine::catalog_with(&cfg.local_engine, Some(engine.local_runtime()));
    let row = catalog["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "crash")
        .unwrap()
        .clone();
    assert!(row["last_error"]
        .as_str()
        .unwrap()
        .contains("fake crash loading model"));

    let before_strict = lines(&f.bin.join("launches.jsonl")).len();
    assert!(local_engine::prepare_with_policy(
        &cfg.local_engine,
        &model_for(&id("gpufail")),
        engine.local_runtime(),
        &CancellationToken::new(),
        false
    )
    .await
    .is_err());
    assert_eq!(
        lines(&f.bin.join("launches.jsonl")).len(),
        before_strict + 1,
        "strict comparison policy must not launch a CPU retry"
    );
    let prepared = engine
        .prepare_model_client(&cfg, &model_for(&id("gpufail")), &CancellationToken::new())
        .await
        .unwrap();
    drop(prepared);
    let loaded = engine.local_runtime().loaded_json().unwrap();
    assert_eq!(loaded["cpu_fallback"], true);
    assert_eq!(loaded["backend"], "cpu");
    assert!(loaded["fallback_reason"]
        .as_str()
        .unwrap()
        .contains("fake device lost"));
    let rows = local_engine::picker_rows(
        &local_engine::catalog_with(&cfg.local_engine, Some(engine.local_runtime())),
        "",
    );
    let row = rows.iter().find(|r| r["id"] == id("gpufail")).unwrap();
    assert!(row["reason"]
        .as_str()
        .unwrap()
        .contains("CPU fallback (GPU load failed)"));
    let last = lines(&f.bin.join("launches.jsonl"));
    let last_argv = last.last().unwrap()["argv"].to_string();
    assert!(last_argv.contains("\"--device\",\"none\",\"-ngl\",\"0\""));
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn ollama_manifest_import_references_blobs_without_copying() {
    let f = fixture(GPU);
    let store = f.models.join("ollama");
    let blobs = store.join("blobs");
    fs::create_dir_all(&blobs).unwrap();
    let digest = |c: char| format!("sha256:{}", c.to_string().repeat(64));
    let blob = |c: char| blobs.join(format!("sha256-{}", c.to_string().repeat(64)));
    qwen_like(&blob('a'), "qwen3", TOOLS_TEMPLATE);
    qwen_like(&blob('b'), "gemma4", TOOLS_TEMPLATE);
    projector(&blob('c'));
    qwen_like(&blob('d'), "gptoss", TOOLS_TEMPLATE);
    qwen_like(&blob('e'), "gemma4", TOOLS_TEMPLATE);
    let manifest = |ns: &str, repo: &str, tag: &str, layers: Value| {
        let dir = store
            .join("manifests/registry.ollama.ai")
            .join(ns)
            .join(repo);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(tag),
            json!({"schemaVersion":2,"layers":layers}).to_string(),
        )
        .unwrap();
    };
    manifest(
        "library",
        "qwen3",
        "14b",
        json!([{"mediaType":"application/vnd.ollama.image.model","digest":digest('a')},{"mediaType":"application/vnd.ollama.image.template","digest":digest('f')}]),
    );
    manifest(
        "huihui_ai",
        "gemma-4-abliterated",
        "12b",
        json!([{"mediaType":"application/vnd.ollama.image.model","digest":digest('b')},{"mediaType":"application/vnd.ollama.image.projector","digest":digest('c')}]),
    );
    manifest(
        "library",
        "gpt-oss",
        "20b",
        json!([{"mediaType":"application/vnd.ollama.image.model","digest":digest('d')}]),
    );
    manifest(
        "library",
        "noproj",
        "1b",
        json!([{"mediaType":"application/vnd.ollama.image.model","digest":digest('e')},{"mediaType":"application/vnd.ollama.image.projector","digest":digest('9')}]),
    );
    let snapshot = |dir: &Path| {
        let mut v: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .flatten()
            .map(|e| {
                use std::os::unix::fs::MetadataExt;
                let m = e.metadata().unwrap();
                (e.file_name(), m.ino(), m.len(), m.modified().unwrap())
            })
            .collect();
        v.sort();
        v
    };
    let before = snapshot(&blobs);
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let root = store.display().to_string();
    let imported = call(
        &service,
        "POST",
        "/api/local-models/import-ollama",
        json!({"tag":"qwen3:14b","root":root}),
    )
    .await
    .unwrap();
    let models = imported["local_engine"]["models"].as_array().unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["name"], "qwen3:14b");
    assert_eq!(models[0]["source"], "ollama");
    assert_eq!(models[0]["path"], blob('a').display().to_string());
    assert_eq!(models[0]["vision"], false);
    let imported = call(
        &service,
        "POST",
        "/api/local-models/import-ollama",
        json!({"tag":"huihui_ai/gemma-4-abliterated:12b","root":root}),
    )
    .await
    .unwrap();
    let gemma = imported["local_engine"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "huihui_ai/gemma-4-abliterated:12b")
        .unwrap()
        .clone();
    assert_eq!(gemma["vision"], true);
    assert_eq!(gemma["mmproj"], blob('c').display().to_string());
    let error = call(
        &service,
        "POST",
        "/api/local-models/import-ollama",
        json!({"tag":"gpt-oss:20b","root":root}),
    )
    .await
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("unsupported architecture gptoss"),
        "{error}"
    );
    let imported = call(
        &service,
        "POST",
        "/api/local-models/import-ollama",
        json!({"tag":"noproj:1b","root":root}),
    )
    .await
    .unwrap();
    let noproj = imported["local_engine"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["name"] == "noproj:1b")
        .unwrap()
        .clone();
    assert_eq!(
        noproj["vision"], false,
        "missing projector blob → text only"
    );
    assert!(noproj["reason"].as_str().unwrap().contains("projector"));
    assert_eq!(snapshot(&blobs), before, "the store is never written");
    assert!(!store
        .join("manifests/registry.ollama.ai/library/qwen3/14b.shadowcode")
        .exists());
    let tags: Vec<_> = shadowcode_core::ollama_store::list(&store)
        .into_iter()
        .map(|m| m.tag)
        .collect();
    assert_eq!(
        tags,
        [
            "huihui_ai/gemma-4-abliterated:12b",
            "gpt-oss:20b",
            "noproj:1b",
            "qwen3:14b"
        ]
    );
}

#[tokio::test]
async fn native_agent_turns_use_the_local_server_with_its_key_and_capabilities() {
    let f = fixture(GPU);
    let coder = f.models.join("coder.gguf");
    qwen_like(&coder, "qwen3", TOOLS_TEMPLATE);
    let chat = f.models.join("chat.gguf");
    qwen_like(&chat, "llama", "{{ messages }}");
    let vdir = f.models.join("vision");
    fs::create_dir_all(&vdir).unwrap();
    let vision = vdir.join("vision.gguf");
    qwen_like(&vision, "gemma4", TOOLS_TEMPLATE);
    projector(&vdir.join("vision.mmproj.gguf"));
    Config::patch(
        &f.paths,
        json!({"local_engine":{"files":[coder.display().to_string(), chat.display().to_string(), vision.display().to_string()]}}),
    )
    .unwrap();
    fs::write(f.project.join("hello.txt"), "hello\n").unwrap();
    // 1x1 red PNG.
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x02, 0x00, 0x00, 0x00, 0x90,
        0x77, 0x53, 0xDE, 0x00, 0x00, 0x00, 0x0C, 0x49, 0x44, 0x41, 0x54, 0x08, 0xD7, 0x63, 0xF8,
        0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x00, 0x05, 0xFE, 0x02, 0xFE, 0xDC, 0xCC,
        0x59, 0xE7, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    fs::write(f.project.join("red.png"), &png).unwrap();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let picker = call(&service, "GET", "/api/picker", Value::Null)
        .await
        .unwrap();
    let id = |name: &str| {
        picker["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["model"] == name)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let requests = f.bin.join("requests.jsonl");

    // Tool-capable coder: read_file round trip, bearer on every request,
    // thinking switched off for templates that support it.
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "What does hello.txt say?", "model": id("coder")}),
    )
    .await
    .unwrap();
    let done = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert!(done["summary"]
        .as_str()
        .unwrap()
        .contains("hello from the fake model"));
    let seen = lines(&requests);
    assert_eq!(seen.len(), 2);
    assert!(seen.iter().all(|r| r["auth"] == true));
    assert!(seen[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "read_file"));
    assert!(!seen[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "view_image"));
    assert_eq!(seen[0]["kwargs"], json!({"enable_thinking": false}));
    assert_eq!(seen[0]["type_list"], false, "type lists are sent as anyOf");
    assert!(seen[0]["max_tokens"].as_u64().unwrap() <= 4096);

    // Images on a non-vision row are refused before a job exists.
    let jobs_before = call(&service, "GET", "/api/jobs", Value::Null)
        .await
        .unwrap()["jobs"]
        .as_array()
        .unwrap()
        .len();
    let error = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "Describe", "model": id("coder"), "images":["red.png"]}),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("cannot read images"), "{error}");
    let jobs_after = call(&service, "GET", "/api/jobs", Value::Null)
        .await
        .unwrap()["jobs"]
        .as_array()
        .unwrap()
        .len();
    assert_eq!(jobs_before, jobs_after);

    // Chat-only template: no tool schemas are sent at all.
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "Say hi", "model": id("chat")}),
    )
    .await
    .unwrap();
    let done = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    let seen = lines(&requests);
    assert_eq!(seen.last().unwrap()["tools"], json!([]));

    // Vision row: view_image is offered and the picture reaches the server
    // as an image_url data URI on the next turn.
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "What colour is red.png?", "model": id("vision")}),
    )
    .await
    .unwrap();
    let done = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    assert!(
        done["summary"]
            .as_str()
            .unwrap()
            .contains("The image is red"),
        "{done} {:?}",
        lines(&requests)
    );
    let seen = lines(&requests);
    let vision_requests = &seen[seen.len() - 2..];
    assert!(vision_requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t == "view_image"));
    assert_eq!(vision_requests[1]["image"], true);

    // An attached image on the vision row passes the precheck.
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "Look", "model": id("vision"), "images":["red.png"]}),
    )
    .await
    .unwrap();
    wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(lines(&requests).last().unwrap()["image"], true);
    let catalog = call(&service, "GET", "/api/local-models", Value::Null)
        .await
        .unwrap();
    assert_eq!(catalog["loaded"]["in_use"], 0);
    let unloaded = call(&service, "POST", "/api/local-models/unload", json!({}))
        .await
        .unwrap();
    assert_eq!(unloaded["unloaded"], true);
    let loaded = call(
        &service,
        "POST",
        "/api/local-models/load",
        json!({"id": id("coder")}),
    )
    .await
    .unwrap();
    assert_eq!(loaded["loaded"]["name"], "coder");
    assert_eq!(loaded["loaded"]["context_tokens"], 16384);
}

// ---------------------------------------------------------------------------
// Live run on this computer's GPU. Ignored by default; run with
//   SHADOWCODE_LIVE_LLAMA=1 SHADOWCODE_LLAMA_SERVER=/path/to/llama-server \
//   cargo test -p shadowcode-core --test local_engine live_ -- --ignored --nocapture
// It imports qwen3:14b and gemma-4 from the Ollama store by reference (read
// only), runs a real native agent task on Qwen3 (edit + test via exec, auto
// approved in this harness), a vision task on Gemma 4 with a generated PNG,
// and measures generation speed. HTTP(S)_PROXY point at a dead port to show
// local inference needs no network.
// ---------------------------------------------------------------------------

fn write_png(path: &Path, rgb: [u8; 3]) {
    let script = format!(
        "import struct, zlib, sys\nw=h=64\nrow=b'\\x00'+bytes([{},{},{}])*w\nraw=row*h\ndef chunk(t,d):\n    return struct.pack('>I',len(d))+t+d+struct.pack('>I',zlib.crc32(t+d)&0xffffffff)\nopen(sys.argv[1],'wb').write(b'\\x89PNG\\r\\n\\x1a\\n'+chunk(b'IHDR',struct.pack('>IIBBBBB',w,h,8,2,0,0,0))+chunk(b'IDAT',zlib.compress(raw))+chunk(b'IEND',b''))\n",
        rgb[0], rgb[1], rgb[2]
    );
    let status = std::process::Command::new("python3")
        .arg("-c")
        .arg(script)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

async fn live_job(
    service: &Service,
    project: &Path,
    task: &str,
    model: &str,
    images: Value,
) -> Value {
    let started = Instant::now();
    let job = call(
        service,
        "POST",
        "/api/jobs",
        json!({"workspace": project, "task": task, "model": model, "images": images}),
    )
    .await
    .unwrap();
    let id = job["id"].as_str().unwrap().to_owned();
    let deadline = Instant::now() + Duration::from_secs(900);
    let done = loop {
        let job = call(service, "GET", &format!("/api/jobs/{id}"), Value::Null)
            .await
            .unwrap();
        if matches!(
            job["status"].as_str(),
            Some("completed" | "failed" | "cancelled")
        ) {
            break job;
        }
        assert!(Instant::now() < deadline, "live job timed out: {job}");
        tokio::time::sleep(Duration::from_millis(500)).await;
    };
    let events = call(
        service,
        "GET",
        &format!(
            "/api/events?session_id={}&limit=2000",
            done["session_id"].as_str().unwrap()
        ),
        Value::Null,
    )
    .await
    .unwrap();
    let tools: Vec<String> = events["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["type"] == "tool.completed" && e["task_id"] == done["task_id"])
        .map(|e| {
            format!(
                "{}{}",
                e["payload"]["tool"].as_str().unwrap_or("?"),
                if e["payload"]["success"] == true {
                    ""
                } else {
                    "(failed)"
                }
            )
        })
        .collect();
    println!(
        "LIVE job model={} status={} steps={} secs={:.1} usage={} tools={:?}\nLIVE summary: {}",
        done["model"],
        done["status"],
        done["steps"],
        started.elapsed().as_secs_f64(),
        done["usage"],
        tools,
        done["summary"]
    );
    done
}

/// Generation speed straight from llama-server's own timings.
async fn live_speed(spec: shadowcode_core::local_runtime::LaunchSpec) {
    let runtime = shadowcode_core::local_runtime::LocalRuntime::new();
    let started = Instant::now();
    let (loaded, lease) = runtime
        .acquire(spec, &CancellationToken::new())
        .await
        .unwrap();
    println!(
        "LIVE load name={} secs={:.1} backend={} ctx={} vision={} cpu_fallback={}",
        loaded.name,
        started.elapsed().as_secs_f64(),
        loaded.backend,
        loaded.ctx,
        loaded.vision,
        loaded.cpu_fallback
    );
    let client = shadowcode_core::local_runtime::loopback_client(Duration::from_secs(300)).unwrap();
    for thinking_off in [true, false] {
        let mut body = json!({
            "messages":[{"role":"user","content":"Write a short paragraph about the history of the Rust programming language."}],
            "max_tokens": 256,
            "stream": false
        });
        if thinking_off {
            body["chat_template_kwargs"] = json!({"enable_thinking": false});
        }
        let response: Value = client
            .post(format!("{}/chat/completions", loaded.endpoint))
            .bearer_auth(&loaded.api_key)
            .json(&body)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let message = &response["choices"][0]["message"];
        println!(
            "LIVE speed name={} enable_thinking_false={} predicted_per_second={} prompt_per_second={} content_chars={} reasoning_chars={}",
            loaded.name,
            thinking_off,
            response["timings"]["predicted_per_second"],
            response["timings"]["prompt_per_second"],
            message["content"].as_str().map_or(0, str::len),
            message["reasoning_content"].as_str().map_or(0, str::len),
        );
    }
    drop(lease);
    runtime.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "runs real models on this computer's GPU"]
async fn live_qwen3_agent_and_gemma4_vision_from_the_ollama_store() {
    if std::env::var_os("SHADOWCODE_LIVE_LLAMA").is_none() {
        eprintln!("set SHADOWCODE_LIVE_LLAMA=1 to run");
        return;
    }
    let server = PathBuf::from(
        std::env::var_os("SHADOWCODE_LLAMA_SERVER").expect("SHADOWCODE_LLAMA_SERVER"),
    );
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir_all(&project).unwrap();
    fs::write(
        project.join("calc.py"),
        "def add(a, b):\n    \"\"\"Return the sum of a and b.\"\"\"\n    return a - b\n",
    )
    .unwrap();
    fs::write(
        project.join("test_calc.py"),
        "import unittest\n\nfrom calc import add\n\n\nclass AddTest(unittest.TestCase):\n    def test_add(self):\n        self.assertEqual(add(2, 3), 5)\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n",
    )
    .unwrap();
    write_png(&project.join("square.png"), [220, 20, 20]);
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(
        &paths,
        json!({
            "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"unused","context_limit":16384},
            "permissions":{"approve_shell":false},
            "trusted_workspaces":[project.clone()],
            "local_engine":{"llama_binary": server.display().to_string()}
        }),
    )
    .unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    for tag in ["qwen3:14b", "huihui_ai/gemma-4-abliterated:12b"] {
        call(
            &service,
            "POST",
            "/api/local-models/import-ollama",
            json!({ "tag": tag }),
        )
        .await
        .unwrap();
    }
    let error = call(
        &service,
        "POST",
        "/api/local-models/import-ollama",
        json!({"tag":"gpt-oss:20b"}),
    )
    .await
    .unwrap_err();
    println!("LIVE gpt-oss import refused: {error}");
    let catalog = call(&service, "GET", "/api/local-models", Value::Null)
        .await
        .unwrap();
    println!(
        "LIVE hardware={} runtime={}",
        catalog["hardware"], catalog["runtime"]
    );
    for model in catalog["models"].as_array().unwrap() {
        println!(
            "LIVE model name={} arch={} ctx={} fits={} vision={} tools={} availability={} reason={}",
            model["name"],
            model["architecture"],
            model["context_tokens"],
            model["fits"],
            model["vision"],
            model["tools"],
            model["availability"],
            model["reason"]
        );
    }
    for store_model in catalog["ollama_store"]["models"].as_array().unwrap() {
        println!(
            "LIVE store tag={} compatible={} added={} reason={}",
            store_model["tag"],
            store_model["compatible"],
            store_model["already_added"],
            store_model["reason"]
        );
    }
    let find = |name: &str| {
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .unwrap()
            .clone()
    };
    let qwen = find("qwen3:14b");
    let gemma = find("huihui_ai/gemma-4-abliterated:12b");

    // Local inference must not need the network: every proxy is dead from
    // here on (the Ollama store import above is file-only anyway).
    for key in [
        "HTTP_PROXY",
        "HTTPS_PROXY",
        "http_proxy",
        "https_proxy",
        "ALL_PROXY",
    ] {
        std::env::set_var(key, "http://127.0.0.1:9");
    }

    // Qwen3 14B: fix a failing test with real tool calls.
    let done = live_job(
        &service,
        &project,
        "test_calc.py fails. Fix the bug in calc.py, then run `python3 -m unittest -v` with the exec tool to confirm the test passes.",
        qwen["id"].as_str().unwrap(),
        json!([]),
    )
    .await;
    let fixed = fs::read_to_string(project.join("calc.py")).unwrap();
    let verify = std::process::Command::new("python3")
        .args(["-m", "unittest", "-q"])
        .current_dir(&project)
        .output()
        .unwrap();
    println!(
        "LIVE qwen status={} calc.py now:\n{}\nLIVE independent unittest exit={} {}",
        done["status"],
        fixed,
        verify.status,
        String::from_utf8_lossy(&verify.stderr).trim()
    );
    let loaded = call(&service, "GET", "/api/local-models", Value::Null)
        .await
        .unwrap()["loaded"]
        .clone();
    println!("LIVE loaded after qwen job: {loaded}");

    // Gemma 4 12B: attached image and view_image.
    let attached = live_job(
        &service,
        &project,
        "What is the main colour of this image? Answer with one word.",
        gemma["id"].as_str().unwrap(),
        json!(["square.png"]),
    )
    .await;
    let viewed = live_job(
        &service,
        &project,
        "Use the view_image tool to look at square.png, then tell me its main colour in one word.",
        gemma["id"].as_str().unwrap(),
        json!([]),
    )
    .await;
    let loaded = call(&service, "GET", "/api/local-models", Value::Null)
        .await
        .unwrap()["loaded"]
        .clone();
    println!("LIVE loaded after gemma jobs: {loaded}");
    call(&service, "POST", "/api/local-models/unload", json!({}))
        .await
        .unwrap();

    // Raw speed with the same runtime and argv.
    for entry in [&qwen, &gemma] {
        live_speed(shadowcode_core::local_runtime::LaunchSpec {
            id: entry["id"].as_str().unwrap().into(),
            name: entry["name"].as_str().unwrap().into(),
            binary: server.clone(),
            model: entry["path"].as_str().unwrap().into(),
            mmproj: entry["mmproj"].as_str().map(PathBuf::from),
            ctx: entry["context_tokens"].as_u64().unwrap(),
            gpu: shadowcode_core::local_runtime::GpuMode::All,
            backend: "vulkan".into(),
        })
        .await;
    }
    assert_eq!(done["status"], "completed", "{done}");
    assert!(verify.status.success(), "the fix must make the test pass");
    assert_eq!(attached["status"], "completed", "{attached}");
    assert_eq!(viewed["status"], "completed", "{viewed}");
    drop(service);
}

/// A subscription hits its plan limit: with `limits.on_limit = "local"` (the
/// default) the same conversation continues on a local model on its own;
/// with "ask" nothing starts and the transcript says so.
#[tokio::test]
async fn a_plan_limit_continues_the_conversation_on_a_local_model() {
    let f = fixture(GPU);
    let coder = f.models.join("coder.gguf");
    qwen_like(&coder, "qwen3", TOOLS_TEMPLATE);
    fs::write(f.project.join("hello.txt"), "hello\n").unwrap();
    let fake_root = tempfile::tempdir().unwrap();
    let fake =
        vendor_support::FakeCodex::new(fake_root.path(), json!({"auth":"chatgpt","turn":"limit"}));
    Config::patch(
        &f.paths,
        json!({
            "local_engine":{"files":[coder.display().to_string()]},
            "cli_agents": vendor_support::cli_agents(&fake),
        }),
    )
    .unwrap();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let picker = call(&service, "GET", "/api/picker", Value::Null)
        .await
        .unwrap();
    let local_id = picker["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["model"] == "coder")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    // The Allowance view already names the model a limit would fall back to.
    let allowance = call(&service, "GET", "/api/allowance", Value::Null)
        .await
        .unwrap();
    let local_row = allowance["rows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "local")
        .unwrap()
        .clone();
    assert_eq!(local_row["fallback"]["id"], json!(local_id));
    assert_eq!(local_row["on_limit"], "local");

    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "What does hello.txt say?", "model": "cli:codex"}),
    )
    .await
    .unwrap();
    let limited = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(limited["status"], "limit_reached", "{limited}");
    let session = limited["session_id"].as_str().unwrap().to_owned();
    // The follow-up job appears in the same conversation and finishes locally.
    let store = service.engine.store();
    let mut follow_up = None;
    for _ in 0..300 {
        let events = store.events_after(&session, 0, None, 10_000).unwrap();
        if let Some(e) = events.iter().find(|e| e["type"] == "limit.fallback") {
            follow_up = Some(e["payload"].clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let follow_up = follow_up.expect("a limit.fallback event is recorded");
    assert_eq!(follow_up["ok"], true, "{follow_up}");
    assert_eq!(follow_up["from"], "Codex");
    assert_eq!(follow_up["target"], json!(local_id));
    let next = wait_job(&service, follow_up["job_id"].as_str().unwrap()).await;
    assert_eq!(next["status"], "completed", "{next}");
    assert_eq!(next["session_id"], json!(session));
    assert!(next["task"]
        .as_str()
        .unwrap()
        .starts_with("Continue where Codex stopped"));
    assert_eq!(
        store.session_meta(&session, "execution_target").unwrap(),
        Some(local_id.clone()),
        "the conversation now stays on the local model"
    );

    // "ask": the limit stops the conversation and nothing else starts.
    Config::patch(&f.paths, json!({"limits":{"on_limit":"ask"}})).unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "Again", "model": "cli:codex", "handoff_consent": true}),
    )
    .await
    .unwrap();
    let limited = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(limited["status"], "limit_reached");
    let session = limited["session_id"].as_str().unwrap().to_owned();
    let mut asked = None;
    for _ in 0..200 {
        let events = store.events_after(&session, 0, None, 10_000).unwrap();
        if let Some(e) = events
            .iter()
            .find(|e| e["type"] == "limit.fallback" && e["task_id"] == limited["task_id"])
        {
            asked = Some(e["payload"].clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(asked.expect("ask is recorded")["ask"], true);
    let jobs = call(&service, "GET", "/api/jobs", Value::Null)
        .await
        .unwrap();
    let later = jobs["jobs"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| {
            j["session_id"] == json!(session)
                && j["started_at"].as_f64() > limited["started_at"].as_f64()
        })
        .count();
    assert_eq!(later, 0, "no follow-up job in ask mode");
    assert!(Config::patch(&f.paths, json!({"limits":{"on_limit":"sometimes"}})).is_err());
}

#[tokio::test]
async fn managed_jobs_run_in_submission_order_across_projects_and_skip_cancelled_entries() {
    use shadowcode_core::engine::StartRequest;
    let f = fixture(GPU);
    let a = f.models.join("a.gguf");
    let b = f.models.join("b.gguf");
    qwen_like(&a, "qwen3", TOOLS_TEMPLATE);
    qwen_like(&b, "qwen3", TOOLS_TEMPLATE);
    let projects: Vec<_> = (0..3)
        .map(|n| f._root.path().join(format!("queue-{n}")))
        .collect();
    for project in &projects {
        fs::create_dir(project).unwrap();
        fs::write(project.join("hello.txt"), "hello\n").unwrap();
    }
    Config::patch(
        &f.paths,
        json!({"local_engine":{"files":[a,b]},"trusted_workspaces":projects}),
    )
    .unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let entries = local_engine::scan(&cfg.local_engine);
    let id = |name| {
        entries
            .iter()
            .find(|entry| entry.name == name)
            .unwrap()
            .id
            .clone()
    };
    let engine = Engine::open(f.paths.clone()).unwrap();
    // Hold A to stop the first submitted B task at the runtime boundary.
    let held = engine
        .prepare_model_client(&cfg, &model_for(&id("a")), &CancellationToken::new())
        .await
        .unwrap();
    let request = |project: PathBuf, model: ModelConfig| StartRequest {
        workspace: project,
        task: "Read hello.txt".into(),
        session_id: None,
        model: Some(model),
        mode: "code".into(),
        queue: false,
        images: vec![],
        web: false,
    };
    let first = engine
        .start(request(projects[0].clone(), model_for(&id("b"))))
        .await
        .unwrap();
    let middle = engine
        .start(request(projects[1].clone(), model_for(&id("a"))))
        .await
        .unwrap();
    let last = engine
        .start(request(projects[2].clone(), model_for(&id("a"))))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(engine.job(&middle.id).unwrap().unwrap().status, "queued");
    assert_eq!(engine.job(&last.id).unwrap().unwrap().status, "queued");
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 1);
    let cancelled = tokio::time::timeout(Duration::from_secs(2), engine.cancel_queued(&middle.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, "cancelled");
    drop(held);
    let first = tokio::time::timeout(Duration::from_secs(15), engine.wait(&first.id))
        .await
        .unwrap()
        .unwrap();
    let last = tokio::time::timeout(Duration::from_secs(15), engine.wait(&last.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.status, "completed", "{}", first.summary);
    assert_eq!(last.status, "completed", "{}", last.summary);
    assert!(last.started_at >= first.finished_at.unwrap());
    assert_eq!(engine.job(&middle.id).unwrap().unwrap().status, "cancelled");
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 3);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn compare_runs_two_installed_gguf_models_sequentially_in_offline_mode() {
    let f = fixture(GPU);
    let a = f.models.join("a.gguf");
    let b = f.models.join("b.gguf");
    let c = f.models.join("c.gguf");
    qwen_like(&c, "qwen3", TOOLS_TEMPLATE);
    qwen_like(&a, "qwen3", TOOLS_TEMPLATE);
    qwen_like(&b, "qwen3", TOOLS_TEMPLATE);
    fs::write(f.project.join("hello.txt"), "hello\n").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "base",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&f.project)
            .status()
            .unwrap()
            .success());
    }
    Config::patch(&f.paths,json!({"local_engine":{"files":[a,b,c]},"network":{"mode":"offline"},"cli_agents":{"enabled":false}})).unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let entries = local_engine::scan(&cfg.local_engine);
    let ids: Vec<_> = ["a", "b"]
        .iter()
        .map(|name| entries.iter().find(|e| e.name == *name).unwrap().id.clone())
        .collect();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let record = call(
        &service,
        "POST",
        "/api/compare",
        json!({"workspace":f.project,"task":"Read hello.txt","models":ids,"web":false}),
    )
    .await
    .unwrap();
    let record_id = record["id"].as_str().unwrap();
    let lanes = record["lanes"].as_array().unwrap();
    let mut jobs = Vec::new();
    for lane in lanes {
        jobs.push(
            tokio::time::timeout(
                Duration::from_secs(15),
                service.engine.wait(lane["job_id"].as_str().unwrap()),
            )
            .await
            .unwrap()
            .unwrap(),
        );
    }
    assert!(jobs.iter().all(|job| job.status == "completed"), "{jobs:?}");
    assert!(jobs[1].started_at >= jobs[0].finished_at.unwrap());
    let result = call(
        &service,
        "GET",
        &format!("/api/compare/{record_id}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(result["state"], "done");
    for (index, lane) in result["lanes"].as_array().unwrap().iter().enumerate() {
        assert_eq!(lane["local_runtime"]["model_id"], ids[index]);
        assert_eq!(
            lane["local_runtime"]["automatic_cpu_fallback_allowed"],
            false
        );
        assert_eq!(lane["local_runtime"]["runtime"]["cpu_fallback"], false);
        assert_eq!(lane["base_commit"], result["base"]["commit"]);
    }
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 2);
    call(
        &service,
        "POST",
        &format!("/api/compare/{record_id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    let c_id = entries
        .iter()
        .find(|entry| entry.name == "c")
        .unwrap()
        .id
        .clone();
    let held = service
        .engine
        .prepare_model_client(&cfg, &model_for(&c_id), &CancellationToken::new())
        .await
        .unwrap();
    let cancelled = call(&service,"POST","/api/compare",json!({"workspace":f.project,"task":"Read hello.txt","models":[ids[0],ids[1],c_id],"web":false})).await.unwrap();
    let cancelled_id = cancelled["id"].as_str().unwrap();
    call(
        &service,
        "POST",
        &format!("/api/compare/{cancelled_id}/cancel"),
        Value::Null,
    )
    .await
    .unwrap();
    for lane in cancelled["lanes"].as_array().unwrap() {
        let job = tokio::time::timeout(
            Duration::from_secs(5),
            service.engine.wait(lane["job_id"].as_str().unwrap()),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(job.status, "cancelled");
    }
    drop(held);
    assert_eq!(
        lines(&f.bin.join("launches.jsonl")).len(),
        3,
        "cancelled queued models must not load"
    );
    call(
        &service,
        "POST",
        &format!("/api/compare/{cancelled_id}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    service.engine.shutdown().await.unwrap();
}

/// Opt-in real inference acceptance. Set explicit model paths; never downloads
/// weights or imports the user's account/profile. Run in a network namespace
/// with loopback enabled for an enforced offline qualification.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires explicit installed GGUF paths and a real llama-server"]
async fn live_local_acceptance_from_explicit_models() {
    let files: Vec<String> = serde_json::from_str(
        &std::env::var("SHADOWCODE_LIVE_GGUF_FILES")
            .expect("SHADOWCODE_LIVE_GGUF_FILES JSON array"),
    )
    .unwrap();
    assert!((1..=3).contains(&files.len()));
    let server = std::env::var("SHADOWCODE_LLAMA_SERVER").expect("SHADOWCODE_LLAMA_SERVER");
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(
        project.join("calc.py"),
        "def add(a, b):\n    return a + b\n",
    )
    .unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "."],
        vec![
            "-c",
            "user.name=Offline Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "acceptance fixture",
        ],
    ] {
        assert!(std::process::Command::new("git")
            .args(args)
            .current_dir(&project)
            .status()
            .unwrap()
            .success());
    }
    let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
    Config::patch(&paths,json!({
        "model":{"provider":"local","endpoint":"http://127.0.0.1:9/v1","name":"unused","context_limit":4096},
        "local_engine":{"llama_binary":server,"files":files,"context_size":4096},
        "network":{"mode":"offline"},"cli_agents":{"enabled":false},
        "trusted_workspaces":[project],"agent":{"max_steps":4}
    })).unwrap();
    let service = Service::open(paths.clone(), Some(project.clone())).unwrap();
    let catalog = call(&service, "GET", "/api/local-models", Value::Null)
        .await
        .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(models.len(), files.len(), "{catalog}");
    let ids: Vec<_> = files
        .iter()
        .map(|file| {
            models.iter().find(|model| model["path"] == *file).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let task = "In one short sentence, explain this Python function: def add(a, b): return a + b. Do not call tools.";
    let (job_ids, comparison) = if ids.len() > 1 {
        let record = call(
            &service,
            "POST",
            "/api/compare",
            json!({"workspace":project,"task":task,"models":ids,"mode":"ask","web":false}),
        )
        .await
        .unwrap();
        (
            record["lanes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|lane| lane["job_id"].as_str().unwrap().to_owned())
                .collect::<Vec<_>>(),
            Some(record["id"].as_str().unwrap().to_owned()),
        )
    } else {
        let job = call(
            &service,
            "POST",
            "/api/jobs",
            json!({"workspace":project,"task":task,"model":ids[0],"mode":"ask","web":false}),
        )
        .await
        .unwrap();
        (vec![job["id"].as_str().unwrap().to_owned()], None)
    };
    let mut jobs = Vec::new();
    for id in &job_ids {
        match tokio::time::timeout(Duration::from_secs(180), service.engine.wait(id)).await {
            Ok(job) => jobs.push(job.unwrap()),
            Err(_) => {
                let _ =
                    tokio::time::timeout(Duration::from_secs(10), service.engine.cancel(id)).await;
                service.engine.shutdown().await.unwrap();
                panic!("live local job timed out");
            }
        }
    }
    let mut runtimes = Vec::new();
    for job in &jobs {
        runtimes.push(
            service
                .engine
                .store()
                .last_task_event(&job.task_id, "local.runtime_ready")
                .unwrap(),
        );
    }
    let report = json!({"scope":if comparison.is_some(){"real multi-model Compare inference"}else{"real single-model inference only"},"runtime":catalog["runtime"],"hardware":catalog["hardware"],"models":models,"jobs":jobs,"runtime_events":runtimes});
    if let Ok(output) = std::env::var("SHADOWCODE_LIVE_REPORT") {
        fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!(
        "LIVE_ACCEPTANCE {}",
        serde_json::to_string(&report).unwrap()
    );
    if let Some(id) = comparison {
        call(
            &service,
            "POST",
            &format!("/api/compare/{id}/discard"),
            Value::Null,
        )
        .await
        .unwrap();
    }
    service.engine.shutdown().await.unwrap();
    for job in &jobs {
        assert_eq!(job.status, "completed", "{}", job.summary);
        assert!(!job.summary.trim().is_empty());
    }
    for pair in jobs.windows(2) {
        assert!(pair[1].started_at >= pair[0].finished_at.unwrap());
    }
}
