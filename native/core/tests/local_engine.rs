//! Built-in local engine against a fake `llama-server` (a small Python
//! script). The fake answers `--version` / `--list-devices`, serves `/health`,
//! `/props`, and `/v1/chat/completions` (canned tool calls), requires the
//! per-launch bearer key, records every launch and request, and exits on
//! SIGTERM. Nothing here touches a GPU or a real model.
mod support;
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
import json, os, signal, socket, sys, threading, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]
if args == ["--post-ready-sentinel"]:
    open(os.path.join(HERE, "post-ready-sentinel-ready"), "w").close()
    while True:
        signal.pause()

def log(name, value):
    with open(os.path.join(HERE, name), "a") as f:
        f.write(json.dumps(value) + "\n")

if args[:1] == ["--foreign-health-props"]:
    foreign_port = int(args[1])
    class Foreign(BaseHTTPRequestHandler):
        def log_message(self, *_):
            pass
        def do_GET(self):
            if self.path == "/props":
                with open(os.path.join(HERE, "foreign-props-observed.json"), "w") as f:
                    json.dump({"path": self.path}, f)
                code, body = 401, {"error": "foreign fixture rejects this launch key"}
            elif self.path == "/fixture/sentinel":
                code, body = 200, {"foreign_listener": "still owned by fixture"}
            else:
                code, body = 200, {"status": "ok"}
            data = json.dumps(body).encode()
            self.send_response(code)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(data)))
            self.end_headers()
            self.wfile.write(data)
    foreign = ThreadingHTTPServer(("127.0.0.1", foreign_port), Foreign)
    with open(os.path.join(HERE, "foreign-listener-ready.json"), "w") as f:
        json.dump({"pid": os.getpid(), "port": foreign_port}, f)
    foreign.serve_forever()
    sys.exit(0)

if args == ["--version"]:
    if os.path.exists(os.path.join(HERE, "hold-probe")):
        open(os.path.join(HERE, "probe-started"), "w").close()
        while not os.path.exists(os.path.join(HERE, "release-probe")):
            time.sleep(0.01)
        open(os.path.join(HERE, "probe-finished"), "w").close()
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
if "holdload" in name:
    while not os.path.exists(os.path.join(HERE, "release-load")):
        time.sleep(0.01)

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
            reported_template = opt("--chat-template", "runtime template fixture")
            if os.path.exists(os.path.join(HERE, "lexer-template")) and reported_template.endswith("\n"):
                reported_template = reported_template[:-1]
            if os.path.exists(os.path.join(HERE, "wrong-template")):
                reported_template = "private-template-MUST-NOT-LEAK"
            if os.path.exists(os.path.join(HERE, "missing-template")):
                reported_template = None
            if os.path.exists(os.path.join(HERE, "wrong-type-template")):
                reported_template = {"private": "MUST-NOT-LEAK"}
            if os.path.exists(os.path.join(HERE, "whitespace-template")):
                reported_template += " "
            if os.path.exists(os.path.join(HERE, "unavailable-props")):
                return self.reply(503, {"private": "MUST-NOT-LEAK"})
            body = {"default_generation_settings": {"n_ctx": ctx,
                    "params": {"temperature":0.8,"top_k":40,"top_p":0.95,"seed":4294967295,
                               "prompt":"must not appear in provenance"}},
                    "chat_template": reported_template,
                    "modalities": {"vision": mmproj is not None}}
            overrides = os.path.join(HERE, "props-overrides.json")
            if os.path.exists(overrides):
                body.update(json.load(open(overrides)))
            return self.reply(200, body)
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
        if name == "activeexit.gguf":
            if os.path.exists(os.path.join(HERE, "active-exit-armed")):
                if not tool_results:
                    return sse(self, tool_call("write_file", {"path": "retained.txt", "content": "retained before runtime exit\n", "expected_hash": "missing"}))
                if not any(m.get("name") == "write_file" and json.loads(m.get("content", "{} ")).get("success") is True for m in tool_results):
                    return self.reply(500, {"error": "fixture needs successful real write result"})
                barrier = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                barrier_path = os.path.join(HERE, "active-stream-exit-%s.sock" % os.getpid())
                barrier.bind(barrier_path)
                barrier.listen(1)
                # Flush enough distinct visible text to cross the engine's
                # stream batching threshold without a sleep as ordering proof.
                partial = "The earlier fixture edit is retained; this response is still incomplete. " + " ".join("observation_%03d=value_%03d" % (i, i) for i in range(220))
                chunk = tool_call("write_file", {"path": "unexecuted.txt", "content": "must never execute", "expected_hash": "missing"})[0]
                chunk["choices"][0]["delta"]["content"] = partial
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                self.wfile.write(("data: " + json.dumps(chunk) + "\n\n").encode())
                self.wfile.flush()
                with open(os.path.join(HERE, "active-stream-held.json"), "w") as f:
                    json.dump({"pid": os.getpid(), "socket": barrier_path}, f)
                connection, _ = barrier.accept()
                with connection, connection.makefile("rb") as stream:
                    if stream.read(4) != b"exit":
                        os._exit(2)
                sys.stderr.write("active-generation fixture exit\n")
                sys.stderr.flush()
                os._exit(29)
            if not tool_results:
                return sse(self, tool_call("read_file", {"path": "retained.txt"}))
            if not any(m.get("name") == "exec" for m in tool_results):
                return sse(self, tool_call("exec", {"command": "python3 check_retained.py"}))
            return sse(self, text("The retained file passed the configured check."))
        if "hermes-partial" in name:
            chunks = tool_call("write_file", {"path": "unexpected.txt", "content": "never"})
            chunks[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"] = '{"path":"unexpected.txt","content":'
            return sse(self, chunks)
        if "hermes-cut-short" in name:
            chunks = tool_call("write_file", {"path": "unexpected.txt", "content": "never"})
            chunks[1]["choices"][0]["finish_reason"] = "length"
            return sse(self, chunks)
        if "hermes-prose" in name:
            return sse(self, text('<tool_call>{"name":"write_file","arguments":{"path":"unexpected.txt","content":"never"}}</tool_call>'))
        if not tool_results:
            return sse(self, tool_call("read_file", {"path": "hello.txt"}))
        return sse(self, text("The file says hello from the fake model."))

if name == "porthold.gguf" and os.path.exists(os.path.join(HERE, "hold-port-bind")):
    bind_barrier = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    bind_path = os.path.join(HERE, "port-bind-%s.sock" % os.getpid())
    bind_barrier.bind(bind_path)
    bind_barrier.listen(1)
    with open(os.path.join(HERE, "port-bind-held.json"), "w") as f:
        json.dump({"pid": os.getpid(), "port": port, "socket": bind_path}, f)
    connection, _ = bind_barrier.accept()
    with connection, connection.makefile("rb") as stream:
        if stream.read(4) != b"bind":
            sys.exit(2)
    bind_barrier.close()

server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
if name == "postready.gguf":
    barrier = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    barrier.bind(os.path.join(HERE, "post-ready-exit-%s.sock" % os.getpid()))
    barrier.listen(1)
    def exit_after_command():
        connection, _ = barrier.accept()
        with connection, connection.makefile("rb") as stream:
            if stream.read(4) == b"exit":
                sys.stderr.write("post-ready fixture exit\n")
                sys.stderr.flush()
                os._exit(23)
    threading.Thread(target=exit_after_command, daemon=True).start()
server.serve_forever()
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

const HERMES_DEFAULT_TEMPLATE: &str = r#"{{bos_token}}{% for message in messages %}{{'<|im_start|>' + message['role'] + '
' + message['content'] + '<|im_end|>' + '
'}}{% endfor %}{% if add_generation_prompt %}{{ '<|im_start|>assistant
' }}{% endif %}"#;

fn hermes_like(path: &Path, name: &str, template: &str) {
    assert_eq!(
        shadowcode_core::gguf::string_identity(HERMES_DEFAULT_TEMPLATE).sha256,
        "a805e50fed68938a076b07e2e602639611b50b1ced0e50f11eb92f1ba25be4dc"
    );
    write_gguf(
        path,
        &[
            ("general.architecture", V::Str("llama")),
            ("general.name", V::Str(name)),
            ("llama.context_length", V::U32(32768)),
            ("llama.embedding_length", V::U32(1024)),
            ("llama.block_count", V::U32(8)),
            ("llama.attention.head_count", V::U32(16)),
            ("llama.attention.head_count_kv", V::U32(4)),
            ("tokenizer.ggml.model", V::Str("gpt2")),
            ("tokenizer.ggml.pre", V::Str("llama-bpe")),
            ("tokenizer.ggml.bos_token_id", V::U32(128000)),
            ("tokenizer.ggml.eos_token_id", V::U32(128003)),
            ("tokenizer.ggml.padding_token_id", V::U32(128001)),
            ("tokenizer.chat_template", V::Str(template)),
        ],
        &["token_embd.weight", "output.weight"],
    );
}

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

#[test]
fn hermes_capability_comes_from_metadata_and_template_not_the_label() {
    let f = fixture(GPU);
    let matched = f.models.join("unrelated-label.gguf");
    hermes_like(&matched, "Hermes-2-Pro-Llama-3-8B", HERMES_DEFAULT_TEMPLATE);
    let label_only = f.models.join("Hermes-2-Pro-Llama-3-8B.gguf");
    hermes_like(&label_only, "Different model", HERMES_DEFAULT_TEMPLATE);
    let changed = f.models.join("changed.gguf");
    hermes_like(
        &changed,
        "Hermes-2-Pro-Llama-3-8B",
        &format!("{HERMES_DEFAULT_TEMPLATE} "),
    );
    let rhea = f.models.join("Rhea-4B-Coding-max.gguf");
    write_gguf(
        &rhea,
        &[
            ("general.architecture", V::Str("qwen3")),
            ("general.name", V::Str("Rhea-4B-Coding-max")),
            ("qwen3.context_length", V::U32(32768)),
            ("tokenizer.ggml.eos_token_id", V::U32(151645)),
        ],
        &["token_embd.weight", "output.weight"],
    );
    let config = local_engine::LocalEngineConfig {
        files: [&matched, &label_only, &changed, &rhea]
            .map(|p| p.display().to_string())
            .into(),
        llama_binary: f.bin.join("llama-server").display().to_string(),
        ..Default::default()
    };
    let entries = local_engine::scan(&config);
    assert_eq!(entries.len(), 4);
    for entry in entries {
        assert_eq!(
            entry.tools,
            entry.id == local_engine::entry_id(&matched),
            "{entry:?}"
        );
        if entry.tools {
            assert!(entry.tools_reason.contains("verified when the model loads"));
        }
    }
    assert!(lines(&f.bin.join("launches.jsonl")).is_empty());
}

#[tokio::test]
async fn hermes_verified_template_enables_structured_tools_and_retains_provenance() {
    assert_hermes_template_roundtrip(false).await;
}

#[tokio::test]
async fn hermes_pinned_lexer_template_enables_tools_with_actual_applied_provenance() {
    assert_hermes_template_roundtrip(true).await;
}

async fn assert_hermes_template_roundtrip(lexer_report: bool) {
    let f = fixture(GPU);
    if lexer_report {
        fs::write(f.bin.join("lexer-template"), b"").unwrap();
    }
    let model = f.models.join("unrelated-label.gguf");
    hermes_like(&model, "Hermes-2-Pro-Llama-3-8B", HERMES_DEFAULT_TEMPLATE);
    let original = fs::read(&model).unwrap();
    Config::patch(&f.paths, json!({"local_engine":{"files":[model]}})).unwrap();
    fs::write(f.project.join("hello.txt"), "hello\n").unwrap();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace":f.project,"task":"What does hello.txt say?","model":local_engine::entry_id(&model)}),
    ).await.unwrap();
    let done = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(done["status"], "completed", "{done}");
    let requests = lines(&f.bin.join("requests.jsonl"));
    assert_eq!(requests.len(), 2);
    assert!(requests[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|tool| tool == "read_file"));
    let events = service
        .engine
        .store()
        .events_after(done["session_id"].as_str().unwrap(), 0, None, 2000)
        .unwrap();
    assert!(events.iter().any(|e| e["task_id"] == done["task_id"]
        && e["type"] == "tool.completed"
        && e["payload"]["tool"] == "read_file"
        && e["payload"]["success"] == true));
    let profile = shadowcode_core::local_templates::Profile::Hermes2ProLlama3;
    let launches = lines(&f.bin.join("launches.jsonl"));
    assert_eq!(launches.len(), 1);
    let args = launches[0]["argv"].as_array().unwrap();
    let option = args.iter().position(|v| v == "--chat-template").unwrap();
    assert_eq!(args[option + 1], profile.template());
    assert!(args.iter().position(|v| v == "--jinja").unwrap() < option);
    let loaded = service.engine.local_runtime().loaded().unwrap();
    let receipt = &loaded.provenance;
    assert_eq!(
        receipt["model"]["chat_template"],
        json!(shadowcode_core::gguf::string_identity(
            HERMES_DEFAULT_TEMPLATE
        ))
    );
    assert_eq!(receipt["template_override"]["profile"], profile.id());
    let applied = shadowcode_core::gguf::string_identity(if lexer_report {
        profile.template().strip_suffix('\n').unwrap()
    } else {
        profile.template()
    });
    assert_eq!(receipt["template_override"]["template"], json!(applied));
    assert_eq!(
        receipt["template_override"]["source_template"],
        json!(profile.identity())
    );
    assert_eq!(
        receipt["template_override"]["source_sha256"],
        profile.identity().sha256
    );
    assert_eq!(
        receipt["template_override"]["match_kind"],
        if lexer_report {
            "pinned_lexer_exact"
        } else {
            "source_exact"
        }
    );
    assert_eq!(
        receipt["template_override"]["normalization"],
        if lexer_report {
            "single_final_lf_removed"
        } else {
            "none"
        }
    );
    assert_eq!(
        receipt["template_override"]["runtime_template_verified"],
        true
    );
    assert_eq!(receipt["runtime"]["reported_chat_template"], json!(applied));
    assert_eq!(
        fs::read(model).unwrap(),
        original,
        "weights and metadata stay untouched"
    );
    service.engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn hermes_unconfirmed_runtime_template_is_refused_and_reaped() {
    for sentinel in [
        "wrong-template",
        "missing-template",
        "wrong-type-template",
        "whitespace-template",
        "unavailable-props",
    ] {
        let f = fixture(GPU);
        let path = f.models.join("hermes.gguf");
        hermes_like(&path, "Hermes-2-Pro-Llama-3-8B", HERMES_DEFAULT_TEMPLATE);
        fs::write(f.bin.join(sentinel), b"").unwrap();
        Config::patch(&f.paths, json!({"local_engine":{"files":[path]}})).unwrap();
        let cfg = Config::load(&f.paths, None).unwrap();
        local_engine::scan(&cfg.local_engine);
        let engine = Engine::open(f.paths.clone()).unwrap();
        let error = engine
            .prepare_model_client(
                &cfg,
                &model_for(&local_engine::entry_id(&path)),
                &CancellationToken::new(),
            )
            .await
            .err()
            .expect("unconfirmed templates cannot prepare tools");
        if sentinel == "unavailable-props" {
            assert_eq!(
                error.to_string(),
                "llama-server readiness /props returned HTTP 503"
            );
        } else {
            assert!(
                error.to_string().contains("did not confirm the expected"),
                "{error}"
            );
            assert!(error.to_string().contains("expected_template_identities"));
            assert!(error.to_string().contains("reported_template_field_type"));
        }
        assert!(!error.to_string().contains("MUST-NOT-LEAK"));
        assert!(engine.local_runtime().loaded().is_none());
        assert_eq!(engine.local_runtime().in_use(), 0);
        assert!(lines(&f.bin.join("requests.jsonl")).is_empty());
        let launches = lines(&f.bin.join("launches.jsonl"));
        assert_eq!(
            launches.len(),
            1,
            "template failure does not fall back to CPU"
        );
        assert!(!pid_alive(launches[0]["pid"].as_u64().unwrap()));
        engine.shutdown().await.unwrap();
    }
}

#[tokio::test]
async fn hermes_override_requires_matching_metadata_before_server_spawn() {
    use shadowcode_core::local_runtime::{GpuMode, LaunchSpec, LocalRuntime};
    let f = fixture(GPU);
    let path = f.models.join("Hermes-2-Pro-Llama-3-8B.gguf");
    hermes_like(&path, "Different model", HERMES_DEFAULT_TEMPLATE);
    let local = LocalRuntime::new();
    let result = local
        .acquire(
            LaunchSpec {
                id: local_engine::entry_id(&path),
                name: "Hermes-2-Pro-Llama-3-8B".into(),
                binary: f.bin.join("llama-server"),
                model: path,
                mmproj: None,
                ctx: 4096,
                gpu: GpuMode::All,
                backend: "vulkan".into(),
                template_profile: Some(shadowcode_core::local_templates::Profile::Hermes2ProLlama3),
            },
            &CancellationToken::new(),
        )
        .await;
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("metadata no longer matches"));
    assert!(local.loaded().is_none());
    assert!(lines(&f.bin.join("launches.jsonl")).is_empty());
    local.stop().await;
}

#[tokio::test]
async fn hermes_template_change_cannot_reuse_or_wait_on_its_own_lease() {
    use shadowcode_core::local_runtime::{GpuMode, LaunchSpec, LocalRuntime};
    let f = fixture(GPU);
    let path = f.models.join("hermes.gguf");
    hermes_like(&path, "Hermes-2-Pro-Llama-3-8B", HERMES_DEFAULT_TEMPLATE);
    let local = LocalRuntime::new();
    let cancel = CancellationToken::new();
    let mut spec = LaunchSpec {
        id: local_engine::entry_id(&path),
        name: "hermes".into(),
        binary: f.bin.join("llama-server"),
        model: path,
        mmproj: None,
        ctx: 4096,
        gpu: GpuMode::All,
        backend: "vulkan".into(),
        template_profile: None,
    };
    let (original, lease) = local.acquire(spec.clone(), &cancel).await.unwrap();
    spec.template_profile = Some(shadowcode_core::local_templates::Profile::Hermes2ProLlama3);
    let refused =
        tokio::time::timeout(Duration::from_secs(1), local.acquire(spec.clone(), &cancel))
            .await
            .expect("same-ID template conflicts must not deadlock");
    assert!(refused
        .err()
        .unwrap()
        .to_string()
        .contains("launch configuration changed"));
    drop(lease);
    let (selected, lease) = local.acquire(spec.clone(), &cancel).await.unwrap();
    assert_ne!(selected.pid, original.pid);
    assert!(!pid_alive(u64::from(original.pid.unwrap())));
    let (warm, second_lease) = local.acquire(spec, &cancel).await.unwrap();
    assert_eq!(warm.pid, selected.pid);
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 2);
    assert_eq!(
        warm.provenance["template_override"]["runtime_template_verified"],
        true
    );
    drop(second_lease);
    drop(lease);
    local.stop().await;
}

#[tokio::test]
async fn hermes_incomplete_calls_and_prose_xml_never_execute_tools() {
    for variant in ["hermes-partial", "hermes-cut-short", "hermes-prose"] {
        let f = fixture(GPU);
        let model = f.models.join(format!("{variant}.gguf"));
        hermes_like(&model, "Hermes-2-Pro-Llama-3-8B", HERMES_DEFAULT_TEMPLATE);
        Config::patch(
            &f.paths,
            json!({"local_engine":{"files":[model]},"agent":{"max_fix_retries":0}}),
        )
        .unwrap();
        let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
        let job = call(&service, "POST", "/api/jobs", json!({"workspace":f.project,"task":"Write a note to unexpected.txt.","model":local_engine::entry_id(&model)})).await.unwrap();
        let done = wait_job(&service, job["id"].as_str().unwrap()).await;
        if variant != "hermes-prose" {
            assert_eq!(done["status"], "failed", "{variant}: {done}");
        }
        assert!(!f.project.join("unexpected.txt").exists());
        let events = service
            .engine
            .store()
            .events_after(done["session_id"].as_str().unwrap(), 0, None, 2000)
            .unwrap();
        assert!(
            !events
                .iter()
                .any(|e| e["task_id"] == done["task_id"] && e["type"] == "tool.started"),
            "{variant}: {events:?}"
        );
        assert!(!lines(&f.bin.join("requests.jsonl"))[0]["tools"]
            .as_array()
            .unwrap()
            .is_empty());
        service.engine.shutdown().await.unwrap();
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
async fn cancel_or_unload_during_provenance_probe_never_launches_a_server() {
    for unload in [false, true] {
        let f = fixture(GPU);
        let path = f.models.join("a.gguf");
        qwen_like(&path, "qwen3", TOOLS_TEMPLATE);
        fs::write(f.bin.join("hold-probe"), b"").unwrap();
        let local = std::sync::Arc::new(shadowcode_core::local_runtime::LocalRuntime::new());
        let request_local = local.clone();
        let cancel = CancellationToken::new();
        let request_cancel = cancel.clone();
        let spec = shadowcode_core::local_runtime::LaunchSpec {
            id: local_engine::entry_id(&path),
            name: "a".into(),
            binary: f.bin.join("llama-server"),
            model: path,
            mmproj: None,
            ctx: 4096,
            gpu: shadowcode_core::local_runtime::GpuMode::All,
            backend: "vulkan".into(),
            template_profile: None,
        };
        let loading =
            tokio::spawn(async move { request_local.acquire(spec, &request_cancel).await });
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.bin.join("probe-started").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        if unload {
            tokio::time::timeout(Duration::from_secs(1), local.unload())
                .await
                .expect("unload releases the slot before the probe returns")
                .unwrap();
        } else {
            cancel.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(1), loading)
            .await
            .expect("Stop does not wait for the blocking probe")
            .unwrap();
        assert!(result.err().unwrap().to_string().contains("cancelled"));
        assert!(local.loaded().is_none());
        fs::write(f.bin.join("release-probe"), b"").unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.bin.join("probe-finished").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        assert!(lines(&f.bin.join("launches.jsonl")).is_empty());
        local.stop().await;
    }
}

#[tokio::test]
async fn replacing_a_model_at_the_same_path_never_reuses_old_weights() {
    let f = fixture(GPU);
    let path = f.models.join("replaced.gguf");
    qwen_like(&path, "qwen3", TOOLS_TEMPLATE);
    Config::patch(&f.paths, json!({"local_engine":{"files":[path]}})).unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    local_engine::scan(&cfg.local_engine);
    let model = model_for(&local_engine::entry_id(&path));
    let engine = Engine::open(f.paths.clone()).unwrap();
    let cancel = CancellationToken::new();
    let first = engine
        .prepare_model_client(&cfg, &model, &cancel)
        .await
        .unwrap();
    let first_pid = engine.local_runtime().loaded().unwrap().pid.unwrap();
    let metadata = fs::metadata(&path).unwrap();
    let replacement = path.with_extension("replacement");
    let mut bytes = fs::read(&path).unwrap();
    *bytes.last_mut().unwrap() = 17;
    fs::write(&replacement, bytes).unwrap();
    fs::File::options()
        .write(true)
        .open(&replacement)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(metadata.modified().unwrap()))
        .unwrap();
    fs::rename(replacement, &path).unwrap();
    // Size and modification time deliberately match the old file.
    assert_eq!(fs::metadata(&path).unwrap().len(), metadata.len());
    assert_eq!(
        fs::metadata(&path).unwrap().modified().unwrap(),
        metadata.modified().unwrap()
    );
    let held_result = engine.prepare_model_client(&cfg, &model, &cancel).await;
    assert!(
        held_result.is_err(),
        "a changed file must not share a live model lease"
    );
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 1);
    drop(first);
    let second = engine
        .prepare_model_client(&cfg, &model, &cancel)
        .await
        .unwrap();
    assert_ne!(
        engine.local_runtime().loaded().unwrap().pid.unwrap(),
        first_pid
    );
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 2);
    assert!(!pid_alive(u64::from(first_pid)));
    drop(second);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn changed_queued_or_loading_model_is_rejected_before_use() {
    let f = fixture(GPU);
    let a = f.models.join("a.gguf");
    let b = f.models.join("b.gguf");
    let slow = f.models.join("holdload.gguf");
    for path in [&a, &b, &slow] {
        qwen_like(path, "qwen3", TOOLS_TEMPLATE);
    }
    Config::patch(&f.paths, json!({"local_engine":{"files":[a,b,slow]}})).unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    local_engine::scan(&cfg.local_engine);
    let engine = Engine::open(f.paths.clone()).unwrap();
    let first = engine
        .prepare_model_client(
            &cfg,
            &model_for(&local_engine::entry_id(&a)),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let waiting = std::sync::Arc::new(tokio::sync::Notify::new());
    let signal = waiting.clone();
    let queued_engine = engine.clone();
    let queued_config = cfg.local_engine.clone();
    let queued_model = model_for(&local_engine::entry_id(&b));
    let queued = tokio::spawn(async move {
        local_engine::prepare_with_progress(
            &queued_config,
            &queued_model,
            queued_engine.local_runtime(),
            &CancellationToken::new(),
            true,
            &|phase| {
                if matches!(phase, shadowcode_core::local_runtime::Progress::Waiting) {
                    signal.notify_one();
                }
                Ok(())
            },
        )
        .await
    });
    tokio::time::timeout(Duration::from_secs(5), waiting.notified())
        .await
        .unwrap();
    let replacement = b.with_extension("new");
    fs::copy(&b, &replacement).unwrap();
    fs::rename(replacement, &b).unwrap();
    drop(first);
    let result = tokio::time::timeout(Duration::from_secs(5), queued)
        .await
        .unwrap()
        .unwrap();
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("changed while preparing"));
    assert_eq!(
        lines(&f.bin.join("launches.jsonl")).len(),
        1,
        "changed queued model never launches"
    );
    let loading_engine = engine.clone();
    let loading_cfg = cfg.clone();
    let loading_model = model_for(&local_engine::entry_id(&slow));
    let loading = tokio::spawn(async move {
        loading_engine
            .prepare_model_client(&loading_cfg, &loading_model, &CancellationToken::new())
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), async {
        while lines(&f.bin.join("launches.jsonl")).len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let replacement = slow.with_extension("new");
    fs::copy(&slow, &replacement).unwrap();
    fs::rename(replacement, &slow).unwrap();
    fs::write(f.bin.join("release-load"), b"ready").unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), loading)
        .await
        .unwrap()
        .unwrap();
    assert!(result
        .err()
        .unwrap()
        .to_string()
        .contains("changed while preparing"));
    assert!(engine.local_runtime().loaded().is_none());
    let launches = lines(&f.bin.join("launches.jsonl"));
    assert!(
        !pid_alive(launches[1]["pid"].as_u64().unwrap()),
        "changed load is terminated and reaped"
    );
    engine.shutdown().await.unwrap();
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

    // A nested or concurrent same-ID request with different launch settings
    // must fail promptly, not wait on a lease held by its own parent.
    for (ctx, gpu, backend) in [
        (
            16384,
            shadowcode_core::local_runtime::GpuMode::Off,
            "vulkan",
        ),
        (8192, shadowcode_core::local_runtime::GpuMode::All, "vulkan"),
        (16384, shadowcode_core::local_runtime::GpuMode::All, "cuda"),
    ] {
        let result = tokio::time::timeout(
            Duration::from_secs(1),
            engine.local_runtime().acquire(
                shadowcode_core::local_runtime::LaunchSpec {
                    id: id_a.clone(),
                    name: "a".into(),
                    binary: f.bin.join("llama-server"),
                    model: a.clone(),
                    mmproj: None,
                    ctx,
                    gpu,
                    backend: backend.into(),
                    template_profile: None,
                },
                &cancel,
            ),
        )
        .await
        .expect("same-ID configuration conflicts do not wait for a lease");
        assert!(result
            .err()
            .unwrap()
            .to_string()
            .contains("launch configuration changed"));
    }
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 1);

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
    let provenance = &loaded["provenance"];
    assert_eq!(provenance["identity_kind"], "filesystem_metadata");
    assert_eq!(
        provenance["files"]["model"]["path"],
        b.display().to_string()
    );
    assert_eq!(provenance["model"]["architecture"], "gemma4");
    assert_eq!(
        provenance["model"]["chat_template"],
        json!(shadowcode_core::gguf::string_identity(TOOLS_TEMPLATE))
    );
    assert_eq!(provenance["runtime"]["reported_version"], "9.9.9-fake");
    assert_eq!(
        provenance["runtime"]["reported_generation_defaults"]["temperature"],
        0.8
    );
    assert_eq!(
        provenance["runtime"]["reported_generation_defaults"]["top_k"],
        40
    );
    assert!(provenance["runtime"]["reported_generation_defaults"]
        .get("prompt")
        .is_none());
    assert_eq!(
        provenance["context"],
        json!({"requested_tokens":16384,"reported_tokens":16384})
    );
    assert!(provenance["files"]["projector"].is_object());
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

/// Settings' Load and Test never wait for a model a running task holds:
/// they answer at once with a clear message, and work once it is free.
#[tokio::test]
async fn settings_load_and_test_refuse_while_a_task_holds_another_model() {
    let f = fixture(GPU);
    let a = f.models.join("a.gguf");
    qwen_like(&a, "qwen3", TOOLS_TEMPLATE);
    let b = f.models.join("b.gguf");
    qwen_like(&b, "qwen3", TOOLS_TEMPLATE);
    Config::patch(
        &f.paths,
        json!({"local_engine":{"files":[a.display().to_string(), b.display().to_string()]}}),
    )
    .unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let entries = local_engine::scan(&cfg.local_engine);
    let id_a = entries.iter().find(|e| e.name == "a").unwrap().id.clone();
    let id_b = entries.iter().find(|e| e.name == "b").unwrap().id.clone();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let held = service
        .engine
        .prepare_model_client(&cfg, &model_for(&id_a), &CancellationToken::new())
        .await
        .unwrap();
    for path in ["/api/local-models/load", "/api/models/test"] {
        let error = tokio::time::timeout(
            Duration::from_secs(10),
            call(&service, "POST", path, json!({"id": id_b})),
        )
        .await
        .unwrap_or_else(|_| panic!("{path} waited for the running task"))
        .unwrap_err()
        .to_string();
        assert!(error.contains("running task is using"), "{path}: {error}");
    }
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 1);
    // The same model is shared, not refused.
    let loaded = call(
        &service,
        "POST",
        "/api/local-models/load",
        json!({"id": id_a}),
    )
    .await
    .unwrap();
    assert_eq!(loaded["loaded"]["id"], id_a.as_str());
    drop(held);
    let loaded = tokio::time::timeout(
        Duration::from_secs(20),
        call(
            &service,
            "POST",
            "/api/local-models/load",
            json!({"id": id_b}),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(loaded["loaded"]["id"], id_b.as_str());
    service.engine.shutdown().await.unwrap();
}

/// A runtime wrapper script that leaves a child holding its output open
/// (for example `llama-server "$@" &`) cannot hang the model catalog: the
/// probe returns and the child is stopped with it.
#[cfg(unix)]
#[test]
fn runtime_probe_does_not_hang_on_a_child_left_behind() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let binary = root.path().join("llama-server");
    let pidfile = root.path().join("child.pid");
    fs::write(
        &binary,
        format!(
            "#!/bin/sh\nsleep 600 &\necho $! > '{}'\necho 'version: 1 (abcdef0)'\n",
            pidfile.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    let probed = binary.clone();
    std::thread::spawn(move || {
        let _ = sender.send(local_engine::probe(&probed));
    });
    let probe = receiver
        .recv_timeout(Duration::from_secs(12))
        .expect("the runtime probe returned");
    assert!(probe.ok, "{:?}", probe.error);
    let pid: u64 = fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let started = Instant::now();
    while pid_alive(pid) {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the wrapper's child survived the probe"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// LOC-04: another program takes llama-server's port between choosing it and
/// the server listening. The load retries on a fresh port on the same device
/// instead of calling it a GPU failure and moving the model to the CPU.
#[tokio::test]
async fn an_occupied_port_is_retried_without_a_cpu_fallback() {
    let f = fixture(GPU);
    let model = f.models.join("porthold.gguf");
    qwen_like(&model, "qwen3", TOOLS_TEMPLATE);
    fs::write(f.bin.join("hold-port-bind"), "hold").unwrap();
    Config::patch(&f.paths, json!({"local_engine":{"files":[model]}})).unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let id = local_engine::scan(&cfg.local_engine)[0].id.clone();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let owner = service.clone();
    let target = id.clone();
    let loading = tokio::spawn(async move {
        call(
            &owner,
            "POST",
            "/api/local-models/load",
            json!({"id": target}),
        )
        .await
    });
    let held_path = f.bin.join("port-bind-held.json");
    let held: Value = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(value) = fs::read(&held_path)
                .ok()
                .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the runtime reached its bind barrier");
    let port = held["port"].as_u64().unwrap() as u16;
    // Another program takes the port; the next launch binds at once.
    let squatter = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    fs::remove_file(f.bin.join("hold-port-bind")).unwrap();
    {
        use std::io::Write;
        let mut barrier =
            std::os::unix::net::UnixStream::connect(held["socket"].as_str().unwrap()).unwrap();
        barrier.write_all(b"bind").unwrap();
    }
    let loaded = tokio::time::timeout(Duration::from_secs(30), loading)
        .await
        .expect("the load finished")
        .unwrap()
        .unwrap();
    assert_eq!(loaded["loaded"]["id"], id.as_str(), "{loaded}");
    let runtime = service.engine.local_runtime().loaded().unwrap();
    assert!(
        !runtime.cpu_fallback,
        "a port conflict is not a GPU failure"
    );
    assert_ne!(runtime.port, port);
    let launches = lines(&f.bin.join("launches.jsonl"));
    assert_eq!(launches.len(), 2);
    for launch in &launches {
        let argv = launch["argv"].to_string();
        assert!(!argv.contains("\"--device\",\"none\""), "{argv}");
    }
    drop(squatter);
    service.engine.shutdown().await.unwrap();
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
    assert_eq!(done["timings"]["model_reused"], false);
    let warm = call(
        &service,
        "POST",
        "/api/jobs",
        json!({
            "workspace": f.project, "task": "What does hello.txt say?", "model": id("coder")
        }),
    )
    .await
    .unwrap();
    let warm = wait_job(&service, warm["id"].as_str().unwrap()).await;
    assert_eq!(warm["status"], "completed", "{warm}");
    assert_eq!(warm["timings"]["model_reused"], true);
    assert!(warm["timings"]["model_load_seconds"].is_null());
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
            template_profile: None,
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

    // A Compare lane never continues on another model, whatever on_limit says.
    Config::patch(&f.paths, json!({"limits":{"on_limit":"local"}})).unwrap();
    let lane = store
        .create_session(&f.project, "cli:codex", "Compare · Codex")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    store
        .set_session_meta(&lane, "compare_id", "fixture-compare")
        .unwrap();
    let job = call(
        &service,
        "POST",
        "/api/jobs",
        json!({"workspace": f.project, "task": "Lane", "model": "cli:codex", "session_id": lane, "handoff_consent": true}),
    )
    .await
    .unwrap();
    let limited = wait_job(&service, job["id"].as_str().unwrap()).await;
    assert_eq!(limited["status"], "limit_reached");
    let mut refused = None;
    for _ in 0..200 {
        let events = store.events_after(&lane, 0, None, 10_000).unwrap();
        if let Some(e) = events.iter().find(|e| e["type"] == "limit.fallback") {
            refused = Some(e["payload"].clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let refused = refused.expect("the lane records why it stopped");
    assert_eq!(refused["ok"], false, "{refused}");
    assert!(refused["reason"].as_str().unwrap().contains("Compare lane"));
    let jobs = call(&service, "GET", "/api/jobs", Value::Null)
        .await
        .unwrap();
    assert_eq!(
        jobs["jobs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["session_id"] == json!(lane))
            .count(),
        1,
        "no follow-up job in the lane"
    );
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

/// A local follow-up queued behind other work in one project does not hold
/// up a local task in another project.
#[tokio::test]
async fn a_queued_local_follow_up_does_not_block_another_project() {
    use shadowcode_core::engine::StartRequest;
    let slow = support::server(|_, _| {
        (
            json!({"choices":[{"message":{"role":"assistant","content":"Done."},"finish_reason":"stop"}],"usage":{"prompt_tokens":5,"completion_tokens":2,"total_tokens":7}}),
            Duration::from_secs(4),
        )
    })
    .await;
    let f = fixture(GPU);
    let a = f.models.join("a.gguf");
    qwen_like(&a, "qwen3", TOOLS_TEMPLATE);
    let projects: Vec<_> = ["x", "y"]
        .iter()
        .map(|name| f._root.path().join(name))
        .collect();
    for project in &projects {
        fs::create_dir(project).unwrap();
    }
    Config::patch(
        &f.paths,
        json!({"local_engine":{"files":[a]},"trusted_workspaces":projects}),
    )
    .unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let id = local_engine::scan(&cfg.local_engine)[0].id.clone();
    let engine = Engine::open(f.paths.clone()).unwrap();
    let request = |project: &PathBuf, model: ModelConfig, queue: bool| StartRequest {
        workspace: project.clone(),
        task: "Say hello".into(),
        session_id: None,
        model: Some(model),
        mode: "code".into(),
        queue,
        images: vec![],
        web: false,
    };
    // Project X: a slow task on another (not managed) model, then a local
    // follow-up queued behind it in the same conversation.
    let other = ModelConfig {
        default: "fixture".into(),
        name: "fixture".into(),
        provider: "local".into(),
        endpoint: slow.endpoint.clone(),
        api_key_env: "SHADOWCODE_TEST_UNUSED_API_KEY".into(),
        keep_alive: "5m".into(),
        context_limit: 16384,
    };
    let first = engine
        .start(request(&projects[0], other, false))
        .await
        .unwrap();
    let mut follow = request(&projects[0], model_for(&id), true);
    follow.session_id = Some(first.session_id.clone());
    let follow = engine.start(follow).await.unwrap();
    // Project Y: a local task starts right away.
    let second = engine
        .start(request(&projects[1], model_for(&id), false))
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while engine
            .store()
            .last_task_event(&second.task_id, "local.runtime_ready")
            .unwrap()
            .is_none()
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the other project's local task started while X was busy");
    assert_eq!(engine.job(&first.id).unwrap().unwrap().status, "running");
    assert_eq!(engine.job(&follow.id).unwrap().unwrap().status, "queued");
    for job in [&first, &follow, &second] {
        let done = tokio::time::timeout(Duration::from_secs(20), engine.wait(&job.id))
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(done.status.as_str(), "completed" | "failed"),
            "{}",
            done.summary
        );
        assert_ne!(done.status, "cancelled");
    }
    assert_eq!(slow.requests.lock().unwrap().len(), 1);
    engine.shutdown().await.unwrap();
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
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let progress = engine
                .store()
                .last_task_event(&first.task_id, "local.runtime_progress")
                .unwrap();
            if progress.is_some_and(|event| event["payload"]["phase"] == "waiting") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("first job reports its held-runtime wait");
    assert!(engine
        .store()
        .last_task_event(&last.task_id, "local.runtime_progress")
        .unwrap()
        .is_none());
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
    let phases: Vec<_> = engine
        .store()
        .events_after(&first.session_id, 0, None, 10_000)
        .unwrap()
        .into_iter()
        .filter(|event| event["task_id"] == first.task_id)
        .filter_map(|event| match event["type"].as_str() {
            Some("local.runtime_progress") => event["payload"]["phase"].as_str().map(str::to_owned),
            Some("local.runtime_ready") => Some("ready".into()),
            _ => None,
        })
        .collect();
    assert_eq!(phases, ["preparing", "waiting", "loading", "ready"]);
    let timing = first.timings.as_ref().unwrap();
    assert!(timing.complete);
    assert!(timing.runtime_wait_seconds.unwrap() > 0.0);
    assert!(timing.model_load_seconds.unwrap() > 0.0);
    assert!(
        timing.preparation_seconds.unwrap()
            >= timing.runtime_wait_seconds.unwrap() + timing.model_load_seconds.unwrap()
    );
    assert!(last.timings.as_ref().unwrap().queue_seconds > timing.runtime_wait_seconds.unwrap());
    let cancelled_timing = cancelled.timings.as_ref().unwrap();
    assert!(cancelled_timing.complete);
    assert!(cancelled_timing.active_seconds.is_none());
    assert!(cancelled_timing.model_load_seconds.is_none());
    assert!(cancelled_timing.model_requests_seconds.is_none());
    assert_eq!(engine.job(&middle.id).unwrap().unwrap().status, "cancelled");
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 3);
    engine.shutdown().await.unwrap();
}

#[tokio::test]
async fn compare_runs_two_installed_gguf_models_sequentially_in_offline_mode() {
    use shadowcode_core::engine::StartRequest;
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
        assert_eq!(
            lane["local_runtime"]["runtime"]["provenance"]["model"]["architecture"],
            "qwen3"
        );
        assert_eq!(
            lane["local_runtime"]["request_policy"]["sampling_source"],
            "runtime_defaults"
        );
        assert_eq!(
            lane["local_runtime"]["request_policy"]["chat_template_kwargs"]["enable_thinking"],
            false
        );
        assert_eq!(lane["base_commit"], result["base"]["commit"]);
        assert_eq!(lane["timings"]["complete"], true);
        assert!(lane["timings"]["model_load_seconds"].as_f64().unwrap() > 0.0);
    }
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 2);
    // A new turn must replace the lane's old runtime receipt while it waits.
    let held = service
        .engine
        .prepare_model_client(&cfg, &model_for(&ids[1]), &CancellationToken::new())
        .await
        .unwrap();
    let lane = &result["lanes"][0];
    let followup = service
        .engine
        .start(StartRequest {
            workspace: PathBuf::from(lane["worktree"].as_str().unwrap()),
            session_id: Some(lane["session_id"].as_str().unwrap().into()),
            task: "Read hello.txt again".into(),
            model: Some(model_for(&ids[0])),
            mode: "code".into(),
            queue: false,
            images: vec![],
            web: false,
        })
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let progress = service
                .engine
                .store()
                .last_task_event(&followup.task_id, "local.runtime_progress")
                .unwrap();
            if progress.is_some_and(|event| event["payload"]["phase"] == "waiting") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("follow-up waits for held model");
    let waiting = call(
        &service,
        "GET",
        &format!("/api/compare/{record_id}"),
        Value::Null,
    )
    .await
    .unwrap();
    assert_eq!(waiting["lanes"][0]["job_id"], followup.id);
    assert!(waiting["lanes"][0]["local_runtime"].is_null());
    assert_eq!(waiting["lanes"][0]["local_progress"]["phase"], "waiting");
    assert_eq!(waiting["lanes"][0]["timings"]["complete"], false);
    assert!(waiting["lanes"][0]["timings"]["model_load_seconds"].is_null());
    service.engine.cancel(&followup.id).await.unwrap();
    let stopped = tokio::time::timeout(Duration::from_secs(5), service.engine.wait(&followup.id))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stopped.status, "cancelled");
    drop(held);
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
                .unwrap()
                .expect("each completed local job must record its own runtime"),
        );
    }
    for ((job, runtime), file) in jobs.iter().zip(&runtimes).zip(&files) {
        assert_eq!(job.result.as_ref().unwrap()["timings"]["model_requests"], 1);
        assert_eq!(runtime["payload"]["automatic_cpu_fallback_allowed"], false);
        assert_eq!(runtime["payload"]["runtime"]["cpu_fallback"], false);
        assert_eq!(
            runtime["payload"]["runtime"]["provenance"]["files"]["model"]["path"],
            *file
        );
    }
    assert_eq!(
        fs::read_to_string(project.join("calc.py")).unwrap(),
        "def add(a, b):\n    return a + b\n"
    );
    let answer_valid: Vec<_> = jobs
        .iter()
        .map(|job| addition_answer_valid(&job.summary))
        .collect();
    let report = json!({"scope":if comparison.is_some(){"real multi-model Compare inference"}else{"real single-model inference only"},"runtime":catalog["runtime"],"hardware":catalog["hardware"],"models":models,"jobs":jobs,"runtime_events":runtimes,"answer_valid":answer_valid,"answer_check":"An explanation contains add/adds/addition/sum/sums and no chat control token; this is a simple output sanity check, not a coding-quality evaluation."});
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
    for (job, valid) in jobs.iter().zip(answer_valid) {
        assert_eq!(job.status, "completed", "{}", job.summary);
        assert!(
            valid,
            "model {} failed the addition-answer sanity check: {:?}",
            job.model, job.summary
        );
    }
    for pair in jobs.windows(2) {
        assert!(pair[1].started_at >= pair[0].finished_at.unwrap());
    }
}

/// Real three-model cancellation acceptance. The first installed model must
/// start streaming before cancellation; the other two must never load.
/// Requires a loopback-only network namespace for offline evidence.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires three explicit installed GGUF paths and a real llama-server"]
async fn live_three_model_compare_cancels_queued_models() {
    let files: Vec<String> = serde_json::from_str(
        &std::env::var("SHADOWCODE_LIVE_GGUF_FILES")
            .expect("SHADOWCODE_LIVE_GGUF_FILES JSON array"),
    )
    .unwrap();
    assert_eq!(files.len(), 3, "three installed models are required");
    let server = std::env::var("SHADOWCODE_LLAMA_SERVER").expect("SHADOWCODE_LLAMA_SERVER");
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    fs::create_dir(&project).unwrap();
    fs::write(project.join("README.md"), "Offline cancellation fixture\n").unwrap();
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
        "trusted_workspaces":[project],"agent":{"max_steps":4,"max_task_tokens":8192}
    })).unwrap();
    let service = Service::open(paths, Some(project.clone())).unwrap();
    let catalog = call(&service, "GET", "/api/local-models", Value::Null)
        .await
        .unwrap();
    let models = catalog["models"].as_array().unwrap();
    assert_eq!(models.len(), 3, "{catalog}");
    let ids: Vec<_> = files
        .iter()
        .map(|file| {
            models.iter().find(|model| model["path"] == *file).unwrap()["id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let record = call(
        &service,
        "POST",
        "/api/compare",
        json!({"workspace":project,"task":"List the integers from 1 through 2000, separated by commas. Do not abbreviate or call tools.","models":ids,"mode":"ask","web":false}),
    )
    .await
    .unwrap();
    let comparison = record["id"].as_str().unwrap().to_owned();
    let lanes = record["lanes"].as_array().unwrap();
    let job_ids: Vec<_> = lanes
        .iter()
        .map(|lane| lane["job_id"].as_str().unwrap().to_owned())
        .collect();
    let task_ids: Vec<_> = job_ids
        .iter()
        .map(|id| {
            service.engine.store().job(id).unwrap().unwrap()["task_id"]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if service
                .engine
                .store()
                .last_task_event(&task_ids[0], "model.stream")
                .unwrap()
                .is_some()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("first model did not stream before the acceptance deadline");
    assert!(service
        .engine
        .store()
        .last_task_event(&task_ids[0], "local.runtime_ready")
        .unwrap()
        .is_some());
    assert!(service
        .engine
        .store()
        .last_task_event(&task_ids[1], "local.runtime_ready")
        .unwrap()
        .is_none());
    assert!(service
        .engine
        .store()
        .last_task_event(&task_ids[2], "local.runtime_ready")
        .unwrap()
        .is_none());
    call(
        &service,
        "POST",
        &format!("/api/compare/{comparison}/cancel"),
        Value::Null,
    )
    .await
    .unwrap();
    let mut jobs = Vec::new();
    for id in &job_ids {
        jobs.push(
            tokio::time::timeout(Duration::from_secs(30), service.engine.wait(id))
                .await
                .expect("cancelled job did not reach a terminal state")
                .unwrap(),
        );
    }
    let queued_loading: Vec<_> = jobs[1..]
        .iter()
        .map(|job| {
            service
                .engine
                .store()
                .events_after(&job.session_id, 0, None, 10_000)
                .unwrap()
                .into_iter()
                .any(|event| {
                    event["task_id"] == job.task_id
                        && event["type"] == "local.runtime_progress"
                        && event["payload"]["phase"] == "loading"
                })
        })
        .collect();
    let report = json!({
        "scope":"real three-model Compare cancellation during first-model stream",
        "models":models,
        "jobs":jobs,
        "first_stream_observed":true,
        "queued_loading":queued_loading,
        "queued_runtime_ready":[
            service.engine.store().last_task_event(&task_ids[1], "local.runtime_ready").unwrap().is_some(),
            service.engine.store().last_task_event(&task_ids[2], "local.runtime_ready").unwrap().is_some()
        ]
    });
    if let Ok(output) = std::env::var("SHADOWCODE_LIVE_REPORT") {
        fs::write(output, serde_json::to_vec_pretty(&report).unwrap()).unwrap();
    }
    println!(
        "LIVE_CANCEL_ACCEPTANCE {}",
        serde_json::to_string(&report).unwrap()
    );
    assert!(jobs.iter().all(|job| job.status == "cancelled"), "{jobs:?}");
    assert_eq!(
        queued_loading,
        vec![false, false],
        "queued models began loading"
    );
    for task_id in &task_ids[1..] {
        assert!(service
            .engine
            .store()
            .last_task_event(task_id, "local.runtime_ready")
            .unwrap()
            .is_none());
    }
    call(
        &service,
        "POST",
        &format!("/api/compare/{comparison}/discard"),
        Value::Null,
    )
    .await
    .unwrap();
    service.engine.shutdown().await.unwrap();
}

fn addition_answer_valid(answer: &str) -> bool {
    !answer.contains("<|")
        && !answer.contains("|>")
        && answer.split(|c: char| !c.is_alphabetic()).any(|word| {
            matches!(
                word.to_ascii_lowercase().as_str(),
                "add" | "adds" | "addition" | "sum" | "sums"
            )
        })
}

#[test]
fn live_acceptance_rejects_control_tokens_and_irrelevant_nonempty_answers() {
    assert!(!addition_answer_valid("<|im_end|>"));
    assert!(!addition_answer_valid("<|assistant|>It adds two values."));
    assert!(!addition_answer_valid("Ready."));
    assert!(addition_answer_valid("It returns the sum of a and b."));
    assert!(addition_answer_valid(
        "The function adds its two arguments."
    ));
}

#[tokio::test]
async fn runtime_tool_report_is_task_scoped_and_bound_to_reused_launch() {
    let f = fixture(GPU);
    let path = f.models.join("tool-hint.gguf");
    qwen_like(&path, "qwen3", "{# tools are unavailable #}{{ messages }}");
    Config::patch(&f.paths, json!({"local_engine":{"files":[path]}})).unwrap();
    fs::write(f.project.join("hello.txt"), "hello\n").unwrap();
    let named = format!("{}named tool template tail", "é".repeat(70_000));
    fs::write(f.bin.join("props-overrides.json"), json!({
        "chat_template_tool_use": named,
        "chat_template_caps": {"supports_tools": false, "supports_tool_calls": false, "private": "fake-secret"}
    }).to_string()).unwrap();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let mut runs = Vec::new();
    for step in 0..3 {
        if step == 1 {
            // The existing live lease keeps the report observed at its launch.
            fs::write(
                f.bin.join("props-overrides.json"),
                json!({
                    "chat_template_tool_use": "replacement named template",
                    "chat_template_caps": {"supports_tools": true, "supports_tool_calls": true}
                })
                .to_string(),
            )
            .unwrap();
        }
        if step == 2 {
            // Same model path, changed owned file identity: requires a fresh launch.
            let replacement = path.with_extension("replacement");
            let mut bytes = fs::read(&path).unwrap();
            *bytes.last_mut().unwrap() = 17;
            fs::write(&replacement, bytes).unwrap();
            fs::rename(replacement, &path).unwrap();
        }
        let session = call(
            &service,
            "POST",
            "/api/sessions",
            json!({"workspace": f.project}),
        )
        .await
        .unwrap();
        let job = call(&service, "POST", "/api/jobs", json!({
            "workspace": f.project, "session_id": session["id"], "task": "What does hello.txt say?", "model": local_engine::entry_id(&path)
        })).await.unwrap();
        let done = wait_job(&service, job["id"].as_str().unwrap()).await;
        assert_eq!(done["status"], "completed", "{done}");
        assert_eq!(done["timings"]["model_reused"], step == 1);
        tokio::time::timeout(Duration::from_secs(2), async {
            while service.engine.local_runtime().in_use() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("completed task must release its runtime lease");
        runs.push(done);
    }
    service.engine.shutdown().await.unwrap();
    assert_eq!(lines(&f.bin.join("launches.jsonl")).len(), 2);
    let requests = lines(&f.bin.join("requests.jsonl"));
    assert_eq!(requests.len(), 6);
    assert!(
        requests.iter().all(|r| r["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t == "read_file")),
        "runtime reports are observations, not a changed schema gate"
    );
    for (step, done) in runs.iter().enumerate() {
        let events = service
            .engine
            .store()
            .events_after(done["session_id"].as_str().unwrap(), 0, None, 2000)
            .unwrap();
        let receipts: Vec<_> = events
            .iter()
            .filter(|e| e["task_id"] == done["task_id"] && e["type"] == "local.runtime_ready")
            .collect();
        assert_eq!(receipts.len(), 1);
        let runtime = &receipts[0]["payload"]["runtime"]["provenance"]["runtime"];
        assert_eq!(
            runtime["reported_tool_capabilities"]["field_status"],
            "reported"
        );
        assert_eq!(
            runtime["reported_tool_capabilities"]["supports_tools"],
            step == 2
        );
        assert_eq!(
            runtime["reported_tool_capabilities"]["supports_tool_calls"],
            step == 2
        );
        assert!(runtime["reported_tool_capabilities"]["supports_object_arguments"].is_null());
        assert_eq!(
            runtime["reported_chat_template"],
            json!(shadowcode_core::gguf::string_identity(
                "runtime template fixture"
            ))
        );
        assert_eq!(
            runtime["reported_chat_template_tool_use"],
            json!(shadowcode_core::gguf::string_identity(if step == 2 {
                "replacement named template"
            } else {
                &named
            }))
        );
        assert!(!runtime.to_string().contains("named tool template tail"));
        assert!(!runtime.to_string().contains("fake-secret"));
    }
}

#[tokio::test]
async fn runtime_tool_report_missing_and_malformed_preserve_schema_compatibility() {
    for (overrides, status, invalid_fields) in [
        (json!({}), "missing", json!([])),
        (
            json!({"chat_template_caps": null, "chat_template_tool_use": true}),
            "invalid",
            json!([]),
        ),
        (json!({"chat_template_caps": "true"}), "invalid", json!([])),
        (
            json!({"chat_template_caps": {"supports_tools": "true", "supports_tool_calls": 1}}),
            "reported",
            json!(["supports_tools", "supports_tool_calls"]),
        ),
    ] {
        let f = fixture(GPU);
        let path = f.models.join("tool-hint.gguf");
        qwen_like(&path, "qwen3", TOOLS_TEMPLATE);
        fs::write(f.bin.join("props-overrides.json"), overrides.to_string()).unwrap();
        Config::patch(&f.paths, json!({"local_engine":{"files":[path]}})).unwrap();
        let cfg = Config::load(&f.paths, None).unwrap();
        local_engine::scan(&cfg.local_engine);
        let model = model_for(&local_engine::entry_id(&path));
        let engine = Engine::open(f.paths.clone()).unwrap();
        let prepared = engine
            .prepare_model_client(&cfg, &model, &CancellationToken::new())
            .await
            .unwrap();
        assert!(
            prepared.tools,
            "unknown report must not disable legacy schemas"
        );
        let mut schemas = vec![json!({"type":"function","function":{"name":"read_file"}})];
        prepared.filter_schemas(&mut schemas);
        assert_eq!(schemas.len(), 1);
        let loaded = engine.local_runtime().loaded().unwrap();
        drop(prepared);
        engine.shutdown().await.unwrap();
        let runtime = &loaded.provenance["runtime"];
        assert_eq!(
            runtime["reported_tool_capabilities"]["field_status"],
            status
        );
        assert!(runtime["reported_tool_capabilities"]["supports_tools"].is_null());
        assert!(runtime["reported_tool_capabilities"]["supports_tool_calls"].is_null());
        assert_eq!(
            runtime["reported_tool_capabilities"]["invalid_fields"],
            invalid_fields
        );
        assert!(runtime["reported_chat_template_tool_use"].is_null());
    }
}

// Linux fixture: the real fake-runtime child exits only after the test sends
// an explicit command over its private socket, after managed readiness.
#[cfg(target_os = "linux")]
async fn post_ready_crash_observation(first_observer: &str) {
    use anyhow::Context;
    use tokio::io::AsyncWriteExt;

    // Observe exact start ticks, allowing a zombie to establish that the owned
    // child has exited before the first catalog/loaded API observation.
    fn process(pid: u64) -> Option<(u64, char)> {
        let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = text.rsplit_once(") ")?.1.split_whitespace().collect();
        Some((
            fields.get(19)?.parse().ok()?,
            fields.first()?.chars().next()?,
        ))
    }
    async fn exited(pid: u64, start: u64) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if process(pid).is_none_or(|(now, state)| now != start || state == 'Z') {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("owned fake runtime did not exit after its explicit command")
    }

    let f = fixture(GPU);
    let model = f.models.join("postready.gguf");
    qwen_like(&model, "qwen3", TOOLS_TEMPLATE);
    fs::write(f.project.join("hello.txt"), "original project bytes\n").unwrap();
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
    let original_index = fs::read(f.project.join(".git/index")).unwrap();
    Config::patch(
        &f.paths,
        json!({
            "local_engine":{"files":[model]},
            "network":{"mode":"offline"},
            "cli_agents":{"enabled":false}
        }),
    )
    .unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let id = local_engine::scan(&cfg.local_engine)[0].id.clone();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    // Same executable, outside LocalRuntime ownership. It must survive both
    // observation and runtime shutdown; the fixture itself reaps it afterward.
    let mut sentinel = tokio::process::Command::new(f.bin.join("llama-server"))
        .arg("--post-ready-sentinel")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let observed: anyhow::Result<Value> = async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.bin.join("post-ready-sentinel-ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("sentinel did not reach its explicit ready barrier")?;
        let before = call(&service, "POST", "/api/local-models/load", json!({"id":id})).await?;
        anyhow::ensure!(before["loaded"]["id"] == id, "managed runtime was not ready: {before}");
        let warm_picker = call(&service, "GET", "/api/picker?cached=1", Value::Null).await?;
        let warm_reason = warm_picker["targets"].as_array().context("missing cached picker targets")?.iter().find(|row| row["id"] == id).context("missing cached local row")?["reason"].clone();
        anyhow::ensure!(warm_reason.as_str().is_some_and(|s| s.starts_with("Loaded")), "cached picker was not primed with loaded state: {warm_picker}");
        let pid = service.engine.local_runtime().loaded().context("missing ready runtime")?.pid.context("missing owned PID")? as u64;
        let (start, state) = process(pid).context("ready child is absent")?;
        anyhow::ensure!(state != 'Z', "ready child already exited");
        let mut exit = tokio::time::timeout(Duration::from_secs(2), tokio::net::UnixStream::connect(f.bin.join(format!("post-ready-exit-{pid}.sock"))))
            .await.context("exit barrier connect timed out")??;
        exit.write_all(b"exit").await?;
        drop(exit);
        exited(pid, start).await?;

        // Each public observation gets an independent first-observer fixture;
        // neither is allowed to rely on another acquire discovering the crash.
        let direct_first = (first_observer == "loaded").then(|| service.engine.local_runtime().loaded().is_none());
        let picker_first = if first_observer == "picker" {
            call(&service, "GET", "/api/picker?cached=1", Value::Null).await?
        } else { Value::Null };
        let first = call(&service, "GET", "/api/local-models", Value::Null).await?;
        let first_row = first["models"].as_array().context("missing rows")?.iter().find(|row| row["id"] == id).context("model disappeared")?.clone();
        let again = call(&service, "GET", "/api/local-models", Value::Null).await?;
        let picker = call(&service, "GET", "/api/picker?cached=1", Value::Null).await?;
        let picker_row = picker["targets"].as_array().context("missing cached picker targets")?.iter().find(|row| row["id"] == id).context("picker row disappeared")?.clone();
        let before_restart_launches = lines(&f.bin.join("launches.jsonl")).len();
        // Final stderr draining is asynchronous. Poll its actual evidence, not
        // a fixed sleep, and preserve the first catalog response independently.
        let tail_visible = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if service.engine.local_runtime().last_error(&id).is_some_and(|error| error.contains("post-ready fixture exit")) { break; }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.is_ok();
        let restarted = call(&service, "POST", "/api/local-models/load", json!({"id":id})).await?;
        let new_pid = service.engine.local_runtime().loaded().context("restart missing")?.pid.context("restart PID missing")? as u64;
        let new_start = process(new_pid).context("restarted child missing")?.0;
        let restart_error = service.engine.local_runtime().last_error(&id);
        service.engine.shutdown().await?;
        exited(new_pid, new_start).await?;
        Ok(json!({
            "first_observer":first_observer,"direct_first":direct_first,"picker_first":picker_first,"warm_reason":warm_reason,
            "first_loaded":first["loaded"],"first_error":first_row["last_error"],
            "second_loaded":again["loaded"],"picker_reason":picker_row["reason"],
            "tail_visible":tail_visible,"before_restart_launches":before_restart_launches,
            "launches":lines(&f.bin.join("launches.jsonl")).len(),
            "restarted":restarted,"restart_error":restart_error,
            "old_pid":pid,"new_pid":new_pid,"sentinel_alive":sentinel.try_wait()?.is_none(),
            "index_unchanged":fs::read(f.project.join(".git/index"))? == original_index,
            "project_unchanged":fs::read_to_string(f.project.join("hello.txt"))? == "original project bytes\n"
        }))
    }.await;

    // Finish all owned cleanup before any expected baseline assertion fails.
    let shutdown = service.engine.shutdown().await;
    let sentinel_survived_shutdown = sentinel.try_wait().unwrap().is_none();
    let _ = sentinel.start_kill();
    let sentinel_reaped = tokio::time::timeout(Duration::from_secs(5), sentinel.wait()).await;
    assert!(shutdown.is_ok(), "{shutdown:?}");
    assert!(
        matches!(sentinel_reaped, Ok(Ok(_))),
        "sentinel fixture cleanup failed: {sentinel_reaped:?}"
    );
    let observed = observed.expect("post-ready fixture setup/cleanup failed");
    eprintln!("POST_READY_CRASH {observed}");
    assert!(
        sentinel_survived_shutdown && observed["sentinel_alive"] == true,
        "unrelated same-executable child was disturbed: {observed}"
    );
    assert_eq!(
        observed["before_restart_launches"], 1,
        "observation must not restart the runtime: {observed}"
    );
    assert_eq!(
        observed["launches"], 2,
        "explicit retry launches exactly once: {observed}"
    );
    assert_ne!(observed["old_pid"], observed["new_pid"]);
    assert_eq!(observed["restarted"]["loaded"]["id"], id);
    assert!(
        observed["restart_error"].is_null(),
        "successful explicit retry clears the old failure: {observed}"
    );
    assert_eq!(observed["index_unchanged"], true);
    assert_eq!(observed["project_unchanged"], true);
    if first_observer == "loaded" {
        assert_eq!(
            observed["direct_first"], true,
            "direct loaded() retained a dead child: {observed}"
        );
    }
    if first_observer == "picker" {
        assert!(
            observed["picker_first"]["local_engine"]["loaded"].is_null(),
            "cached picker retained a dead runtime as first observer: {observed}"
        );
        let row = observed["picker_first"]["targets"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["id"] == id)
            .unwrap();
        assert!(
            row["reason"]
                .as_str()
                .is_some_and(|s| !s.starts_with("Loaded") && s.contains("stopped unexpectedly")),
            "first cached picker lost crash evidence: {observed}"
        );
    }
    assert!(
        observed["first_loaded"].is_null(),
        "first catalog retained a dead child: {observed}"
    );
    assert!(
        observed["second_loaded"].is_null(),
        "repeat catalog retained a dead child: {observed}"
    );
    assert!(
        observed["first_error"]
            .as_str()
            .is_some_and(|s| s.contains("stopped unexpectedly")),
        "first catalog lost crash evidence: {observed}"
    );
    assert!(
        observed["picker_reason"]
            .as_str()
            .is_some_and(|s| !s.starts_with("Loaded") && s.contains("stopped unexpectedly")),
        "picker did not retain crash evidence: {observed}"
    );
    assert_eq!(
        observed["tail_visible"], true,
        "bounded stderr crash tail lost: {observed}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn post_ready_crash_clears_first_catalog_and_allows_explicit_restart() {
    post_ready_crash_observation("catalog").await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn post_ready_crash_clears_direct_loaded_observation() {
    post_ready_crash_observation("loaded").await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn post_ready_crash_refreshes_primed_cached_picker_without_acquire() {
    post_ready_crash_observation("picker").await;
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn foreign_health_with_rejected_props_cannot_mark_an_unbound_runtime_loaded() {
    use anyhow::Context;
    fn process(pid: u64) -> Option<(u64, char)> {
        let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = text.rsplit_once(") ")?.1.split_whitespace().collect();
        Some((
            fields.get(19)?.parse().ok()?,
            fields.first()?.chars().next()?,
        ))
    }
    async fn barrier(path: &Path) -> anyhow::Result<Value> {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(bytes) = fs::read(path) {
                    if let Ok(value) = serde_json::from_slice(&bytes) {
                        return value;
                    }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("fake runtime did not reach its explicit socket/listener barrier")
    }
    async fn gone(pid: u64, start: u64) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while process(pid).is_some_and(|(now, state)| now == start && state != 'Z') {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("owned fake runtime remained after cleanup")
    }

    let f = fixture(""); // Fake CPU-only mode: no GPU retry can obscure the case.
    let model = f.models.join("porthold.gguf");
    qwen_like(&model, "qwen3", TOOLS_TEMPLATE);
    let model_before = fs::read(&model).unwrap();
    fs::write(f.bin.join("hold-port-bind"), "hold").unwrap();
    fs::write(f.project.join("hello.txt"), "original port fixture\n").unwrap();
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
    let index = fs::read(f.project.join(".git/index")).unwrap();
    Config::patch(&f.paths, json!({"local_engine":{"files":[model]},"network":{"mode":"offline"},"cli_agents":{"enabled":false}})).unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let id = local_engine::scan(&cfg.local_engine)[0].id.clone();
    let service = Service::open(f.paths.clone(), Some(f.project.clone())).unwrap();
    let mut loading = None;
    let mut foreign: Option<tokio::process::Child> = None;
    let observed: anyhow::Result<Value> = async {
        let owner = service.clone(); let target = id.clone();
        loading = Some(tokio::spawn(async move { call(&owner, "POST", "/api/local-models/load", json!({"id":target})).await }));
        let held = barrier(&f.bin.join("port-bind-held.json")).await?;
        let pid = held["pid"].as_u64().context("barrier PID missing")?;
        let port = held["port"].as_u64().context("barrier port missing")?;
        let (start, state) = process(pid).context("held runtime child absent")?;
        anyhow::ensure!(state != 'Z', "owned child exited before collision setup");
        anyhow::ensure!(Path::new(held["socket"].as_str().context("barrier socket missing")?).exists(), "private bind barrier is not established");
        // This independent listener is fixture-owned, not a LocalRuntime child.
        foreign = Some(tokio::process::Command::new(f.bin.join("llama-server"))
            .args(["--foreign-health-props", &port.to_string()])
            .env_clear().env("PATH", "/usr/bin:/bin")
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .kill_on_drop(true).spawn()?);
        let listener = barrier(&f.bin.join("foreign-listener-ready.json")).await?;
        anyhow::ensure!(listener["port"] == port && listener["pid"].as_u64() == foreign.as_ref().unwrap().id().map(u64::from), "wrong listener barrier: {listener}");
        // The barrier marker is emitted only by the actual /props handler.
        barrier(&f.bin.join("foreign-props-observed.json")).await?;
        let attempt = match tokio::time::timeout(Duration::from_secs(5), loading.as_mut().unwrap()).await {
            Ok(joined) => {
                loading = None;
                match joined.context("public load worker panicked")? {
                    Ok(value) => json!({"kind":"accepted","response":value}),
                    Err(error) => json!({"kind":"refused","error":error.to_string()}),
                }
            }
            Err(_) => json!({"kind":"pending"}),
        };
        let owned_alive_after_props = process(pid).is_some_and(|(now, state)| now == start && state != 'Z');
        let owned_present_after_props = process(pid).is_some_and(|(now, _)| now == start);
        let catalog = call(&service, "GET", "/api/local-models", Value::Null).await?;
        let picker = call(&service, "GET", "/api/picker?cached=1", Value::Null).await?;
        let launches_before_retry = lines(&f.bin.join("launches.jsonl")).len();
        let _ = tokio::time::timeout(Duration::from_secs(10), call(&service, "POST", "/api/local-models/unload", json!({}))).await.context("managed attempt unload deadline")??;
        if let Some(task) = loading.as_mut() {
            let _outcome = tokio::time::timeout(Duration::from_secs(5), &mut *task).await.context("load worker did not join after cancellation")?.context("load worker panicked during cleanup")?;
            loading = None;
        }
        gone(pid, start).await?;
        anyhow::ensure!(foreign.as_mut().unwrap().try_wait()?.is_none(), "managed cleanup stopped unrelated listener");
        let client = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(2)).build()?;
        let sentinel = client.get(format!("http://127.0.0.1:{port}/fixture/sentinel")).send().await?.json::<Value>().await?;
        anyhow::ensure!(sentinel == json!({"foreign_listener":"still owned by fixture"}), "foreign listener was replaced or stopped: {sentinel}");
        // The fixture itself releases its listener only after survival proof.
        let child = foreign.as_mut().unwrap(); child.start_kill()?;
        tokio::time::timeout(Duration::from_secs(3), child.wait()).await.context("foreign listener cleanup deadline")??;
        fs::remove_file(f.bin.join("hold-port-bind"))?;
        let retry = tokio::time::timeout(Duration::from_secs(10), call(&service, "POST", "/api/local-models/load", json!({"id":id}))).await.context("explicit retry deadline")??;
        let loaded = service.engine.local_runtime().loaded().context("explicit retry not loaded")?;
        let new_pid = loaded.pid.context("explicit retry PID missing")? as u64;
        let (new_start, _) = process(new_pid).context("retry child missing")?;
        service.engine.shutdown().await?;
        gone(new_pid, new_start).await?;
        Ok(json!({"attempt":attempt,"owned_alive_after_props":owned_alive_after_props,"owned_present_after_props":owned_present_after_props,"catalog_loaded":catalog["loaded"],"picker":picker,
            "launches_before_retry":launches_before_retry,"launches_after_retry":lines(&f.bin.join("launches.jsonl")).len(),
            "foreign_survived_managed_cleanup":true,"retry":retry,"first_pid":pid,"retry_pid":new_pid,
            "no_inference_requests":lines(&f.bin.join("requests.jsonl")).is_empty(),
            "index_unchanged":fs::read(f.project.join(".git/index"))? == index,
            "model_unchanged":fs::read(&model)? == model_before,
            "project_unchanged":fs::read_to_string(f.project.join("hello.txt"))? == "original port fixture\n"}))
    }.await;

    // Cleanup precedes the expected before-fix false-readiness assertion.
    let shutdown = tokio::time::timeout(Duration::from_secs(12), service.engine.shutdown()).await;
    let load_joined = if let Some(task) = loading.as_mut() {
        match tokio::time::timeout(Duration::from_secs(5), &mut *task).await {
            Ok(Ok(_)) => true,
            Ok(Err(_)) => false,
            Err(_) => {
                task.abort();
                let _ = task.await;
                false
            }
        }
    } else {
        true
    };
    let foreign_reaped = if let Some(child) = foreign.as_mut() {
        let _ = child.start_kill();
        matches!(
            tokio::time::timeout(Duration::from_secs(3), child.wait()).await,
            Ok(Ok(_))
        )
    } else {
        true
    };
    assert!(
        matches!(shutdown, Ok(Ok(()))),
        "managed shutdown failed: {shutdown:?}"
    );
    assert!(
        load_joined && foreign_reaped,
        "owned fixture cleanup did not complete"
    );
    let observed = observed.expect("port collision fixture setup/cleanup failed");
    eprintln!("PORT_FALSE_READINESS {observed}");
    assert_eq!(observed["foreign_survived_managed_cleanup"], true);
    assert_eq!(
        observed["launches_before_retry"], 1,
        "no automatic runtime retries: {observed}"
    );
    assert_eq!(observed["launches_after_retry"], 2);
    assert_eq!(observed["retry"]["loaded"]["id"], id);
    assert_eq!(observed["no_inference_requests"], true);
    assert_eq!(observed["index_unchanged"], true);
    assert_eq!(observed["model_unchanged"], true);
    assert_eq!(observed["project_unchanged"], true);
    assert_eq!(
        observed["attempt"]["kind"], "refused",
        "foreign health200/props401 must not establish readiness: {observed}"
    );
    assert_eq!(
        observed["owned_alive_after_props"], false,
        "refusal must stop its owned child before returning: {observed}"
    );
    assert_eq!(
        observed["owned_present_after_props"], false,
        "refusal must reap its owned child before returning: {observed}"
    );
    assert!(
        observed["catalog_loaded"].is_null(),
        "foreign endpoint published as loaded: {observed}"
    );
    let row = observed["picker"]["targets"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .unwrap();
    assert!(
        !row["reason"].as_str().unwrap().starts_with("Loaded"),
        "picker claimed foreign runtime ready: {observed}"
    );
}

/// LOC-04 active generation: a confirmed managed-child exit must fail the
/// admitted task, preserve completed work, and permit a separate explicit retry.
#[cfg(target_os = "linux")]
#[tokio::test]
async fn active_runtime_exit_preserves_edit_and_failed_task_then_allows_explicit_retry() {
    use anyhow::{ensure, Context};
    use futures_util::FutureExt;
    use tokio::io::AsyncWriteExt;

    fn process(pid: u64) -> Option<(u64, char)> {
        let text = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let fields: Vec<_> = text.rsplit_once(") ")?.1.split_whitespace().collect();
        Some((
            fields.get(19)?.parse().ok()?,
            fields.first()?.chars().next()?,
        ))
    }
    async fn exited(pid: u64, start: u64) -> anyhow::Result<()> {
        tokio::time::timeout(Duration::from_secs(5), async {
            while process(pid).is_some_and(|(now, state)| now == start && state != 'Z') {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .context("owned runtime did not exit after explicit command")
    }
    fn git(project: &Path, args: &[&str]) -> anyhow::Result<String> {
        let out = std::process::Command::new("git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(project)
            .output()?;
        ensure!(
            out.status.success(),
            "fixture git failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Ok(String::from_utf8(out.stdout)?.trim().to_owned())
    }

    let f = fixture(""); // CPU fixture; no GPU-fallback path or real model.
    let model = f.models.join("activeexit.gguf");
    qwen_like(&model, "qwen3", TOOLS_TEMPLATE);
    let model_bytes = fs::read(&model).unwrap();
    fs::write(f.bin.join("active-exit-armed"), "").unwrap();
    fs::write(f.project.join("hello.txt"), "original project bytes\n").unwrap();
    fs::write(f.project.join("check_retained.py"), "from pathlib import Path\nassert Path('retained.txt').read_text() == 'retained before runtime exit\\n'\nassert not Path('unexecuted.txt').exists()\nprint('retained-check-passed')\n").unwrap();
    git(&f.project, &["init", "-q"]).unwrap();
    git(&f.project, &["add", "."]).unwrap();
    git(&f.project, &["commit", "-qm", "base"]).unwrap();
    fs::write(f.project.join("hello.txt"), "staged user bytes\n").unwrap();
    git(&f.project, &["add", "hello.txt"]).unwrap();
    fs::write(f.project.join("hello.txt"), "unstaged user bytes\n").unwrap();
    fs::write(f.project.join("unrelated.txt"), "unrelated user bytes\n").unwrap();
    let original_index = fs::read(f.project.join(".git/index")).unwrap();
    let original_head = git(&f.project, &["rev-parse", "HEAD"]).unwrap();
    let check_bytes = fs::read(f.project.join("check_retained.py")).unwrap();
    Config::patch(
        &f.paths,
        json!({
            "local_engine":{"files":[model]},
            "network":{"mode":"offline"},"cli_agents":{"enabled":false},
            "permissions":{"mode":"allow_edits","approve_shell":false},
            "verification":{"commands":["python3 check_retained.py"]}
        }),
    )
    .unwrap();
    let cfg = Config::load(&f.paths, None).unwrap();
    let id = local_engine::scan(&cfg.local_engine)[0].id.clone();
    let mut service = Some(Service::open(f.paths.clone(), Some(f.project.clone())).unwrap());
    let mut sentinel = tokio::process::Command::new(f.bin.join("llama-server"))
        .arg("--post-ready-sentinel")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();

    let observed = std::panic::AssertUnwindSafe(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !f.bin.join("post-ready-sentinel-ready").is_file() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("sentinel ready barrier")?;
        let started = call(service.as_ref().unwrap(), "POST", "/api/jobs", json!({
            "workspace":f.project,"task":"Create a retained fixture file and verify it.","model":id,"mode":"code"
        })).await?;
        let job_id = started["id"].as_str().context("job ID")?.to_owned();
        let task_id = started["task_id"].as_str().context("task ID")?.to_owned();
        let session_id = started["session_id"].as_str().context("session ID")?.to_owned();
        let held = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(bytes) = fs::read(f.bin.join("active-stream-held.json")) {
                    if let Ok(value) = serde_json::from_slice::<Value>(&bytes) { break value; }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("fake runtime never reached active stream barrier")?;
        // Require receipt of real visible SSE before triggering the crash.
        let partial = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(event) = service.as_ref().unwrap().engine.store().last_task_event(&task_id,"model.stream")? {
                    if event["payload"]["text"].as_str().is_some_and(|text| text.contains("The earlier fixture edit is retained") && text.len() >= 4000) { break Ok::<_,anyhow::Error>(event); }
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("partial SSE was not durably observed")??;
        let runtime = service.as_ref().unwrap().engine.local_runtime().loaded().context("active runtime missing")?;
        let pid = runtime.pid.context("owned PID missing")? as u64;
        let (start, state) = process(pid).context("owned runtime missing from proc")?;
        ensure!(state != 'Z' && held["pid"] == pid, "barrier belongs to different/dead runtime");
        ensure!(fs::read_to_string(f.project.join("retained.txt"))? == "retained before runtime exit\n", "real edit absent before crash");
        ensure!(!f.project.join("unexecuted.txt").exists(), "partial tool ran before crash");
        ensure!(service.as_ref().unwrap().engine.job(&job_id)?.context("active job missing")?.status == "running", "task ended before crash");
        let expected_socket = f.bin.join(format!("active-stream-exit-{pid}.sock"));
        ensure!(held["socket"] == expected_socket.to_string_lossy().as_ref(), "unexpected private exit socket");
        let mut exit = tokio::time::timeout(Duration::from_secs(2), tokio::net::UnixStream::connect(&expected_socket)).await.context("active exit connect deadline")??;
        exit.write_all(b"exit").await?;
        drop(exit);
        exited(pid,start).await?;
        // Keep configured retry defaults. Retries cannot respawn a runtime or
        // execute incomplete tool calls; their budget must eventually fail.
        let done = tokio::time::timeout(Duration::from_secs(20), service.as_ref().unwrap().engine.wait(&job_id)).await.context("active crash did not reach bounded terminal")??;
        let store = service.as_ref().unwrap().engine.store();
        let saved = store.job(&job_id)?.context("durable failed job missing")?;
        let events = store.events_after(&session_id,0,Some(done.event_cursor),1000)?;
        ensure!(done.status == "failed" && !done.summary.is_empty() && saved["result"]["success"] == false, "runtime death was not failed: {saved}");
        ensure!(saved["result"]["verification"]["verified"] == false && saved["result"]["verification"]["status"] != "passed", "failed task claimed a passed check");
        ensure!(events.iter().filter(|e| e["task_id"] == task_id && e["type"] == "agent.completed").count() == 1, "terminal event not exactly once");
        ensure!(events.iter().filter(|e| e["task_id"] == task_id && e["type"] == "tool.started").count() == 1 && events.iter().any(|e| e["task_id"] == task_id && e["type"] == "tool.completed" && e["payload"]["tool"] == "write_file" && e["payload"]["success"] == true), "completed write lost or partial tool executed");
        ensure!(!events.iter().any(|e| e["task_id"] == task_id && (e["type"] == "verification.receipt" || (e["type"] == "tool.started" && e["payload"]["tool"] == "exec"))), "check was fabricated/executed before retry");
        let failed_response = events.iter().find(|e| e["type"] == "model.response_metadata" && e["payload"]["message_id"] == partial["payload"]["message_id"]).context("partial response failure metadata missing")?;
        ensure!(failed_response["payload"]["accepted"] == false && failed_response["payload"]["failure_kind"] == "disconnected" && failed_response["payload"]["finish_marker_seen"] == false && failed_response["payload"]["tool_call_slots"] == 1, "wrong active stream failure metadata: {failed_response}");
        ensure!(events.iter().any(|e| e["type"] == "model.stream_end" && e["payload"]["message_id"] == partial["payload"]["message_id"] && e["payload"]["complete"] == false), "partial stream not marked incomplete");
        let retries = events.iter().filter(|e| e["type"] == "model.retry").count();
        ensure!(retries <= cfg.agent.model_retries, "retry budget exceeded");
        let catalog = call(service.as_ref().unwrap(),"GET","/api/local-models",Value::Null).await?;
        ensure!(catalog["loaded"].is_null(), "dead runtime remains Loaded");
        ensure!(process(pid).is_none_or(|(now,_)| now != start), "owned exited child was not reaped by observation");
        tokio::time::timeout(Duration::from_secs(2), async {
            while !service.as_ref().unwrap().engine.local_runtime().last_error(&id).is_some_and(|e| e.contains("active-generation fixture exit")) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }).await.context("runtime crash tail missing")?;
        ensure!(lines(&f.bin.join("launches.jsonl")).len() == 1 && lines(&f.bin.join("requests.jsonl")).len() == 2, "failure automatically relaunched/replayed completed work");
        ensure!(sentinel.try_wait()?.is_none(), "unrelated process stopped on runtime death");
        service.as_ref().unwrap().engine.shutdown().await?;
        drop(service.take());
        service = Some(Service::open(f.paths.clone(),Some(f.project.clone()))?);
        ensure!(service.as_ref().unwrap().engine.store().job(&job_id)? == Some(saved.clone()) && service.as_ref().unwrap().engine.store().events_after(&session_id,0,Some(done.event_cursor),1000)? == events, "failed task/error/partial stream changed after reopen");
        let verification = call(service.as_ref().unwrap(),"GET",&format!("/api/jobs/{job_id}/verification"),Value::Null).await?;
        ensure!(verification["verified"] == false && verification["status"] != "passed", "reopen reassessed failed task as verified");
        ensure!(fs::read_to_string(f.project.join("retained.txt"))? == "retained before runtime exit\n" && !f.project.join("unexecuted.txt").exists(), "work not preserved after failure/reopen");
        // Only the fixture failure switch changes; model/config/budgets stay
        // byte-identical. This is one explicit new task, not an invisible retry.
        fs::remove_file(f.bin.join("active-exit-armed"))?;
        let restarted = call(service.as_ref().unwrap(),"POST","/api/jobs",json!({"workspace":f.project,"task":"Check the retained fixture file without editing it.","model":id,"mode":"code"})).await?;
        let retry_id = restarted["id"].as_str().context("explicit retry ID")?;
        let retried = tokio::time::timeout(Duration::from_secs(15),service.as_ref().unwrap().engine.wait(retry_id)).await.context("explicit retry task deadline")??;
        ensure!(retried.status == "completed" && retried.result.as_ref().context("retry result")?["verification"]["verified"] == true, "explicit retry/check failed: {retried:?}");
        let retry_pid = service.as_ref().unwrap().engine.local_runtime().loaded().context("retry runtime missing")?.pid.context("retry PID missing")? as u64;
        let retry_start = process(retry_pid).context("retry runtime absent")?.0;
        ensure!(lines(&f.bin.join("launches.jsonl")).len() == 2, "explicit retry did not launch exactly once");
        ensure!(service.as_ref().unwrap().engine.store().job(&job_id)? == Some(saved.clone()) && service.as_ref().unwrap().engine.store().events_after(&session_id,0,Some(done.event_cursor),1000)? == events, "new task overwrote failed task evidence");
        ensure!(fs::read(&model)? == model_bytes && fs::read(f.project.join(".git/index"))? == original_index && git(&f.project,&["rev-parse","HEAD"])? == original_head, "model/HEAD/index changed");
        ensure!(fs::read_to_string(f.project.join("hello.txt"))? == "unstaged user bytes\n" && fs::read_to_string(f.project.join("unrelated.txt"))? == "unrelated user bytes\n" && fs::read(f.project.join("check_retained.py"))? == check_bytes, "unrelated project/check bytes changed");
        service.as_ref().unwrap().engine.shutdown().await?;
        exited(retry_pid,retry_start).await?;
        ensure!(process(retry_pid).is_none_or(|(now,_)| now != retry_start), "explicit retry child was not reaped after shutdown");
        ensure!(sentinel.try_wait()?.is_none(), "runtime shutdown killed unrelated process");
        Ok::<_,anyhow::Error>(json!({"failed_job":saved,"partial_event":partial["id"],"failure_metadata":failed_response,"retries":retries,"old_pid":pid,"retry_pid":retry_pid,"retry_job":retried.id,"retry_verification":retried.result.unwrap()["verification"],"launches":2,"requests":lines(&f.bin.join("requests.jsonl")).len()}))
    }).catch_unwind().await;

    // Cleanup remains outside the caught observation and before all outcome
    // assertions. Signals target only retained owned Child handles.
    let shutdown = if let Some(current) = service.as_ref() {
        tokio::time::timeout(Duration::from_secs(20), current.engine.shutdown()).await
    } else {
        Ok(Ok(()))
    };
    let sentinel_survived = sentinel.try_wait().map(|status| status.is_none());
    let _ = sentinel.start_kill();
    let sentinel_reaped = tokio::time::timeout(Duration::from_secs(5), sentinel.wait()).await;
    eprintln!("ACTIVE_RUNTIME_EXIT panic={} result={:?}; shutdown={shutdown:?}; sentinel={sentinel_reaped:?}",observed.is_err(),observed.as_ref().ok());
    assert!(matches!(shutdown, Ok(Ok(()))), "{shutdown:?}");
    assert!(matches!(sentinel_reaped, Ok(Ok(_))), "{sentinel_reaped:?}");
    assert!(
        matches!(sentinel_survived, Ok(true)),
        "unrelated process did not survive cleanup: {sentinel_survived:?}"
    );
    match observed {
        Ok(result) => {
            result.expect("active runtime fixture/acceptance failed");
        }
        Err(panic) => std::panic::resume_unwind(panic),
    }
}
