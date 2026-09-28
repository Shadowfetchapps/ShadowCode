// Exercise the shipped executable without DISPLAY, Python, or a browser.
import assert from "node:assert/strict";
import { spawn, execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdtemp, mkdir, readFile, writeFile, rm, rename } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { DatabaseSync } from "node:sqlite";

const root = fileURLToPath(new URL("../", import.meta.url));
const expectedVersion=JSON.parse(await readFile(path.join(root,"src-tauri/tauri.conf.json"),"utf8")).version;
const binary = process.env.SHADOW_DESKTOP_BINARY || path.join(root, "target/debug/shadowcode");
const binaryArgs = JSON.parse(process.env.SHADOW_CLI_ARGS || "[]");
assert.ok(Array.isArray(binaryArgs) && binaryArgs.every(arg => typeof arg === "string"));
const artifacts = path.resolve(process.env.SHADOW_CLI_ARTIFACTS || path.join(root, "artifacts/native-cli"));
await mkdir(artifacts, { recursive: true });
await rm(path.join(artifacts, "result.json"), { force: true });
await rm(path.join(artifacts, "failure.txt"), { force: true });
const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-cli-"));
const project = path.join(scratch, "project");
const profile = path.join(scratch, "profile");
await mkdir(project); await mkdir(path.join(profile, "config"), { recursive: true });
await writeFile(path.join(project, "README.md"), "# CLI project\nfixture-read-value\n");
const env = { ...process.env };
delete env.DISPLAY; delete env.WAYLAND_DISPLAY;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const children = new Set(), sockets = new Set(), checks = [];
let modelError, requests = 0, hungRequests = 0;
async function until(label, fn, timeout = 15000) {
  const end = Date.now() + timeout; let last;
  while (Date.now() < end) {
    if (modelError) throw modelError;
    try { const value = await fn(); if (value) return value; } catch (error) { last = error; }
    await delay(50);
  }
  throw new Error(`${label} timed out${last ? `: ${last}` : ""}`);
}
function launch(args, { workspace = project, tty = false } = {}) {
  const commandArgs = [...binaryArgs, "--profile", profile, "--workspace", workspace, ...args];
  // util-linux script supplies a real PTY for approval tests. Every argument is
  // shell quoted; no task or filesystem text is interpolated as shell syntax.
  const quote = text => `'${text.replaceAll("'", "'\\''")}'`;
  const child = tty ? spawn("script", ["--quiet", "--return", "--flush", "--command", [binary, ...commandArgs].map(quote).join(" "), "/dev/null"], { env, stdio: "pipe", detached: true })
    : spawn(binary, commandArgs, { env, stdio: "pipe", detached: true });
  const output = { stdout: "", stderr: "", code: undefined, signal: undefined };
  child.stdout.on("data", data => { output.stdout += data; if (output.stdout.length > 2_000_000) child.kill("SIGKILL"); });
  child.stderr.on("data", data => { output.stderr += data; if (output.stderr.length > 2_000_000) child.kill("SIGKILL"); });
  children.add(child);
  const done = new Promise((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (code, signal) => { output.code = code; output.signal = signal; children.delete(child); resolve(output); });
  });
  return { child, output, done, args };
}
async function interrupt(run, signal = "SIGINT") {
  run.child.kill(signal);
}
async function finish(run, code = 0, timeout = 20000) {
  let timer;
  const output = await Promise.race([run.done, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`CLI ${JSON.stringify(run.args)} timed out: ${JSON.stringify(run.output)}`)), timeout); })]).finally(() => clearTimeout(timer));
  // The patched AppImage wrapper waits for cleanup and preserves the native
  // process's exit status, including interruption and approval result codes.
  assert.equal(output.code, code, JSON.stringify(output));
  assert.equal(output.signal, null, JSON.stringify(output));
  if (modelError) throw modelError;
  return output;
}
async function cli(args, code = 0, opts) {
  const output = await finish(launch(["--json", ...args], opts), code);
  try { return JSON.parse(output.stdout); } catch { throw new Error(`Invalid CLI JSON: ${JSON.stringify(output)}`); }
}
async function dead(pid) {
  assert.ok(Number.isSafeInteger(Number(pid)) && Number(pid)>1, "Expected a recorded process PID");
  try { return /\) [ZX] /.test(await readFile(`/proc/${pid}/stat`, "utf8")); } catch (error) { if (error.code !== "ENOENT") throw error; return true; }
}
const model = createServer(async (req, res) => {
  try {
    let body = ""; for await (const chunk of req) body += chunk;
    const payload = JSON.parse(body); const index = requests++;
    assert.equal(payload.model, "native-cli-fixture");
    const prompt = payload.messages.filter(m => m.role === "user").at(-1).content;
    const current = payload.messages.slice(payload.messages.findLastIndex(m => m.role === "user") + 1);
    const hadTool = current.some(m => m.role === "tool");
    const tool = (name, args) => ({ id: `cli-call-${index}`, type: "function", function: { name, arguments: JSON.stringify(args) } });
    if (prompt.includes("PIPE")) {
      res.writeHead(200, { "Content-Type": "text/event-stream" }); res.flushHeaders();
      const interval = setInterval(() => res.write(`data: ${JSON.stringify({choices:[{delta:{content:"Streaming until the client closes its output. "}, finish_reason:null}]})}\n\n`), 20);
      res.on("close", () => clearInterval(interval)); return;
    }
    if (prompt.includes("HANG")) { hungRequests++; res.writeHead(200, { "Content-Type": "application/json" }); res.flushHeaders(); return; }
    let message;
    if (prompt.includes("MCP") && !hadTool) {
      message = { role: "assistant", content: "Requesting the reviewed MCP call.", tool_calls: [tool("mcp_call", { server: "config:cli-peer", tool: "echo", arguments: { message: "native-cli-mcp-ok" } })] };
    } else if (prompt.includes("APPROVE") && !hadTool) {
      message = { role: "assistant", content: "Checking the exact authorized operation.", tool_calls: [tool("exec", { command: "printf approval-ran > approval.txt" })] };
    } else if (prompt.includes("CHECK_WRITE") && !hadTool) {
      message = { role: "assistant", content: "Writing a checkpointed file.", tool_calls: [tool("write_file", { path: "checkpoint.txt", content: "checkpoint-value\n", expected_hash: "missing" })] };
    } else if (!hadTool) {
      message = { role: "assistant", content: "Inspecting this project.", tool_calls: [tool("read_file", { path: "README.md" })] };
    } else {
      if (prompt.includes("CONTINUE")) {
        assert.ok(payload.messages.some(m => m.role === "assistant" && m.content?.includes("CLI completed")), "Continuation must preserve prior assistant history");
        assert.ok(payload.messages[0].content.includes("Prefer the saved offline fixture."), "Continuation must include saved task notes");
      }
      if (prompt.includes("MCP")) {
        const result = JSON.parse(current.filter(m=>m.role==="tool").at(-1).content);
        assert.equal(result.success,true);
        assert.equal(result.output.result.structuredContent.arguments.message,"native-cli-mcp-ok");
      } else if (!prompt.includes("APPROVE") && !prompt.includes("CHECK_WRITE")) assert.ok(current.some(m => m.role === "tool" && m.content.includes("fixture-read-value")), "Read result must reach the model");
      message = { role: "assistant", content: "CLI completed the requested inspection." };
    }
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ choices: [{ message, finish_reason: message.tool_calls ? "tool_calls" : "stop" }], usage: { prompt_tokens: 30, completion_tokens: 10, total_tokens: 40 } }));
  } catch (error) {
    modelError = error;
    res.writeHead(500, { "Content-Type": "application/json" }); res.end(JSON.stringify({ error: String(error) }));
  }
});
model.on("connection", socket => { sockets.add(socket); socket.on("close", () => sockets.delete(socket)); });
await new Promise(resolve => model.listen(0, "127.0.0.1", resolve));
await writeFile(path.join(profile, "config/config.yaml"), JSON.stringify({
  model: { default: "native-cli-fixture", name: "native-cli-fixture", provider: "local", endpoint: `http://127.0.0.1:${model.address().port}/v1`, context_limit: 16384 },
  onboarding: { completed: true, workspace: project }, ui: { notify: false },
}));
let server;
try {
  assert.equal((await finish(launch(["--version"]))).stdout.trim(), `ShadowCode ${expectedVersion}`);
  assert.equal((await finish(launch(["ui", "--version"]))).stdout.trim(), `ShadowCode ${expectedVersion}`);
  assert.match((await finish(launch(["--help"]))).stdout, /serve/);
  await finish(launch(["--unknown-option"]), 2);
  assert.equal((await cli(["health"])).runtime, "rust");
  assert.match((await cli(["run", "READ"], 1)).error, /trust/i);
  assert.match((await cli(["config", "not_a_setting", "true"], 1)).error, /Unknown setting/);
  await cli(["trust"]);
  assert.match((await cli(["run", "READ", "--detach"], 1)).error, /Detached/);
  assert.match((await cli(["background", "start", "--command", "touch unexpected"], 1)).error, /Background processes/);
  assert.equal((await cli(["jobs"])).jobs.length, 0);
  await finish(launch(["models", "--use", "native-cli-fixture", "--context-limit", "32768"]), 2);
  const registered = await cli(["models", "--use", "native-cli-fixture", "--provider", "local", "--endpoint", `http://127.0.0.1:${model.address().port}/v1`, "--context-limit", "32768"]);
  assert.equal(registered.model.context_limit, 32768);
  assert.equal(await cli(["config", "model.context_limit"]), 32768);
  // The update check's switch is settable before it was ever set.
  assert.equal(await cli(["config", "updates.check"]), null);
  assert.equal((await cli(["config", "updates.check", "false"])).updates.check, false);
  assert.equal(await cli(["config", "updates.check"]), false);
  assert.match((await finish(launch(["completions", "bash"]))).stdout, /complete -F _shadowcode/);
  assert.match((await finish(launch(["manpage"]))).stdout, /^\.TH SHADOWCODE 1 /m);
  checks.push("headless startup, arguments, project trust and lifecycle validation");
  const pluginList = await cli(["plugin"]);
  assert.equal(pluginList.format,"native-plugins-v1");
  assert.ok(pluginList.available.some(p=>p.name==="python-expert"));
  const pluginPreview = await cli(["plugin","inspect","linux-expert"]);
  await cli(["plugin","install","linux-expert","--hash","stale"],1);
  await cli(["plugin","install","linux-expert","--hash",pluginPreview.hash]);
  const installedPlugin = (await cli(["plugin"])).installed[0];
  assert.ok((await cli(["skill","--list"])).skills.some(p=>p.name==="linux-expert--appimage"));
  const removePlugin = await cli(["plugin","remove","linux-expert","--hash",installedPlugin.hash]);
  assert.equal(removePlugin.result.retained.length,0);
  const pluginFile = path.join(scratch,"custom-plugin.json");
  await writeFile(pluginFile,JSON.stringify({format:"shadowcode-plugin-v1",name:"cli-bundle",version:"1.0.0",description:"CLI plugin fixture",skills:{inspect:{description:"Inspect files",mode:"review",content:"Read $ARGUMENTS and report actual contents."}}}));
  const imported = await cli(["plugin","inspect","--file",pluginFile]);
  await cli(["plugin","install","--file",pluginFile,"--hash",imported.hash]);
  await writeFile(path.join(project,".shadowcode/skills/cli-bundle--inspect/SKILL.md"),"User-edited skill");
  const customRemoval=await cli(["plugin","remove","cli-bundle","--hash",(await cli(["plugin"])).installed[0].hash]);
  assert.equal(customRemoval.result.retained.length,1);
  assert.equal(await readFile(path.join(project,".shadowcode/skills/cli-bundle--inspect/SKILL.md"),"utf8"),"User-edited skill");
  checks.push("native plugin preview, stale-hash refusal, built-in and custom installation, skill discovery, removal and local edit preservation");
  const projectMap = await cli(["understand"]);
  assert.equal(projectMap.saved, false);
  assert.equal((await cli(["understand", "--save"])).saved, true);
  assert.equal((await cli(["doctor"])).runtime, "rust");
  assert.match((await finish(launch(["doctor"]))).stdout, /Native diagnostics/);
  assert.equal((await cli(["command", "understand"])).headline, "Project map");
  assert.equal((await cli(["command", "doctor"])).headline, "Native diagnostics");
  await finish(launch(["why", "--count", "0"]), 2);
  assert.match((await cli(["why", "../outside"], 1)).error, /outside|escape|traversal/i);
  checks.push("native project inspection, saved map, readable diagnostics and history argument validation");
  const database = new DatabaseSync(path.join(project,"data.db"));
  database.exec("CREATE TABLE items(value TEXT); INSERT INTO items VALUES('cli sqlite')");
  database.close();
  assert.deepEqual((await cli(["sqlite","data.db"])).tables,["items"]);
  assert.equal((await cli(["sqlite","data.db","SELECT value FROM items WHERE value=?","--params",'["cli sqlite"]'])).rows[0].value,"cli sqlite");
  assert.match((await cli(["sqlite","data.db","DELETE FROM items"],1)).error,/read-only/);
  await cli(["sqlite","data.db","SELECT ?","--params","{}"],1);
  await finish(launch(["sqlite","data.db","--limit","0"]),2);
  assert.equal((await cli(["sqlite","data.db","SELECT count(*) AS total FROM items"])).rows[0].total,1);
  checks.push("native SQLite table discovery, bound queries, read-only enforcement and input limits");

  const mcpDefinition = path.join(scratch, "mcp.json");
  await writeFile(mcpDefinition, JSON.stringify({ name: "cli-mcp", command: ["sh", "-c", "touch unexpected-mcp"], env: { TOKEN: "cli-private-mcp-value" } }));
  const mcpRegistered = await cli(["mcp", "add", mcpDefinition]);
  assert.equal(mcpRegistered.format, "native-mcp-v1");
  assert.equal(JSON.stringify(mcpRegistered).includes("cli-private-mcp-value"), false);
  const mcpServer = mcpRegistered.servers[0];
  assert.equal(mcpServer.enabled, false);
  await finish(launch(["mcp", "enable", mcpServer.id]), 2);
  assert.match((await cli(["mcp", "enable", mcpServer.id, "--hash", "0".repeat(64)], 1)).error, /changed|review/i);
  assert.equal((await cli(["mcp", "enable", mcpServer.id, "--hash", mcpServer.hash])).servers[0].enabled, true);
  assert.equal((await cli(["mcp", "disable", mcpServer.id])).servers[0].enabled, false);
  assert.equal((await cli(["mcp", "remove", mcpServer.id, "--hash", mcpServer.hash])).servers.length, 0);
  assert.equal(await readFile(path.join(project, "unexpected-mcp")).then(() => true, () => false), false);
  checks.push("inert MCP registration, private environment metadata, exact-hash project activation, disable and removal");

  const peerPids = path.join(scratch,"mcp-pids.json"), peerRequests = path.join(scratch,"mcp-requests.jsonl");
  await writeFile(mcpDefinition,JSON.stringify({name:"cli-peer",command:["node",path.join(root,"native/core/tests/fixtures/mcp-server.mjs"),"normal"],env:{MCP_PID_FILE:peerPids,MCP_REQUEST_FILE:peerRequests}}));
  const peer = (await cli(["mcp","add",mcpDefinition])).servers[0];
  await cli(["mcp","enable",peer.id,"--hash",peer.hash]);
  assert.equal((await cli(["run","MCP unattended"],2)).status,"needs_approval");
  // Enabling the reviewed server authorizes catalog discovery, not tools/call.
  const peerMessages = async () => (await readFile(peerRequests, "utf8")).trim().split("\n").filter(Boolean).map(line => JSON.parse(line));
  const unattendedMessages = await peerMessages();
  assert.ok(unattendedMessages.some(message => message.method === "initialize"), "Enabled MCP catalog must initialize");
  assert.ok(unattendedMessages.some(message => message.method === "tools/list"), "Enabled MCP catalog must discover tools");
  assert.equal(unattendedMessages.some(message => message.method === "tools/call"), false, "Unapproved MCP request must not invoke a tool");
  for (const pid of JSON.parse(await readFile(peerPids,"utf8"))) await until("Unattended CLI MCP cleanup",()=>dead(pid));
  const peerTask = launch(["run","MCP interactively","--interactive"],{tty:true});
  await until("MCP exact argument prompt",()=>peerTask.output.stdout.includes("[y/N]"));
  assert.match(peerTask.output.stdout,/MCP config:cli-peer \/ echo[\s\S]*"message": "native-cli-mcp-ok"/);
  assert.equal((await peerMessages()).some(message => message.method === "tools/call"), false, "Pending interactive approval must not invoke a tool");
  peerTask.child.stdin.write("y\n");
  await finish(peerTask);
  const approvedCalls = (await peerMessages()).filter(message => message.method === "tools/call");
  assert.equal(approvedCalls.length, 1, "Only the approved tool call may execute");
  assert.equal(approvedCalls[0].params.name, "echo");
  assert.deepEqual(approvedCalls[0].params.arguments, { message: "native-cli-mcp-ok" });
  for (const pid of JSON.parse(await readFile(peerPids,"utf8"))) await until("CLI MCP cleanup",()=>dead(pid));
  await cli(["mcp","remove",peer.id,"--hash",peer.hash]);
  checks.push("MCP refusal without a terminal, exact argument display, real PTY approval, subprocess result and cleanup");

  const hookPath = ".shadowcode/hooks/cli-check.json";
  await mkdir(path.join(project, ".shadowcode/hooks"), { recursive: true });
  await writeFile(path.join(project, hookPath), JSON.stringify({ name: "cli-check", events: ["on_complete"], command: "printf native-cli-hook > hook-result.txt", timeout_sec: 10 }));
  const hook = (await cli(["hooks"])).hooks[0];
  assert.equal(hook.enabled, false);
  await finish(launch(["hooks", "--enable", hookPath]), 2);
  assert.match((await cli(["hooks", "--enable", hookPath, "--hash", "0".repeat(64)], 1)).error, /changed|review/i);
  assert.equal((await cli(["hooks", "--enable", hookPath, "--hash", hook.hash])).hooks[0].enabled, true);

  const task = await cli(["run", "READ this project\r \u001b[31m"]);
  assert.equal(task.status, "completed"); assert.equal(task.usage.total_tokens, 80);
  assert.equal(await readFile(path.join(project, "hook-result.txt"), "utf8"), "native-cli-hook");
  const notes = await cli(["memory", "--task", task.task_id, "Prefer the saved offline fixture."]);
  assert.match(notes.task, /saved offline fixture/);
  assert.equal((await cli(["memory", "--task", task.task_id])).task, notes.task);
  assert.match((await finish(launch(["memory", "--task", task.task_id]))).stdout, /## Task notes/);
  assert.equal((await cli(["command", "memory", "--", `--task ${task.task_id}`])).metadata.task, notes.task);
  assert.match((await cli(["memory", "--task", task.task_id, "--replace", "--expected-hash", "stale", "overwrite"], 1)).error, /changed|hash/i);
  await finish(launch(["memory", "--task", task.task_id, "--replace", "overwrite"]), 2);
  checks.push("persistent task notes, stale replacement rejection and continuation context");
  const continued = await cli(["run", "CONTINUE this inspection", "--session", task.session_id.slice(0, 12)]);
  assert.equal(continued.status, "completed"); assert.equal(continued.session_id, task.session_id);
  const replay = await finish(launch(["jobs", task.id, "--watch"]));
  assert.match(replay.stderr, /Hook: cli-check/);
  const replayed = replay.stdout;
  assert.equal(replayed.split("CLI completed the requested inspection.").length - 1, 1, "Completed jobs replay their own saved response once");
  const events = (await finish(launch(["run", "EVENTS inspect project", "--events"]))).stdout.trim().split("\n").map(line => JSON.parse(line));
  assert.ok(events.some(e => e.type === "event" && e.event.type === "tool.completed"));
  assert.ok(events.some(e => e.type === "event" && e.event.type === "hook.completed" && e.event.payload.success));
  const cursors = events.filter(e => e.type === "event").map(e => e.event.id);
  assert.ok(cursors.every((id, index) => index === 0 || id > cursors[index - 1]), "Events must be emitted once in saved cursor order");
  assert.equal(events.at(-1).type, "result"); assert.equal(events.at(-1).exit_code, 0);
  const badEvents = (await finish(launch(["run", "EVENTS invalid", "--events", "--interactive"]), 1)).stdout.trim().split("\n").map(line => JSON.parse(line));
  assert.equal(badEvents.at(-1).exit_code, 1);
  checks.push("real native tool loop, structured results, ordered events and saved continuation");
  assert.equal((await cli(["hooks", "--disable", hookPath])).hooks[0].enabled, false);
  checks.push("reviewed hook activation, exact content checks, completion execution, event replay and disable");

  const denied = await cli(["run", "APPROVE this command"], 2);
  assert.equal(denied.status, "needs_approval");
  assert.equal(await readFile(path.join(project, "approval.txt")).then(() => true, () => false), false);
  const interactive = launch(["run", "APPROVE interactively", "--interactive"], { tty: true });
  await until("Interactive approval", () => interactive.output.stdout.includes("[y/N]"));
  interactive.child.stdin.write("y\n");
  await finish(interactive);
  assert.equal(await readFile(path.join(project, "approval.txt"), "utf8"), "approval-ran");
  await rm(path.join(project, "approval.txt"));
  const decline = launch(["run", "APPROVE but decline", "--interactive"], { tty: true });
  await until("Interactive denial", () => decline.output.stdout.includes("[y/N]"));
  decline.child.stdin.write("n\n"); await finish(decline);
  assert.equal(await readFile(path.join(project, "approval.txt")).then(() => true, () => false), false);
  const cancelPrompt = launch(["run", "APPROVE then interrupt", "--interactive"], { tty: true });
  await until("Interruptible approval", () => cancelPrompt.output.stdout.includes("[y/N]"));
  cancelPrompt.child.stdin.write("\x03"); await finish(cancelPrompt, 130);
  checks.push("noninteractive approval refusal, actual PTY approval/denial and Ctrl-C at prompt");

  for (const signal of ["SIGINT", "SIGTERM", "SIGHUP"]) {
    const before = hungRequests;
    const hung = launch(["--json", "run", `HANG until ${signal}`]);
    await until("Stalled provider", () => hungRequests > before);
    const observed = await cli(["jobs"]);
    assert.ok(observed.jobs.some(j => j.status === "running"));
    const status = await cli(["status"]);
    assert.equal(status.workspace, project); assert.ok(status.jobs.some(j => j.status === "running"));
    assert.match((await cli(["exec", "touch unexpected"], 1)).error, /foreground CLI/);
    await interrupt(hung, signal);
    assert.equal(JSON.parse((await finish(hung, 130)).stdout).status, "cancelled");
    assert.ok((await cli(["jobs"])).jobs.every(j => !["running", "queued", "cancelling"].includes(j.status)));
  }
  checks.push("SIGINT/SIGTERM/SIGHUP cancellation, concurrent observation and temporary-owner mutation rejection");

  await cli(["sessions", task.session_id.slice(0, 12), "--rename", "CLI unique title % _"]);
  const found = await cli(["sessions", "CLI unique title % _"]);
  assert.equal(found.sessions.length, 1); assert.equal(found.sessions[0].id, task.session_id);
  const exported = path.join(scratch, "conversation.json");
  await cli(["export", "--session", task.session_id, "--format", "json", "--output", exported]);
  const exact = (await finish(launch(["export", "--session", task.session_id, "--format", "json"]))).stdout;
  assert.equal(exact, await readFile(exported, "utf8")); assert.ok(JSON.parse(exact));
  const markdown = path.join(scratch, "conversation.md");
  await cli(["export", "--session", task.session_id, "--output", markdown]);
  const rawMarkdown = (await finish(launch(["export", "--session", task.session_id]))).stdout;
  assert.equal(rawMarkdown, await readFile(markdown, "utf8")); assert.ok(rawMarkdown.includes("\u001b[31m"), "Redirected exports preserve source bytes");
  const written = await cli(["run", "CHECK_WRITE create checkpoint"]);
  assert.equal(await readFile(path.join(project, "checkpoint.txt"), "utf8"), "checkpoint-value\n");
  const checkpoint = await cli(["checkpoints", "--session", written.session_id]);
  assert.notEqual(checkpoint.kind, "error");
  await cli(["checkpoints", "--session", written.session_id, "--undo"]);
  assert.equal(await readFile(path.join(project, "checkpoint.txt")).then(() => true, () => false), false);
  checks.push("literal session search, prefix resolution, exact atomic export and checkpoint rewind");

  await mkdir(path.join(project, ".shadow/skills"), { recursive: true });
  await writeFile(path.join(project, ".shadow/skills/inspect.md"), "---\nmode: review\ndescription: CLI inspection\n---\nInspect this project. $ARGUMENTS\n");
  assert.ok(JSON.stringify(await cli(["skill", "--list"])).includes("inspect"));
  assert.equal((await cli(["skill", "inspect", "CLI_SKILL"])).status, "completed");
  const goalCount = (await cli(["goals"])).goals.length;
  await cli(["goal", "Invalid interactive goal", "--run", "--interactive"], 1);
  assert.equal((await cli(["goals"])).goals.length, goalCount, "Invalid options must not create a goal");
  const goal = await cli(["goal", "HANG persistent goal"]);
  assert.ok((await cli(["goals"])).goals.some(g => g.id === goal.id));
  checks.push("project skills, durable goal creation and validation before mutation");

  server = launch(["--json", "serve"]);
  await until("Headless owner", () => server.output.stderr.includes("serving"));
  assert.match((await cli(["serve"], 1)).error, /already owns/);
  const serverHealth = await cli(["health"]); assert.equal(serverHealth.workspace, project);
  const background = await cli(["background", "start", "--name", "cli-dev", "--command", "printf ready; sleep 60"]);
  assert.ok(background.id);
  const runningBackground = await until("Background logs", async () => {
    const task=await cli(["background", "logs", background.id]);
    return task.output.includes("ready") && task;
  });
  assert.ok(runningBackground.pid > 1);
  assert.equal(await dead(runningBackground.pid), false);
  assert.equal((await cli(["run", "READ while server runs"])).status, "completed");
  const brokenPipe = launch(["run", "PIPE until output closes", "--events"]);
  brokenPipe.child.stdout.once("data", () => brokenPipe.child.stdout.destroy());
  await finish(brokenPipe, 1);
  assert.ok((await cli(["jobs"])).jobs.every(j => !["running", "queued", "cancelling"].includes(j.status)), "A broken output pipe must cancel the task started by that CLI, including on a persistent owner");
  const brokenStatus = launch(["run", "HANG with closed stderr"]);
  brokenStatus.child.stderr.destroy();
  await finish(brokenStatus, 1);
  assert.ok((await cli(["jobs"])).jobs.every(j => !["running", "queued", "cancelling"].includes(j.status)), "Closed status output must not panic or abandon the new task");
  await cli(["background", "stop", background.id.slice(0, 12)]);
  await until("Stopped background process", () => dead(runningBackground.pid));
  const before = hungRequests;
  const detached = await cli(["run", "HANG detached", "--detach"]);
  await until("Detached model request", () => hungRequests > before);
  const observer = launch(["jobs", detached.id, "--watch"]);
  await until("Attached job observer", () => observer.output.stderr.includes("Model:"));
  await interrupt(observer);
  assert.equal(JSON.parse((await finish(observer, 130)).stdout).status, "detached");
  assert.equal((await cli(["jobs", detached.id])).status, "running");
  await cli(["jobs", detached.id, "--cancel"]);

  const waiting = launch(["--json", "run", "APPROVE through another CLI", "--approval", "wait"]);
  const pending = await until("Externally available approval", async () => (await cli(["approvals"])).approvals[0]);
  await cli(["approvals", "--session", pending.session_id, "--id", pending.id, "--decision", "approve"]);
  assert.equal(JSON.parse((await finish(waiting)).stdout).status, "completed");
  assert.equal(await readFile(path.join(project, "approval.txt"), "utf8"), "approval-ran");

  const goalRun = launch(["--json", "goals", "--resume", goal.id]);
  await until("Resumed persistent goal", async () => (await cli(["goals"])).goals.some(g => g.id === goal.id && g.running));
  await interrupt(goalRun); await finish(goalRun, 130);
  assert.equal((await cli(["goals"])).goals.find(g => g.id === goal.id).running, false);
  checks.push("shared persistent owner, background lifecycle, detached jobs, watch-only cancellation, external approval and goal pause");

  const remoteExec = launch(["--json", "exec", "sleep 60 & echo $! > remote-child.pid; wait", "--timeout", "120"]);
  const remoteChild = await until("Remote terminal child", async () => (await readFile(path.join(project, "remote-child.pid"), "utf8")).trim());
  await interrupt(remoteExec, "SIGTERM");
  assert.equal(JSON.parse((await finish(remoteExec, 130)).stdout).status, "cancelled");
  await until("Remote disconnect child cleanup", () => dead(remoteChild));
  const stubborn = await cli(["background", "start", "--name", "stubborn", "--command", "trap '' TERM; sleep 60 & echo $! > stubborn-child.pid; wait"]);
  const stubbornChild = await until("Stubborn child", async () => (await readFile(path.join(project, "stubborn-child.pid"), "utf8")).trim());
  const runningStubborn = await cli(["background","logs",stubborn.id]);
  assert.equal(await dead(runningStubborn.pid), false);
  assert.equal(await dead(stubbornChild), false);
  await interrupt(server, "SIGTERM");
  assert.equal(JSON.parse((await finish(server)).stdout).status, "stopped");
  await until("Persistent parent cleanup", () => dead(runningStubborn.pid));
  await until("Persistent child cleanup", () => dead(stubbornChild));
  assert.ok((await cli(["background", "list"])).tasks.every(t => !["RUNNING", "STARTING", "STOPPING"].includes(t.status)));
  assert.equal(await readFile(path.join(project, "unexpected")).then(() => true, () => false), false);
  checks.push("remote command disconnect and persistent-owner shutdown clean process groups and release the profile");
  const isolatedSource=path.join(scratch,"worktree-source");await mkdir(isolatedSource);
  const fixtureGit=args=>execFileSync("git",["-c","core.hooksPath=/dev/null","-c","user.name=Worktree Test","-c","user.email=test@example.invalid","-c","commit.gpgsign=false",...args],{cwd:isolatedSource,encoding:"utf8"});
  fixtureGit(["init","-q"]);fixtureGit(["config","user.name","Worktree Test"]);fixtureGit(["config","user.email","test@example.invalid"]);await writeFile(path.join(isolatedSource,"README.md"),"fixture-read-value committed\n");fixtureGit(["add","README.md"]);fixtureGit(["commit","-qm","Base"]);
  await writeFile(path.join(isolatedSource,"README.md"),"source local edits\n");
  await cli(["trust"],0,{workspace:isolatedSource});
  assert.equal((await cli(["worktree"],0,{workspace:isolatedSource})).worktrees.length,0);
  const isolated=await cli(["worktree","--create","--reference","HEAD"],0,{workspace:isolatedSource});
  assert.equal(isolated.state,"ready");assert.equal(await readFile(path.join(isolated.path,"README.md"),"utf8"),"fixture-read-value committed\n");
  assert.equal((await cli(["worktree"],0,{workspace:isolatedSource})).worktrees[0].id,isolated.id);
  await cli(["trust"],0,{workspace:isolated.path});
  assert.equal((await cli(["run","READ isolated checkout"],0,{workspace:isolated.path})).status,"completed");
  assert.equal(await readFile(path.join(isolatedSource,"README.md"),"utf8"),"source local edits\n");
  const removal = await cli(["worktree","--inspect",isolated.id],0,{workspace:isolatedSource});assert.equal(removal.can_remove,true);
  assert.match((await cli(["worktree","--remove",isolated.id,"--hash","stale"],1,{workspace:isolatedSource})).error,/changed/);
  assert.equal((await cli(["worktree","--remove",isolated.id,"--hash",removal.hash],0,{workspace:isolatedSource})).state,"removed");
  assert.equal((await cli(["worktree"],0,{workspace:isolatedSource})).worktrees.length,0);
  assert.equal(fixtureGit(["rev-parse",isolated.branch]).trim(),isolated.base_commit);
  checks.push("native worktree creation, inventory, trust, isolated model task, reviewed clean removal and preserved branch");

  const missing=await cli(["worktree","--create"],0,{workspace:isolatedSource});
  await rename(missing.path,path.join(scratch,"retained-missing-checkout"));
  const recovery=await cli(["worktree","--recovery",missing.id],0,{workspace:isolatedSource});
  assert.equal(recovery.commit,missing.base_commit);
  assert.match((await cli(["worktree","--restore",missing.id,"--recovery-hash","stale"],1,{workspace:isolatedSource})).error,/changed/);
  const rescued=await cli(["worktree","--restore",missing.id,"--recovery-hash",recovery.hash],0,{workspace:isolatedSource});
  assert.notEqual(rescued.path,missing.path);
  assert.equal(await readFile(path.join(rescued.path,"README.md"),"utf8"),"fixture-read-value committed\n");
  assert.ok(fixtureGit(["worktree","list","--porcelain"]).includes(missing.path));
  assert.equal(fixtureGit(["rev-parse",missing.branch]).trim(),missing.base_commit);
  checks.push("missing worktree reviewed rescue with stale-hash rejection and original metadata retained");

  await writeFile(path.join(rescued.path,"returned.txt"),"reviewed return\n");
  for(const args of [["add","returned.txt"],["commit","-qm","Worktree result"]])execFileSync("git",["-c","core.hooksPath=/dev/null","-c","user.name=Worktree Test","-c","user.email=test@example.invalid","-c","commit.gpgsign=false",...args],{cwd:rescued.path});
  fixtureGit(["add","README.md"]);fixtureGit(["commit","-qm","Retain source edits"]);
  const returnReview=await cli(["worktree","--review-return",rescued.id],0,{workspace:isolatedSource});
  assert.ok(returnReview.diff.includes("reviewed return"));
  assert.match((await cli(["worktree","--return-changes",rescued.id,"--return-hash","stale"],1,{workspace:isolatedSource})).error,/changed/);
  assert.equal((await cli(["worktree","--return-changes",rescued.id,"--return-hash",returnReview.hash],0,{workspace:isolatedSource})).state,"merge_pending");
  assert.equal(fixtureGit(["rev-parse","HEAD"]).trim(),returnReview.source_head);
  assert.equal(await readFile(path.join(isolatedSource,"returned.txt"),"utf8"),"reviewed return\n");
  assert.equal(await readFile(path.join(isolatedSource,"README.md"),"utf8"),"source local edits\n");
  fixtureGit(["commit","-qm","Reviewed integration"]);
  checks.push("reviewed worktree return preserves divergent source work and requires a separate commit");

  await writeFile(path.join(isolatedSource,"README.md"),"staged source snapshot\n");fixtureGit(["add","README.md"]);
  await writeFile(path.join(isolatedSource,"README.md"),"unstaged source snapshot\n");
  await writeFile(path.join(isolatedSource,"untracked-copy.txt"),"untracked snapshot\n");
  const changesReview=await cli(["worktree","--review-changes"],0,{workspace:isolatedSource});
  assert.ok(changesReview.staged_diff.includes("staged source snapshot"));
  assert.ok(changesReview.unstaged_diff.includes("unstaged source snapshot"));
  assert.match((await cli(["worktree","--copy-changes","--copy-hash","stale"],1,{workspace:isolatedSource})).error,/changed/);
  const copiedChanges=await cli(["worktree","--copy-changes","--copy-hash",changesReview.hash],0,{workspace:isolatedSource});
  assert.equal(await readFile(path.join(copiedChanges.path,"README.md"),"utf8"),"unstaged source snapshot\n");
  assert.equal(execFileSync("git",["show",":README.md"],{cwd:copiedChanges.path,encoding:"utf8"}),"staged source snapshot\n");
  assert.equal(await readFile(path.join(copiedChanges.path,"untracked-copy.txt"),"utf8"),"untracked snapshot\n");
  assert.equal(await readFile(path.join(isolatedSource,"README.md"),"utf8"),"unstaged source snapshot\n");
  assert.equal(fixtureGit(["show",":README.md"]),"staged source snapshot\n");
  checks.push("reviewed dirty-worktree copy preserves staged, unstaged and untracked source changes");

  const repairAdmin=path.join(copiedChanges.common_directory,"worktrees",copiedChanges.id);
  const repairIndex=await readFile(path.join(repairAdmin,"index"));
  await rm(path.join(copiedChanges.path,".git"));
  const repairReview=await cli(["worktree","--review-repair",copiedChanges.id],0,{workspace:isolatedSource});
  assert.equal(repairReview.checkout_pointer,null);
  assert.match((await cli(["worktree","--repair",copiedChanges.id,"--repair-hash","stale"],1,{workspace:isolatedSource})).error,/changed/);
  assert.equal((await cli(["worktree","--repair",copiedChanges.id,"--repair-hash",repairReview.hash],0,{workspace:isolatedSource})).state,"ready");
  assert.deepEqual(await readFile(path.join(repairAdmin,"index")),repairIndex);
  assert.equal(execFileSync("git",["show",":README.md"],{cwd:copiedChanges.path,encoding:"utf8"}),"staged source snapshot\n");
  assert.equal(await readFile(path.join(copiedChanges.path,"README.md"),"utf8"),"unstaged source snapshot\n");
  assert.equal(await readFile(path.join(copiedChanges.path,"untracked-copy.txt"),"utf8"),"untracked snapshot\n");
  checks.push("reviewed worktree connection repair preserves the original index and dirty files");

  // Seed only this disposable, stopped profile. Recent-list limits must not hide
  // older IDs or force fetching multi-megabyte job results to resolve a prefix.
  const historyDb = new DatabaseSync(path.join(profile, "state/shadow-agent.db"));
  const oldProject=path.join(scratch,"old-project"); await mkdir(oldProject);
  historyDb.exec("BEGIN");
  historyDb.prepare("INSERT INTO sessions(id,workspace,created_at,updated_at,status,title) VALUES(?,?,1,1,'idle','Old project history')").run("old-project-session",oldProject);
  historyDb.exec("WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<10050) INSERT INTO sessions(id,workspace,created_at,updated_at,status,title) SELECT printf('noise-session-%05d',i),'/noise',9999999999,9999999999,'idle','Newer fixture' FROM n");
  const putHistoryJob=historyDb.prepare("INSERT INTO desktop_jobs(id,payload) VALUES(?,?)");
  for(let i=0;i<1100;i++){const id=`noise-job-${i.toString().padStart(5,'0')}`;putHistoryJob.run(id,JSON.stringify({id,workspace:project,session_id:task.session_id,task_id:task.task_id,status:"completed",result:{summary:"x".repeat(9000)}}));}
  historyDb.exec("COMMIT");historyDb.close();
  server=launch(["--json","serve"]);await until("History owner",()=>server.output.stderr.includes("serving"));
  assert.equal((await cli(["jobs",task.id])).id,task.id);
  assert.equal((await cli(["jobs",task.id.slice(0,12)])).id,task.id);
  await cli(["sessions",task.session_id.slice(0,12),"--rename","Old history remains reachable"]);
  assert.equal((await cli(["export","--session",task.session_id.slice(0,12)])).content.includes("CLI completed"),true);
  assert.ok((await cli(["export"],0,{workspace:oldProject})).content.includes("Old project history"));
  assert.match((await cli(["jobs","noise-job"],1)).error,/unique/);
  await interrupt(server,"SIGTERM");await finish(server);
  checks.push("indexed full-history IDs and project-default selection beyond 10,000 sessions and 1,000 large jobs");
  await writeFile(path.join(artifacts, "result.json"), JSON.stringify({ passed: true, binary, modelRequests: requests, checks }, null, 2));
  console.log(`Native CLI passed ${checks.length} scenario groups with ${requests} model requests.`);
} catch (error) {
  await writeFile(path.join(artifacts, "failure.txt"), `${error.stack}\nModel error: ${modelError?.stack || "none"}\nModel requests: ${requests}\nPassed: ${checks.join("; ")}\n${[...children].map(c => `Live child: ${c.pid}`).join("\n")}\n`);
  throw error;
} finally {
  for (const child of children) { try { process.kill(-child.pid, "SIGTERM"); } catch {} }
  for (const socket of sockets) socket.destroy(); model.close();
  await delay(3000);
  for (const child of children) { try { process.kill(-child.pid, "SIGKILL"); } catch {} }
  await rm(scratch, { recursive: true, force: true });
}
