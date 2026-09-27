// Real PTY smoke/lifecycle test. Node and util-linux are test tools only.
import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { createServer } from "node:http";
import { mkdtemp, mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";
const execute = promisify(execFile);
let terminalCounter = 0;
const root = fileURLToPath(new URL("../", import.meta.url));
const binary =
  process.env.SHADOW_DESKTOP_BINARY ||
  path.join(root, "target/debug/shadowcode");
const binaryArgs = JSON.parse(process.env.SHADOW_CLI_ARGS || "[]");
const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-tui-"));
const profile = path.join(scratch, "profile"),
  project = path.join(scratch, "project");
const artifacts = path.resolve(
  process.env.SHADOW_TUI_ARTIFACTS || path.join(root, "artifacts/native-tui"),
);
await mkdir(project);
await mkdir(path.join(profile, "config"), { recursive: true });
await mkdir(artifacts, { recursive: true });
await writeFile(
  path.join(project, "README.md"),
  "# Terminal fixture\nlocal-file-evidence\n",
);
await rm(path.join(artifacts, "result.json"), { force: true });
await rm(path.join(artifacts, "failure.txt"), { force: true });
const env = { ...process.env, TERM: "xterm-256color" };
delete env.DISPLAY;
delete env.WAYLAND_DISPLAY;
// Exercise real color output even when the invoking tool disables colors.
delete env.NO_COLOR;
env.COLORTERM = "truecolor";
const children = new Set(),
  sockets = new Set(),
  requests = [];
let fixtureError;
const delay = (ms) => new Promise((r) => setTimeout(r, ms));
async function until(label, fn, timeout = 15000) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    if (fixtureError) throw fixtureError;
    try {
      const value = await fn();
      if (value) return value;
    } catch (e) {
      last = e;
    }
    await delay(50);
  }
  throw Error(`${label} timed out: ${last || ""}`);
}
const model = createServer(async (req, res) => {
  try {
    if (req.method === "GET") {
      res.end(JSON.stringify({ data: [{ id: "tui-fixture" }] }));
      return;
    }
    let body = "";
    for await (const chunk of req) body += chunk;
    const payload = JSON.parse(body);
    requests.push(payload);
    const prompt = payload.messages
      .filter((m) => m.role === "user")
      .at(-1).content;
    const current = payload.messages.slice(
      payload.messages.findLastIndex((m) => m.role === "user") + 1,
    );
    if (prompt.includes("HANG")) {
      res.writeHead(200, { "Content-Type": "application/json" });
      res.flushHeaders();
      return;
    }
    const message =
      prompt.includes("APPROVE") && !current.some((m) => m.role === "tool")
        ? {
            role: "assistant",
            content: "Request exact command approval.",
            tool_calls: [
              {
                id: `approval-${requests.length}`,
                type: "function",
                function: {
                  name: "exec",
                  arguments: JSON.stringify({
                    command: "printf approved > tui-approved.txt",
                  }),
                },
              },
            ],
          }
        : !current.some((m) => m.role === "tool")
          ? {
              role: "assistant",
              content: "Read local evidence.",
              tool_calls: [
                {
                  id: `read-${requests.length}`,
                  type: "function",
                  function: {
                    name: "read_file",
                    arguments: JSON.stringify({ path: "README.md" }),
                  },
                },
              ],
            }
          : { role: "assistant", content: `Terminal result: ${prompt}` };
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(
      JSON.stringify({
        choices: [
          {
            message,
            finish_reason: message.tool_calls ? "tool_calls" : "stop",
          },
        ],
        usage: { prompt_tokens: 30, completion_tokens: 10, total_tokens: 40 },
      }),
    );
  } catch (e) {
    fixtureError = e;
    res.writeHead(500);
    res.end(String(e));
  }
});
model.on("connection", (s) => {
  sockets.add(s);
  s.on("close", () => sockets.delete(s));
});
await new Promise((r) => model.listen(0, "127.0.0.1", r));
await writeFile(
  path.join(profile, "config/config.yaml"),
  JSON.stringify({
    model: {
      provider: "local",
      name: "tui-fixture",
      default: "tui-fixture",
      endpoint: `http://127.0.0.1:${model.address().port}/v1`,
    },
    trusted_workspaces: [project],
    agent: { model_retries: 0 },
    onboarding: { completed: true, workspace: project },
    ui: { notify: false },
  }),
);
function launch(args, tty = false) {
  const argv = [
    ...binaryArgs,
    "--profile",
    profile,
    "--workspace",
    project,
    ...args,
  ];
  const quote = (s) => `'${s.replaceAll("'", "'\\''")}'`;
  const ttyFile = path.join(scratch, `terminal-${terminalCounter++}.tty`);
  const command = `tty > ${quote(ttyFile)}; stty rows 32 cols 110; before=$(stty -g); ${[binary, ...argv].map(quote).join(" ")}; result=$?; after=$(stty -g); if [ "$before" = "$after" ]; then printf '\\nTERMINAL_RESTORED\\n'; else printf '\\nTERMINAL_BROKEN\\n'; fi; exit "$result"`;
  const child = tty
    ? spawn(
        "script",
        ["--quiet", "--return", "--flush", "--command", command, "/dev/null"],
        { env, stdio: "pipe", detached: true },
      )
    : spawn(binary, argv, { env, stdio: "pipe", detached: true });
  children.add(child);
  const output = { stdout: "", stderr: "", code: undefined };
  child.stdout.on("data", (b) => (output.stdout += b));
  child.stderr.on("data", (b) => (output.stderr += b));
  const done = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code) => {
      output.code = code;
      children.delete(child);
      resolve(output);
    });
  });
  return { child, output, done, ttyFile };
}
async function resize(run, rows, columns, expected) {
  const terminal = (await readFile(run.ttyFile, "utf8")).trim();
  assert.match(terminal, /^\/dev\/pts\/\d+$/);
  const start = run.output.stdout.length;
  await execute("stty", [
    "-F",
    terminal,
    "rows",
    String(rows),
    "cols",
    String(columns),
  ]);
  await until(`terminal resize ${columns}x${rows}`, () =>
    run.output.stdout.slice(start).includes(expected),
  );
}
async function finish(run) {
  await until("process exit", () => run.output.code !== undefined, 20000);
  assert.equal(run.output.code, 0, JSON.stringify(run.output));
  return run.output;
}
async function cli(args) {
  const run = launch(["--json", ...args]);
  return JSON.parse((await finish(run)).stdout);
}
let database;
function jobs() {
  if (!database)
    database = new DatabaseSync(path.join(profile, "state/shadow-agent.db"), {
      readOnly: true,
    });
  return database
    .prepare("SELECT payload FROM desktop_jobs")
    .all()
    .map((r) => JSON.parse(r.payload));
}
let tui;
try {
  tui = launch(["tui"], true);
  await until("terminal header", () =>
    tui.output.stdout.includes("SHADOWCODE"),
  );
  await until(
    "conversation created",
    async () => (await cli(["sessions"])).sessions.length === 1,
  );
  // Bracketed paste must insert a multiline Unicode prompt, never execute it.
  tui.child.stdin.write("\x1b[200~Inspect 界\nsecond line\x1b[201~");
  await delay(200);
  assert.equal(requests.length, 0);
  await resize(tui, 8, 30, "Resize terminal");
  await resize(tui, 18, 60, "SHADOWCODE");
  tui.child.stdin.write("\x1bOP");
  await until("compact help", () => tui.output.stdout.includes("Help · PgUp"));
  tui.child.stdin.write("\x1b[6~".repeat(40));
  await delay(150);
  // A width change forces a complete frame: incremental ANSI updates may split words.
  await resize(
    tui,
    18,
    80,
    "When attached to another engine, unrelated tasks continue.",
  );
  tui.child.stdin.write("\x1b[H");
  await delay(150);
  await resize(tui, 18, 81, "ShadowCode · native terminal");
  tui.child.stdin.write("\x1b[6~".repeat(40));
  await delay(150);
  await resize(tui, 40, 110, "unrelated tasks continue.");
  const closingHelp = tui.output.stdout.length;
  tui.child.stdin.write("\x1b");
  await until("Escape closes help before another key", () =>
    tui.output.stdout.slice(closingHelp).includes("\x1b[?25h"),
  );
  await resize(tui, 32, 110, "SHADOWCODE");
  assert.equal(
    requests.length,
    0,
    "Resizing and help must not submit the draft",
  );
  tui.child.stdin.write("\r");
  await until("completed pasted task", () =>
    jobs().some((j) => j.status === "completed"),
  );
  assert.equal(
    requests[0].messages.filter((m) => m.role === "user").at(-1).content,
    "Inspect 界\nsecond line",
  );
  tui.child.stdin.write("APPROVE\r");
  const approval = await until(
    "pending approval",
    async () => (await cli(["approvals"])).approvals[0],
  );
  // Typing a y into the composer never grants permission.
  tui.child.stdin.write("y");
  await delay(200);
  assert.equal((await cli(["approvals"])).approvals[0].id, approval.id);
  tui.child.stdin.write("\x7f");
  // Backend approval visibility precedes the TUI worker's next poll. F4
  // acts on that rendered view, so observe its pending indicator first.
  // Width changes force full frames; ANSI diffs can otherwise split words.
  const pendingFrame = tui.output.stdout.length;
  const terminal = (await readFile(tui.ttyFile, "utf8")).trim();
  assert.match(terminal, /^\/dev\/pts\/\d+$/);
  let approvalColumns = 110;
  await until("terminal pending approval indicator", async () => {
    if (
      tui.output.stdout
        .slice(pendingFrame)
        .includes("1 approval(s) waiting · F4 to review")
    )
      return true;
    approvalColumns = approvalColumns === 110 ? 111 : 110;
    await execute("stty", ["-F", terminal, "cols", String(approvalColumns)]);
    return false;
  });
  const openingApproval = tui.output.stdout.length;
  tui.child.stdin.write("\x1bOS");
  await until("terminal approval dialog", () =>
    tui.output.stdout.slice(openingApproval).includes("DENY · Tab switches"),
  );
  const switchingApproval = tui.output.stdout.length;
  tui.child.stdin.write("\t");
  await until("terminal approval allow state", () =>
    tui.output.stdout.slice(switchingApproval).includes("ALLOW · Tab switches"),
  );
  tui.child.stdin.write("\r");
  await until(
    "approved exact command",
    async () =>
      (await readFile(path.join(project, "tui-approved.txt"), "utf8")) ===
      "approved",
  );
  await until(
    "approval task completed",
    () => jobs().filter((j) => j.status === "completed").length === 2,
  );
  // F3 enters read-only planning; the model receives that routing/mode.
  tui.child.stdin.write("\x1bOR");
  tui.child.stdin.write("PLAN fixture\r");
  await until("plan completion", () =>
    jobs().some((j) => j.mode === "plan" && j.status === "completed"),
  );
  tui.child.stdin.write("HANG terminal-owned\r");
  await until("running owned task", () =>
    jobs().some((j) => j.status === "running"),
  );
  tui.child.stdin.write("\x11");
  await finish(tui);
  assert.match(tui.output.stdout, /TERMINAL_RESTORED/);
  assert.ok(
    jobs().every(
      (j) => !["queued", "running", "cancelling"].includes(j.status),
    ),
  );
  await writeFile(path.join(artifacts, "terminal.ansi"), tui.output.stdout);
  const server = launch(["--json", "serve"]);
  await until("shared owner ready", () =>
    server.output.stderr.includes("serving"),
  );
  const unrelated = await cli(["run", "HANG unrelated", "--detach"]);
  tui = launch(["tui"], true);
  await until("attached terminal ready", () =>
    tui.output.stdout.includes("SHADOWCODE"),
  );
  await until(
    "second conversation created",
    async () => (await cli(["sessions"])).sessions.length >= 3,
  );
  tui.child.stdin.write("HANG queued owned\r");
  const queued = await until("owned follow-up queued", () =>
    jobs().find((j) => j.status === "queued"),
  );
  tui.child.stdin.write("\x11");
  await finish(tui);
  assert.match(tui.output.stdout, /TERMINAL_RESTORED/);
  assert.equal(jobs().find((j) => j.id === queued.id).status, "cancelled");
  assert.equal(
    jobs().find((j) => j.id === unrelated.id).status,
    "running",
    "Quitting an attached terminal must preserve unrelated work",
  );
  server.child.kill("SIGTERM");
  await finish(server);

  await writeFile(
    path.join(artifacts, "result.json"),
    JSON.stringify(
      {
        ok: true,
        requests: requests.length,
        checks: [
          "real PTY startup",
          "multiline Unicode paste preserved through minimum-size and restored layouts",
          "real PTY compact help scrolling and resize recovery",
          "shared CLI attachment",
          "explicit tool approval",
          "read-only planning mode",
          "owned task cancellation on quit",
          "terminal state restoration",
          "attached terminal preserves unrelated tasks and cancels its queued work",
        ],
      },
      null,
      2,
    ),
  );
  console.log(
    `Native terminal PTY checks passed (${requests.length} model requests).`,
  );
} catch (error) {
  await writeFile(
    path.join(artifacts, "failure.txt"),
    `${error.stack}\n${tui?.output.stdout || ""}\n${tui?.output.stderr || ""}`,
  );
  throw error;
} finally {
  for (const child of children) {
    try {
      process.kill(-child.pid, "SIGTERM");
    } catch {}
  }
  await delay(100);
  for (const child of children) {
    try {
      process.kill(-child.pid, "SIGKILL");
    } catch {}
  }
  for (const socket of sockets) socket.destroy();
  await new Promise((r) => model.close(r));
  database?.close();
  await rm(scratch, { recursive: true, force: true });
}
