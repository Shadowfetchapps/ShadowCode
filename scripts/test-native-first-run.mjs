// Real Tauri/WebKit window test of the first run for someone with no account:
// no vendor CLI on PATH, an empty HOME, no API keys, no local model. The real
// llama.cpp runtime (the managed install) detects this computer's hardware.
//
// The run: onboarding → the model step (three choices; the recommendation
// comes from this computer) → "See other free models" → download one model
// for real from Hugging Face with Pause and Resume → it is selected in the
// composer → one short read-only task on it → Delete → quit. Nothing is
// downloaded before the click. The profile (and the ~2 GB download) lives in
// a scratch folder that is removed afterwards.
//
//   xvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- node scripts/test-native-first-run.mjs
// Environment (the first four as in test-native-desktop.mjs):
//   SHADOW_DESKTOP_BINARY   binary to drive (default target/debug/shadowcode)
//   SHADOW_TAURI_DRIVER     tauri-driver path (default: tauri-driver on PATH)
//   SHADOW_WEBKIT_DRIVER    WebKitWebDriver path (passed as --native-driver)
//   SHADOW_NATIVE_ARTIFACTS screenshots/report directory (default artifacts/first-run; deleted first)
//   SHADOW_FIRST_RUN_LLAMA  llama-server to use (default ~/.local/lib/shadowcode/llama-server)
//   SHADOW_FIRST_RUN_MODEL  catalog id to download (default granite-4.2-3b, the smallest)
//   SHADOW_KEEP_SCRATCH=1   keep the scratch profile
import assert from "node:assert/strict";
import { capturePrivateSession, writePrivateSessionReport } from "./native-test-session.mjs";
import { createServer } from "node:http";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { createWriteStream, existsSync } from "node:fs";
import { mkdtemp, mkdir, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const privateSession = await capturePrivateSession();
const root = fileURLToPath(new URL("../", import.meta.url));
const binary = process.env.SHADOW_DESKTOP_BINARY || path.join(root, "target/debug/shadowcode");
const artifacts = process.env.SHADOW_NATIVE_ARTIFACTS || path.join(root, "artifacts/first-run");
const llama = process.env.SHADOW_FIRST_RUN_LLAMA || path.join(homedir(), ".local/lib/shadowcode/llama-server");
const modelId = process.env.SHADOW_FIRST_RUN_MODEL || "granite-4.2-3b";
assert.ok(existsSync(binary), `Desktop binary missing: ${binary}`);
assert.ok(existsSync(llama), `llama-server missing: ${llama} (set SHADOW_FIRST_RUN_LLAMA)`);
await rm(artifacts, { recursive: true, force: true });
await mkdir(artifacts, { recursive: true });
const axeSource = await readFile(path.join(root, "ui/node_modules/axe-core/axe.min.js"), "utf8");
const run = promisify(execFile);
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const checks = [];
const note = (text) => { checks.push(text); console.log(`  ok  ${text}`); };

// ---------------------------------------------------------------- fixtures
const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-first-run-"));
const project = path.join(scratch, "project");
const home = path.join(scratch, "home");
const profile = path.join(scratch, "profile");
const configDirectory = path.join(profile, "config/shadow-agent");
const downloads = path.join(profile, "data/shadow-agent/local-models");
for (const dir of [project, home, configDirectory, path.join(profile, "data"), path.join(profile, "state"), path.join(profile, "cache"), path.join(scratch, "tmp")])
  await mkdir(dir, { recursive: true });
await writeFile(path.join(project, "README.md"), "# Notes\nA tiny project for the first-run test.\n");
await writeFile(path.join(project, "hello.py"), "print('hello')\n");
await run("git", ["init", "-q"], { cwd: project });
// The real runtime, named explicitly: HOME is empty, so the managed copy under
// ~/.local/lib/shadowcode would not be found.
await writeFile(path.join(configDirectory, "config.yaml"), JSON.stringify({ local_engine: { llama_binary: llama }, ui: { notify: false, theme: "light" } }));

// No accounts: vendor CLIs off PATH, no provider keys, an empty HOME.
const VENDOR_BINARIES = ["codex", "claude", "cursor-agent", "agy", "grok"];
const pathDirs = (process.env.PATH || "").split(":").filter(Boolean);
const vendorDirs = pathDirs.filter((dir) => VENDOR_BINARIES.some((bin) => existsSync(path.join(dir, bin))));
assert.ok(!vendorDirs.some((dir) => ["/usr/bin", "/bin"].includes(dir)), `A vendor CLI is in a system folder (${vendorDirs}); this test can't hide it`);
const nativeEnv = {
  ...process.env,
  HOME: home,
  PATH: pathDirs.filter((dir) => !vendorDirs.includes(dir)).join(":"),
  XDG_CONFIG_HOME: path.join(profile, "config"),
  XDG_DATA_HOME: path.join(profile, "data"),
  XDG_STATE_HOME: path.join(profile, "state"),
  XDG_CACHE_HOME: path.join(profile, "cache"),
  TMPDIR: path.join(scratch, "tmp"),
  GDK_BACKEND: "x11",
  WEBKIT_DISABLE_DMABUF_RENDERER: "1",
};
for (const key of Object.keys(nativeEnv))
  if (/(_API_KEY|_TOKEN|_API_TOKEN)$/.test(key) || key === "OLLAMA_MODELS" || key === "SHADOWCODE_LLAMA_SERVER") delete nativeEnv[key];
delete nativeEnv.WAYLAND_DISPLAY;
delete nativeEnv.WAYLAND_SOCKET;

// ---------------------------------------------------------------- helpers
async function until(label, fn, timeout = 15000, every = 100) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    try { const value = await fn(); if (value) return value; } catch (error) { last = error; }
    await delay(every);
  }
  throw new Error(`${label} timed out${last ? `: ${last.message || last}` : ""}`);
}
async function unusedPort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}
async function dead(pid) {
  try { return /\) [ZX] /.test(await readFile(`/proc/${pid}/stat`, "utf8")); } catch { return true; }
}
async function descendants(pid) {
  const parents = new Map();
  for (const name of await readdir("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      const text = await readFile(`/proc/${name}/stat`, "utf8");
      const rest = text.slice(text.lastIndexOf(")") + 2).split(" ");
      if (rest[0] !== "Z") parents.set(Number(name), Number(rest[1]));
    } catch { /* exited */ }
  }
  const out = [];
  const walk = (p) => { for (const [child, parent] of parents) if (parent === p) { out.push(child); walk(child); } };
  walk(pid);
  return out;
}
const port = await unusedPort(), nativePort = await unusedPort();
const driverLog = createWriteStream(path.join(artifacts, "webdriver.log"));
const driverArgs = ["--port", String(port), "--native-port", String(nativePort)];
if (process.env.SHADOW_WEBKIT_DRIVER) driverArgs.push("--native-driver", process.env.SHADOW_WEBKIT_DRIVER);
const driver = spawn(process.env.SHADOW_TAURI_DRIVER || "tauri-driver", driverArgs, {
  cwd: project, detached: true, stdio: ["ignore", "pipe", "pipe"], env: nativeEnv,
});
driver.stdout.pipe(driverLog); driver.stderr.pipe(driverLog);
let spawnError;
driver.on("error", (error) => { spawnError = error; });
let session;
const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";
async function wd(method, endpoint, body) {
  const response = await fetch(`http://127.0.0.1:${port}${endpoint}`, {
    method, headers: { "Content-Type": "application/json" },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }), signal: AbortSignal.timeout(60000),
  });
  const data = await response.json();
  if (!response.ok || data.value?.error) throw new Error(JSON.stringify(data.value));
  return data.value;
}
const execute = (script, args = []) => wd("POST", `/session/${session}/execute/sync`, { script, args });
async function native(command, args = {}) {
  const response = await wd("POST", `/session/${session}/execute/async`, {
    script: "const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(value=>done({value}),error=>done({error:String(error)}));",
    args: [command, args],
  });
  if (response.error) throw new Error(response.error);
  return response.value;
}
const api = (method, endpoint, body = null) => native("api", { request: { method, path: endpoint, body } });
const text = () => execute("return document.body.innerText");
const visible = (selector) => execute("const e=document.querySelector(arguments[0]);return !!e && e.getClientRects().length>0", [selector]);
async function element(selector) {
  return (await wd("POST", `/session/${session}/element`, { using: "css selector", value: selector }))[ELEMENT];
}
async function click(selector) {
  await until(`Visible: ${selector}`, () => visible(selector));
  await wd("POST", `/session/${session}/element/${await element(selector)}/click`, {});
}
/** Click the visible, enabled button whose text (or aria-label) is `label`. */
async function clickButton(label, scope = "", timeout = 15000) {
  const xpath = `${scope}//button[(normalize-space(.)='${label}' or @aria-label='${label}') and not(@disabled)]`;
  await until(`Button ready: ${label}`, () => execute(
    "const r=document.evaluate(arguments[0],document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null);for(let i=0;i<r.snapshotLength;i++){if(r.snapshotItem(i).getClientRects().length)return true}return false", [xpath]), timeout);
  const found = await wd("POST", `/session/${session}/elements`, { using: "xpath", value: xpath });
  for (const item of found) {
    const shown = await wd("GET", `/session/${session}/element/${item[ELEMENT]}/displayed`).catch(() => false);
    if (shown) { await wd("POST", `/session/${session}/element/${item[ELEMENT]}/click`, {}); return; }
  }
  throw new Error(`No displayed button ${label}`);
}
async function fill(selector, value) {
  await click(selector);
  await execute("const el=document.querySelector(arguments[0]);const p=el instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(el,'');el.dispatchEvent(new Event('input',{bubbles:true}));", [selector]);
  await wd("POST", `/session/${session}/element/${await element(selector)}/value`, { text: value });
  await until(`Filled ${selector}`, async () => (await execute("return document.querySelector(arguments[0]).value", [selector])) === value);
}
const shots = [];
async function screenshot(name) {
  await delay(400);
  const file = path.join(artifacts, `${name}.png`);
  await writeFile(file, Buffer.from(await wd("GET", `/session/${session}/screenshot`), "base64"));
  shots.push(file);
}
const axeFindings = {};
async function accessibility(name) {
  await delay(300);
  const report = await wd("POST", `/session/${session}/execute/async`, {
    script: `${axeSource}\nconst done=arguments[arguments.length-1];window.axe.run(document,{runOnly:{type:'tag',values:['wcag2a','wcag2aa','wcag21aa']}}).then(r=>done({violations:r.violations.map(v=>({id:v.id,impact:v.impact,help:v.help,nodes:v.nodes.map(n=>({target:n.target,summary:n.failureSummary}))}))}),e=>done({error:String(e)}));`,
    args: [],
  });
  await writeFile(path.join(artifacts, `accessibility-${name}.json`), JSON.stringify(report, null, 2));
  assert.equal(report.error, undefined, `${name}: axe failed`);
  axeFindings[name] = report.violations.map((v) => `${v.impact}:${v.id}`);
  const severe = report.violations.filter((v) => ["serious", "critical"].includes(v.impact));
  assert.deepEqual(severe.map((v) => `${v.id}: ${v.help} ${JSON.stringify(v.nodes.slice(0, 3))}`), [], `${name}: serious/critical accessibility violations`);
}
/** Click a button inside the article (model row) named `name`. */
async function clickInRow(name, label, timeout = 15000) {
  await clickButton(label, `//article[@aria-label='${name}' and contains(@class,'download-model')]`, timeout);
}
/** The download row (not the "Your models" row with the same name). */
const rowText = (name) => execute("const a=[...document.querySelectorAll('article.download-model')].find(a=>a.getAttribute('aria-label')===arguments[0]);return a?a.innerText:''", [name]);
const partial = async (file) => { try { return (await stat(path.join(downloads, `${file}.part`))).size; } catch { return 0; } };

let appPid;
let finalReport;
try {
  await until("WebDriver startup", async () => { if (spawnError) throw spawnError; return wd("GET", "/status"); });
  const created = await wd("POST", "/session", { capabilities: { alwaysMatch: { "tauri:options": { application: binary, args: [] } } } });
  session = created.sessionId;
  await wd("POST", `/session/${session}/timeouts`, { script: 60000, implicit: 0, pageLoad: 30000 });
  await wd("POST", `/session/${session}/window/rect`, { width: 1360, height: 900 });

  // ------------------------------------------------------------ step 1
  await until("Onboarding dialog", () => visible("#onboarding-folder"), 30000);
  appPid = (await api("GET", "/api/version")).pid;
  const catalog = await api("GET", "/api/local-models/downloads");
  const target = catalog.models.find((m) => m.id === modelId);
  assert.ok(target, `${modelId} is in the download list`);
  const recommended = catalog.models.find((m) => m.id === catalog.recommended);
  note(`hardware: ${catalog.hardware.gpu || "no GPU"} (${Math.round((catalog.hardware.vram_bytes || 0) / 2 ** 30)} GB) · ${Math.round(catalog.hardware.ram_bytes / 2 ** 30)} GB RAM → recommended ${recommended?.name || "none"} (${catalog.recommended_fit || "-"})`);
  await fill("#onboarding-folder", project);
  await clickButton("Trust and open");

  // ------------------------------------------------------------ step 2
  await until("Model step", async () => /Choose how ShadowCode thinks/.test(await text()), 30000);
  const step = await text();
  for (const choice of [/Download a free model to run on this computer/, /Use an OpenRouter key/, /Sign in to a subscription/]) assert.match(step, choice);
  if (recommended) {
    assert.match(step, new RegExp(`${recommended.name} by ${recommended.publisher}: [\\d.]+ GB, about`), "Recommended model with size and time");
    assert.match(step, /Picked for this computer: /);
    assert.equal(await execute("const r=document.querySelector('.wizard input[type=radio]:checked');return r?r.closest('label').innerText:''").then((t) => /Download a free model/.test(t)), true, "Download is preselected");
  }
  const picker = await api("GET", "/api/picker?cached=1");
  assert.ok(!picker.targets.some((t) => t.availability === "ready"), "No model is ready on a first run without accounts");
  assert.equal(existsSync(downloads) ? (await readdir(downloads)).length : 0, 0, "Nothing is downloaded before a click");
  await screenshot("first-run-model-step");
  await accessibility("first-run-model-step");
  note(`first run without accounts: model step with three choices${recommended ? `, ${recommended.name} preselected` : ""}; nothing downloaded`);

  // ------------------------------------------------------------ download
  await clickButton("See other free models");
  await until("Settings › Local models", async () => /Download a free model/.test(await text()), 20000);
  const sizeLabel = await until(`${target.name} offers Download`, async () => (await rowText(target.name)).match(/Download \(([^)]+)\)/)?.[1], 20000);
  await screenshot("local-models-before");
  await accessibility("local-models");
  const started = Date.now();
  await clickInRow(target.name, `Download (${sizeLabel})`);
  await until("Progress shown", async () => / of [\d.]+ GB/.test(await rowText(target.name)), 30000);
  await until("64 MB on disk", async () => (await partial(target.file)) >= 64 * 2 ** 20, 120000, 250);
  await clickInRow(target.name, "Pause");
  await until("Paused", async () => /Paused at/.test(await rowText(target.name)), 20000);
  const pausedAt = await partial(target.file);
  await screenshot("download-paused");
  await delay(1500);
  assert.equal(await partial(target.file), pausedAt, "Pause stops writing");
  note(`Pause kept ${(pausedAt / 2 ** 20).toFixed(0)} MB`);
  await clickInRow(target.name, "Resume");
  await until(`${target.name} downloaded`, async () => /Downloaded/.test(await rowText(target.name)), 30 * 60000, 1000);
  const seconds = Math.round((Date.now() - started) / 1000);
  assert.ok(existsSync(path.join(downloads, target.file)) && !existsSync(path.join(downloads, `${target.file}.part`)), "Final file, no part left");
  assert.equal((await stat(path.join(downloads, target.file))).size, target.bytes);
  await screenshot("download-finished");
  note(`${target.name} downloaded, resumed and verified in ${seconds} s`);

  // ------------------------------------------------------------ selected
  const local = await api("GET", "/api/local-models");
  const row = local.models.find((m) => m.source === "download");
  assert.equal(row?.name, target.name, "The download is a local model under its catalog name");
  assert.equal(row.availability, "ready", `Ready: ${row.reason}`);
  await clickButton("Close", "//div[contains(@class,'settings-close')]");
  await until("Selected in the composer", async () => (await execute("return document.querySelector('.unified-picker-trigger')?.getAttribute('aria-label')||''")) === `Model for this task: ${target.name} · This computer`, 30000);
  await screenshot("first-run-selected");
  note(`${target.name} is selected in the composer after the download`);

  // ------------------------------------------------------------ task
  const composer = 'textarea[aria-label="Message ShadowCode"]';
  await fill(composer, "List the files in this project with a tool, then tell me how many there are.");
  await click('button[aria-label="Send task"]');
  const job = await until("Task finished", async () => {
    const jobs = (await api("GET", "/api/jobs")).jobs || [];
    const done = jobs.find((j) => ["completed", "failed", "cancelled"].includes(j.status));
    if (done) return done;
    // Read-only tools need no approval; anything else is declined.
    for (const card of await execute("return [...document.querySelectorAll('.approval')].map(a=>a.dataset.approvalId)"))
      await clickButton("Deny", `//div[@data-approval-id='${card}']`).catch(() => undefined);
    return false;
  }, 8 * 60000, 1000);
  assert.equal(job.status, "completed", `Task ${job.status}: ${job.error || ""}`);
  const events = (await api("GET", `/api/sessions/${job.session_id}/events?limit=500`)).events || [];
  const tools = events.filter((e) => e.type === "tool.completed").map((e) => e.payload?.tool);
  assert.ok(tools.some((t) => ["list_files", "read_file", "search_code", "exec"].includes(t)), `A tool ran (${tools})`);
  const loaded = (await api("GET", "/api/local-models")).loaded;
  assert.equal(loaded?.name, target.name);
  await until("Answer shown", async () => /\b(2|two)\b/i.test(await text()), 10000).catch(() => undefined);
  await screenshot("first-run-task");
  note(`a task on ${target.name} (${loaded.backend || "?"}) used ${[...new Set(tools)].join(", ")} and completed`);

  // ------------------------------------------------------------ delete
  if (await visible('button[aria-label="Show sidebar"]')) await click('button[aria-label="Show sidebar"]');
  const settingsButton = "//aside//button[.//span[normalize-space(.)='Settings']]";
  await until("Sidebar Settings", () => execute("return !!document.evaluate(arguments[0],document,null,XPathResult.FIRST_ORDERED_NODE_TYPE,null).singleNodeValue", [settingsButton]));
  const found = await wd("POST", `/session/${session}/element`, { using: "xpath", value: settingsButton });
  await wd("POST", `/session/${session}/element/${found[ELEMENT]}/click`, {});
  await until("Settings open", () => visible(".settings-nav"), 10000);
  await clickButton("Local models", "//nav[@aria-label='Settings sections']");
  await until("Model row", async () => /Downloaded/.test(await rowText(target.name)), 15000);
  await clickButton("Delete", `//article[@aria-label='${target.name}' and contains(@class,'download-model')]`);
  await clickButton("Delete", `//article[@aria-label='${target.name}' and contains(@class,'download-model')]`);
  await until("File deleted", async () => !existsSync(path.join(downloads, target.file)), 30000);
  assert.equal((await api("GET", "/api/local-models")).models.length, 0, "No local model left");
  note("Delete unloaded the model and removed the file");

  // ------------------------------------------------------------ quit
  const children = await descendants(appPid);
  await execute("setTimeout(()=>window.__TAURI_INTERNALS__.invoke('desktop_quit'),30);return true;");
  await until("App exited", () => dead(appPid), 30000);
  for (const pid of children) await until(`Child ${pid} exited`, () => dead(pid), 15000);
  note(`quit: app and ${children.length} child processes exited`);
  await wd("DELETE", `/session/${session}`).catch(() => {}); session = undefined;
  finalReport = { passed: true, model: target.id, recommended: catalog.recommended, recommended_fit: catalog.recommended_fit, hardware: catalog.hardware, download_seconds: seconds, checks, axe: axeFindings, screenshots: shots };
} catch (error) {
  finalReport = { passed: false, checks, failure: error.stack || String(error) };
  if (session) {
    await writeFile(path.join(artifacts, "failure.png"), Buffer.from(await wd("GET", `/session/${session}/screenshot`), "base64")).catch(() => {});
    await text().then((body) => writeFile(path.join(artifacts, "failure.txt"), body)).catch(() => {});
  }
  process.exitCode = 1;
  console.error(error);
} finally {
  if (session) await wd("DELETE", `/session/${session}`).catch(() => {});
  if (appPid && !(await dead(appPid))) {
    const stray = await descendants(appPid);
    for (const pid of [appPid, ...stray]) try { process.kill(pid, "SIGKILL"); } catch { /* exited */ }
  }
  try { process.kill(-driver.pid, "SIGTERM"); } catch { /* exited */ }
  await delay(300);
  try { process.kill(-driver.pid, "SIGKILL"); } catch { /* exited */ }
  const busCleanup = await privateSession.cleanup();
  await writePrivateSessionReport(privateSession, busCleanup);
  finalReport.private_session_cleanup = busCleanup;
  if (!busCleanup.ok) { finalReport.passed = false; process.exitCode = 1; }
  await writeFile(path.join(artifacts, "result.json"), JSON.stringify(finalReport, null, 2) + "\n");
  console.log(`First-run window test ${finalReport.passed ? "passed" : "failed"} (${checks.length} checks). Screenshots in ${artifacts}`);
  driverLog.end();
  if (!process.env.SHADOW_KEEP_SCRATCH) await rm(scratch, { recursive: true, force: true });
}
