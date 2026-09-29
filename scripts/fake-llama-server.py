#!/usr/bin/env python3
"""Test double for llama.cpp's `llama-server` used by the real-window test.

It answers `--version` / `--list-devices`, serves `/health`, `/props` and a
streaming `/v1/chat/completions` that requires the per-launch bearer key
(`LLAMA_API_KEY`). No model is loaded and no GPU is touched.

Turns (decided from the messages after the last user message):
  * a request mentioning `stop-probe` streams one chunk and then waits, so the
    window can stop a running task;
  * `memory-probe` answers in one short reply, without tools;
  * `reconnect-probe` runs one appending shell command (a non-idempotent
    effect), then streams six parts slowly and holds the stream open until
    `release-reconnect` exists next to this file, then finishes;
  * otherwise the model writes hello.txt, then runs one shell command, then
    answers with a short summary.
When `oom-above-ctx` (a number) exists next to this file, a launch whose
`--ctx-size` is larger fails the way llama.cpp does when the KV cache does
not fit in memory. Every launch and request is appended to launches.jsonl /
requests.jsonl next to this file.
"""
import json, os, signal, sys, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HERE = os.path.dirname(os.path.abspath(__file__))
args = sys.argv[1:]


def log(name, value):
    with open(os.path.join(HERE, name), "a") as f:
        f.write(json.dumps(dict(value, at=time.time())) + "\n")


if args == ["--version"]:
    print("version: 0.0.0-test (build 1, commit testdouble)")
    sys.exit(0)
if args == ["--list-devices"]:
    print("Available devices:")
    sys.exit(0)


def opt(name, default=None):
    return args[args.index(name) + 1] if name in args else default


model = opt("-m", "")
port = int(opt("--port"))
ctx = int(opt("--ctx-size", "0"))
key = os.environ.get("LLAMA_API_KEY", "")
log("launches.jsonl", {"argv": args, "pid": os.getpid(), "key_env": bool(key),
                       "key_in_argv": key != "" and key in " ".join(args)})
signal.signal(signal.SIGTERM, lambda *_: os._exit(0))

limit_file = os.path.join(HERE, "oom-above-ctx")
if os.path.exists(limit_file) and ctx > int(open(limit_file).read().strip() or "0"):
    sys.stderr.write(
        "llama_kv_cache_unified: kv_size = %d, type_k = 'f16', type_v = 'f16'\n"
        "ggml_backend_cpu_buffer_type_alloc_buffer: failed to allocate buffer of size 17179869184\n"
        "llama_kv_cache_unified: failed to allocate buffer for kv cache\n"
        "llama_init_from_model: failed to initialize the context: failed to allocate buffer for kv cache\n"
        "common_init_from_params: failed to create context with model '%s'\n"
        "main: exiting due to model loading error\n" % (ctx, model))
    sys.stderr.flush()
    sys.exit(1)


def text_of(message):
    content = message.get("content")
    if isinstance(content, list):
        return " ".join(part.get("text", "") for part in content if isinstance(part, dict))
    return content or ""


def chunk(delta, finish=None, usage=None):
    body = {"choices": [{"delta": delta, "finish_reason": finish}]}
    if usage:
        body["usage"] = usage
    return body


def tool_call(call_id, name, arguments):
    return [
        chunk({"role": "assistant", "content": None, "tool_calls": [
            {"index": 0, "id": call_id, "type": "function",
             "function": {"name": name, "arguments": json.dumps(arguments)}}]}),
        chunk({}, "tool_calls", {"prompt_tokens": 900, "completion_tokens": 24}),
    ]


def answer(content):
    return [
        chunk({"role": "assistant", "content": content}),
        chunk({}, "stop", {"prompt_tokens": 1100, "completion_tokens": 18}),
    ]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *a):
        pass

    def authorized(self):
        return self.headers.get("Authorization", "") == "Bearer " + key

    def reply(self, code, body):
        data = json.dumps(body).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def stream(self, chunks, hang=False):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        for item in chunks:
            self.wfile.write(("data: " + json.dumps(item) + "\n\n").encode())
            self.wfile.flush()
        if hang:
            # Hold the stream open until the client goes away or we are stopped.
            for _ in range(600):
                time.sleep(0.2)
            return
        self.wfile.write(b"data: [DONE]\n\n")

    def reconnect_stream(self):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def send(item):
            self.wfile.write(("data: " + json.dumps(item) + "\n\n").encode())
            self.wfile.flush()

        send(chunk({"role": "assistant", "content": "Reconnect probe:"}))
        for part in range(1, 7):
            time.sleep(0.4)
            send(chunk({"content": " part %d" % part}))
        open(os.path.join(HERE, "reconnect-holding"), "w").close()
        release = os.path.join(HERE, "release-reconnect")
        for _ in range(900):
            if os.path.exists(release):
                break
            time.sleep(0.1)
        send(chunk({"content": ". Finished after the reload."}))
        send(chunk({}, "stop", {"prompt_tokens": 1200, "completion_tokens": 30}))
        self.wfile.write(b"data: [DONE]\n\n")

    def do_GET(self):
        if self.path == "/health":
            return self.reply(200, {"status": "ok"})
        if not self.authorized():
            return self.reply(401, {"error": "Invalid API Key"})
        if self.path == "/props":
            return self.reply(200, {"default_generation_settings": {"n_ctx": ctx},
                                    "modalities": {"vision": False}})
        self.reply(404, {})

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        messages = body.get("messages", [])
        last_user = max((i for i, m in enumerate(messages) if m.get("role") == "user"), default=-1)
        request = text_of(messages[last_user]) if last_user >= 0 else ""
        results = [m for m in messages[last_user + 1:] if m.get("role") == "tool"]
        log("requests.jsonl", {"auth": self.authorized(), "request": request[:200],
                               "tool_results": len(results),
                               "tools": [t["function"]["name"] for t in body.get("tools", [])]})
        if not self.authorized():
            return self.reply(401, {"error": "Invalid API Key"})
        if "stop-probe" in request:
            return self.stream([chunk({"role": "assistant", "content": "Looking at the project"})], hang=True)
        if "memory-probe" in request:
            return self.stream(answer("The project has a README and a greeting file."))
        if "reconnect-probe" in request and body.get("tools"):
            if len(results) == 0:
                return self.stream(tool_call("call_effect", "exec", {"command": "printf 'A\\n' >> effects-A.log"}))
            return self.reconnect_stream()
        if not body.get("tools"):
            return self.stream(answer("I can only chat in this mode."))
        if len(results) == 0:
            return self.stream(tool_call("call_write", "write_file", {
                "path": "hello.txt", "content": "Hello from ShadowCode\n", "expected_hash": "missing"}))
        if len(results) == 1:
            return self.stream(tool_call("call_exec", "exec", {"command": "cat hello.txt"}))
        return self.stream(answer("Created `hello.txt` with a greeting and checked it with `cat hello.txt`."))


server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
server.daemon_threads = True
server.serve_forever()
