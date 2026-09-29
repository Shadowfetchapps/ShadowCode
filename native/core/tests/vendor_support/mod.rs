//! Fake vendor CLIs for the vendor tests. They speak the documented
//! protocols (Codex app-server JSON-RPC, `codex exec --json`, login/logout
//! subcommands) and are steered by a `fake.json` next to the script, because
//! probes clear the environment. They never reach a real vendor.
#![allow(dead_code)]
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

pub const FAKE_CODEX: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
def cfg():
    try:
        with open(os.path.join(HERE, "fake.json")) as f:
            return json.load(f)
    except Exception:
        return {}
C = cfg()
def mark(name, text=""):
    with open(os.path.join(HERE, name), "a") as f:
        f.write(text + "\n")
def send(o):
    sys.stdout.write(json.dumps(o) + "\n")
    sys.stdout.flush()
args = sys.argv[1:]
KEYS = ["OPENROUTER_API_KEY", "ANTHROPIC_API_KEY","ANTHROPIC_AUTH_TOKEN","OPENAI_API_KEY","CODEX_API_KEY","CURSOR_API_KEY","XAI_API_KEY","GROK_API_KEY","GEMINI_API_KEY","GOOGLE_API_KEY"]
if "--version" in args:
    print("codex-cli 0.158.0-fake"); sys.exit(0)
if args[:1] == ["--help"]:
    print("Commands:\n  exec\n  app-server\n  login\n  logout"); sys.exit(0)
if args[:2] == ["login", "status"]:
    if C.get("auth"):
        print("Logged in using an API key" if C.get("auth") == "apiKey" else "Logged in using ChatGPT"); sys.exit(0)
    print("Not logged in"); sys.exit(1)
if args[:1] == ["login"]:
    mark("login_ran", " ".join(args) + " keys=" + ",".join(k for k in KEYS if k in os.environ))
    print("Starting local login server on http://localhost:1455.")
    print("If your browser did not open, navigate to this URL to authenticate:")
    print("https://auth.example.invalid/oauth/authorize?client_id=fake&code_challenge=abc123&state=xyz")
    sys.stdout.flush()
    time.sleep(float(C.get("login_sleep", 0)))
    sys.exit(int(C.get("login_exit", 0)))
if args[:1] == ["logout"]:
    mark("logout_ran"); print("Successfully logged out"); sys.exit(0)
if args[:1] == ["exec"]:
    mark("exec_ran")
    sys.stdin.read()
    send({"type":"thread.started","thread_id":"exec-thread"})
    send({"type":"item.completed","item":{"id":"m","type":"agent_message","text":"exec answer"}})
    send({"type":"turn.completed","usage":{"input_tokens":1,"output_tokens":1}})
    sys.exit(0)
if args[:1] != ["app-server"]:
    sys.exit(2)
if C.get("exit_on_start"):
    sys.exit(3)
def snapshot(used=None, reached=None):
    return {"limitId":"codex","limitName":None,"normalModelSlug":None,
        "primary":{"usedPercent":C.get("used", 40) if used is None else used,"windowDurationMins":10080,"resetsAt":1790454596},
        "secondary":None,"credits":{"hasCredits":False,"unlimited":False,"balance":"0"},
        "planType":"pro","rateLimitReachedType":reached}
def rate_limits():
    s = snapshot(reached=C.get("reached"))
    return {"rateLimits":s,"rateLimitsByLimitId":{"codex":s,"base_model_inference":{"limitId":"base_model_inference","limitName":"gpt-reserve","normalModelSlug":"gpt-5.6-luna","primary":{"usedPercent":5,"windowDurationMins":10080}}},"ordinaryUsageAllowed":True}
active_thread = None
active_turn = None
turn_count = 0
for raw in sys.stdin:
    m = json.loads(raw)
    method = m.get("method"); mid = m.get("id"); params = m.get("params") or {}
    if method == "initialize":
        if C.get("turn") == "startup_malformed":
            mark("transport.pid", str(os.getpid()))
            for _ in range(40):
                print("not JSON", flush=True)
            time.sleep(20)
            continue
        if C.get("turn") == "startup_oversized":
            mark("transport.pid", str(os.getpid()))
            sys.stdout.write("x" * 4_000_001); sys.stdout.flush()
            time.sleep(20)
            continue
        if C.get("turn") == "startup_protocol":
            mark("transport.pid", str(os.getpid()))
            until = time.monotonic() + 20
            while time.monotonic() < until:
                send({"jsonrpc":"2.0","method":"future/heartbeat","params":{}})
                time.sleep(0.01)
            continue
        if C.get("reject_initialize"):
            send({"jsonrpc":"2.0","id":mid,"error":{"code":-1,"message":"unsupported client"}}); continue
        send({"jsonrpc":"2.0","id":mid,"result":{"userAgent":"fake"}})
    elif method == "account/read":
        auth = C.get("auth")
        acct = None
        if auth == "apiKey":
            acct = {"type":"apiKey"}
        elif auth:
            acct = {"type":"chatgpt","email":C.get("email","a@example.invalid"),"planType":"pro"}
        send({"jsonrpc":"2.0","id":mid,"result":{"account":acct,"requiresOpenaiAuth":True}})
    elif method == "account/rateLimits/read":
        if C.get("rate_error"):
            send({"jsonrpc":"2.0","id":mid,"error":{"code":-1,"message":"rate limits unavailable"}})
        else:
            send({"jsonrpc":"2.0","id":mid,"result":rate_limits()})
    elif method == "model/list":
        send({"jsonrpc":"2.0","id":mid,"result":{"data":[
            {"id":"gpt-6-astra","displayName":"GPT-6-Astra","isDefault":True,"inputModalities":["text","image"] if C.get("images", True) else ["text"]},
            {"id":"gpt-5.6-luna","displayName":"GPT-5.6-Luna","isDefault":False,"inputModalities":["text","image"] if C.get("images", True) else ["text"]}]}})
    elif method in ("thread/start", "thread/resume"):
        active_thread = params.get("threadId") or "thr-1"
        mark("threads.log", method + " " + json.dumps(params.get("model")) + " " + json.dumps(params.get("threadId")))
        mark("sandbox.log", json.dumps({"sandbox": params.get("sandbox"), "cwd": params.get("cwd"), "pwd": os.getcwd()}))
        mark("developer.log", json.dumps(params.get("developerInstructions")))
        if C.get("block_stdin"):
            import fcntl
            fcntl.fcntl(sys.stdin.fileno(), fcntl.F_SETPIPE_SZ, 4096)
        if C.get("close_stdin_after_thread"):
            os.close(sys.stdin.fileno())
        send({"jsonrpc":"2.0","id":mid,"result":{"thread":{"id":active_thread}}})
        if C.get("close_stdin_after_thread"):
            time.sleep(20)
            sys.exit(0)
        if C.get("block_stdin"):
            mark("stdin_blocked", str(os.getpid()))
            until = time.monotonic() + 20
            while not os.path.exists(os.path.join(HERE, "release_stdin")) and time.monotonic() < until:
                time.sleep(0.01)
            sys.exit(0)
    elif method == "turn/start":
        assert params["threadId"] == active_thread
        turn_count += 1
        active_turn = "turn-" + str(turn_count)
        text = "".join(i.get("text","") for i in params.get("input", []) if i.get("type") == "text")
        mark("prompts.log", json.dumps(text))
        send({"jsonrpc":"2.0","id":mid,"result":{"turn":{"id":active_turn}}})
        mode = C.get("turn", "ok")
        # A prompt that contains a key runs that mode instead (roles tests).
        for key, value in (C.get("mode_when") or {}).items():
            if key in text:
                mode = value
                break
        if mode == "pause_budget" and not os.path.exists(os.path.join(HERE, "interrupted")):
            mark("pause_ready")
            continue
        if mode == "continuous_protocol":
            mark("transport.pid", str(os.getpid()))
            until = time.monotonic() + 20
            while time.monotonic() < until:
                # Both ignored frames and meaningful accounting frames are
                # valid protocol traffic; neither should extend a run forever.
                send({"jsonrpc":"2.0","method":"future/heartbeat","params":{}})
                send({"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"threadId":active_thread,"turnId":active_turn,"tokenUsage":{"last":{"inputTokens":1,"outputTokens":1}}}})
                time.sleep(0.01)
            continue
        if mode == "aggregate_protocol":
            mark("transport.pid", str(os.getpid()))
            for _ in range(34):
                send({"jsonrpc":"2.0","method":"future/heartbeat","params":{"padding":"x" * (2 * 1024 * 1024)}})
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"completed"}}})
            continue
        if mode == "aggregate_text":
            mark("transport.pid", str(os.getpid()))
            for n in range(72):
                send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"m1","delta":"x" * (128 * 1024)}})
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"completed"}}})
            continue
        if mode == "stream_approval":
            for _ in range(300):
                send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"m1","delta":"x"}})
            send({"jsonrpc":"2.0","id":77,"method":"item/commandExecution/requestApproval","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"command-" + str(turn_count),"startedAtMs":0,"command":"printf fixture","reason":"fixture approval"}})
            continue
        if mode == "stream_wait":
            send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"m1","delta":"live prefix"}})
            mark("stream_waiting")
            until = time.monotonic() + 5
            while not os.path.exists(os.path.join(HERE, "observed_stream")) and time.monotonic() < until:
                time.sleep(0.01)
            send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"m1","delta":" and end"}})
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"completed"}}})
            continue
        if mode == "limit":
            send({"jsonrpc":"2.0","method":"account/rateLimits/updated","params":{"rateLimits":snapshot(used=100, reached="rate_limit_reached")}})
            time.sleep(10)
            continue
        if mode == "fail":
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"failed","error":{"message":"No such file or directory (os error 2)"}}}})
            continue
        if mode == "approval":
            send({"jsonrpc":"2.0","id":77,"method":"item/commandExecution/requestApproval","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"command-" + str(turn_count),"startedAtMs":0,"command":"rm -rf build","reason":"clean"}})
            continue
        if mode == "slow":
            time.sleep(float(C.get("slow", 2)))
        if mode == "fragmented":
            frame = json.dumps({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"m1","delta":"complete fragmented reply"}})
            sys.stdout.write(frame[:45]); sys.stdout.flush()
            time.sleep(0.7)  # crosses the runner's 250 ms cancellation poll
            sys.stdout.write(frame[45:] + "\n"); sys.stdout.flush()
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"completed"}}})
            continue
        if mode == "oversized_stdout":
            mark("transport.pid", str(os.getpid()))
            sys.stdout.write("x" * 4_000_001); sys.stdout.flush()
            time.sleep(120)  # deliberately never terminates the line
            continue
        if mode == "edit":
            # Write and delete project files with the CLI's own tools, as a
            # real vendor would, relative to the working directory.
            for name, text in (C.get("edits") or {}).items():
                with open(os.path.join(os.getcwd(), name), "w") as f:
                    f.write(text)
            for name in C.get("deletes") or []:
                os.remove(os.path.join(os.getcwd(), name))
        reply = "fake answer"
        for key, value in (C.get("replies") or {}).items():
            if key in text:
                reply = value
                break
        if mode == "env":
            reply = "API keys visible: " + (",".join(k for k in KEYS if k in os.environ) or "none") + "; marker=" + os.environ.get("SHADOWCODE_FAKE_MARKER", "absent")
        for n in (1, 2):
            send({"jsonrpc":"2.0","method":"thread/tokenUsage/updated","params":{"threadId":active_thread,"turnId":active_turn,"tokenUsage":{"last":{"inputTokens":10,"outputTokens":5,"totalTokens":15},"total":{"inputTokens":1000*n,"outputTokens":500*n,"totalTokens":1500*n}}}})
        send({"jsonrpc":"2.0","method":"account/rateLimits/updated","params":{"rateLimits":snapshot(used=41)}})
        send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"m1","delta":reply}})
        send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"completed"}}})
    elif mid == 77:
        decision = (m.get("result") or {}).get("decision")
        send({"jsonrpc":"2.0","method":"item/agentMessage/delta","params":{"threadId":active_thread,"turnId":active_turn,"itemId":"m1","delta":"decision=" + str(decision)}})
        send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"completed"}}})
    elif method == "turn/interrupt":
        mark("interrupted")
        if C.get("turn") == "pause_budget":
            send({"jsonrpc":"2.0","method":"turn/completed","params":{"threadId":active_thread,"turn":{"id":active_turn,"status":"interrupted"}}})
        send({"jsonrpc":"2.0","id":mid,"result":{}})
"#;

/// A directory holding `codex` (the fake) and its `fake.json`.
pub struct FakeCodex {
    pub dir: PathBuf,
}

impl FakeCodex {
    pub fn new(root: &Path, config: Value) -> Self {
        let dir = root.join("fake-bin");
        fs::create_dir_all(&dir).unwrap();
        let script = dir.join("codex");
        fs::write(&script, FAKE_CODEX).unwrap();
        let mut perm = fs::metadata(&script).unwrap().permissions();
        perm.set_mode(0o755);
        fs::set_permissions(&script, perm).unwrap();
        let fake = Self { dir };
        fake.configure(config);
        fake
    }
    pub fn binary(&self) -> String {
        self.dir.join("codex").to_string_lossy().into_owned()
    }
    pub fn configure(&self, config: Value) {
        fs::write(self.dir.join("fake.json"), config.to_string()).unwrap();
    }
    pub fn marker(&self, name: &str) -> Option<String> {
        fs::read_to_string(self.dir.join(name)).ok()
    }
}

/// `cli_agents` pointing Codex at the fake and every other vendor at a path
/// that does not exist, so no real vendor CLI is ever started.
pub fn cli_agents(fake: &FakeCodex) -> Value {
    json!({
        "codex_binary": fake.binary(),
        "claude_binary": "/nonexistent/shadowcode-test/claude",
        "cursor_binary": "/nonexistent/shadowcode-test/cursor-agent",
        "antigravity_binary": "/nonexistent/shadowcode-test/agy",
        "grok_binary": "/nonexistent/shadowcode-test/grok",
        "stall_timeout_sec": 30,
        "approval_timeout_sec": 15,
    })
}

pub async fn eventually<T>(mut probe: impl FnMut() -> Option<T>, what: &str) -> T {
    for _ in 0..400 {
        if let Some(value) = probe() {
            return value;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("timed out waiting for {what}");
}
