// Real Tauri/WebKit window test for the current ShadowCode desktop app.
//
// Drives the actual `shadowcode` binary through tauri-driver + WebKitWebDriver:
// first-run onboarding, the unified model picker (real vendor CLI rows when
// they are installed, read-only probes done by the app itself), a local GGUF
// row served by a test-double llama-server (scripts/fake-llama-server.py, no
// GPU, no weights), approvals, the activity timeline, the summary card, the
// Changes drawer and its Git/Tasks/Tools panels, native file editing, reload persistence,
// a local model that runs out of memory (plain failure, smaller-context retry),
// reconnecting through dropped/duplicated/stale events and a mid-stream reload
// with a second conversation queued, every Settings page, the cloud consent dialog
// (always cancelled: no vendor turn ever runs), Stop, light/dark/compact
// layouts with axe checks, and process cleanup after quit.
//
// Requirements: a display (xvfb-run), DBus, tauri-driver, WebKitWebDriver.
//   node scripts/run-native-x11.mjs node scripts/test-native-desktop.mjs
// Environment:
//   SHADOW_DESKTOP_BINARY   binary to drive (default target/debug/shadowcode)
//   SHADOW_DESKTOP_ARGS     JSON array of extra arguments (e.g. AppImage flags)
//   SHADOW_TAURI_DRIVER     tauri-driver path (default: tauri-driver on PATH)
//   SHADOW_WEBKIT_DRIVER    WebKitWebDriver path (passed as --native-driver)
//   SHADOW_NATIVE_ARTIFACTS screenshots/reports directory (default artifacts/native)
//   SHADOW_NATIVE_ENDURANCE=1 adds the required dedicated UI-03 100-task phase
//   SHADOW_NATIVE_DISPLAY=wayland explicitly exercises the current desktop;
//     run directly under dbus-run-session, never through run-native-x11.mjs.
//     scripts/run-native-wayland.mjs sets it for a private Weston compositor.
//   SHADOW_EXPECT_VENDORS   e.g. "codex=Ready,claude=Sign in": exact picker
//                           availability expected for vendors on this machine
import assert from "node:assert/strict";
import { capturePrivateSession, writePrivateSessionReport } from "./native-test-session.mjs";
import { EDITOR_FILE, OTHER_FILE, seedNativeEditorFixtures, nativeEditorBeforeReload, nativeEditorAfterReload } from "./fixtures/native-editor-smoke.mjs";
import { nativeRunCheck } from "./fixtures/native-run-check-smoke.mjs";
import { nativeEndurance } from "./fixtures/native-endurance-smoke.mjs";
import { nativeLocalMemory, nativeReconnect } from "./fixtures/native-recovery-smoke.mjs";
import { createServer } from "node:http";
import { createHash } from "node:crypto";
import { spawn, execFile } from "node:child_process";
import { promisify } from "node:util";
import { createWriteStream, existsSync } from "node:fs";
import { chmod, copyFile, mkdtemp, mkdir, readFile, readdir, readlink, rm, stat, symlink, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const displayBackend = process.env.SHADOW_NATIVE_DISPLAY || "x11";
assert.ok(["x11", "wayland"].includes(displayBackend), "Unknown native display backend");
const displayEvidence = { backend: displayBackend, geometryFailures: [] };
if (displayBackend === "wayland") {
  assert.equal(process.env.XDG_SESSION_TYPE, "wayland", "Wayland qualification requires an actual Wayland session");
  assert.ok(process.env.WAYLAND_DISPLAY && process.env.XDG_RUNTIME_DIR, "Wayland display and runtime directory required");
  const socket = path.resolve(process.env.XDG_RUNTIME_DIR, process.env.WAYLAND_DISPLAY);
  const identity = await stat(socket);
  assert.ok(identity.isSocket(), "Wayland display must identify a live compositor socket");
  assert.equal(identity.uid, process.getuid(), "Wayland socket must belong to this user");
  Object.assign(displayEvidence, { socket, socketDevice: identity.dev, socketInode: identity.ino,
    desktop: process.env.XDG_CURRENT_DESKTOP || null, session: process.env.XDG_SESSION_ID || null });
}
const privateSession = await capturePrivateSession();
let finalReport;
const root = fileURLToPath(new URL("../", import.meta.url));
const binary = process.env.SHADOW_DESKTOP_BINARY || path.join(root, "target/debug/shadowcode");
const binaryArgs = JSON.parse(process.env.SHADOW_DESKTOP_ARGS || "[]");
assert.ok(Array.isArray(binaryArgs) && binaryArgs.every((arg) => typeof arg === "string"), "SHADOW_DESKTOP_ARGS must be a JSON array of strings");
const artifacts = process.env.SHADOW_NATIVE_ARTIFACTS || path.join(root, "artifacts/native");
await rm(artifacts, { recursive: true, force: true });
await mkdir(artifacts, { recursive: true });
const axeSource = await readFile(path.join(root, "ui/node_modules/axe-core/axe.min.js"), "utf8");
const run = promisify(execFile);
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const WIDE = { width: 1360, height: 860 };
const checks = [];
const note = (text) => { checks.push(text); console.log(`  ok  ${text}`); };
const WINDOW_RULE = "Window test profile rule: WINDOW-RULE-7.";

// ---------------------------------------------------------------- fixtures
const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-window-"));
const project = path.join(scratch, "project");
const profile = path.join(scratch, "profile");
const runtimeDir = path.join(scratch, "runtime");
const modelsDir = path.join(scratch, "models");
const configDirectory = path.join(profile, "config/shadow-agent");
// The app canonicalizes each XDG root before opening the native backend, so
// create all isolated roots up front (not just the config directory).
for (const dir of [project, configDirectory, path.join(profile, "data"), path.join(profile, "state"), path.join(profile, "cache"), runtimeDir, modelsDir, path.join(scratch, "tmp")]) await mkdir(dir, { recursive: true });

// Disposable git project.
await writeFile(path.join(project, "README.md"), "# Window test\nA disposable workspace.\n");
await seedNativeEditorFixtures(project);
for (const args of [["init", "-q"], ["add", "README.md", EDITOR_FILE, OTHER_FILE], ["commit", "-qm", "Window fixture base"]])
  await run("git", ["-c", "core.hooksPath=/dev/null", "-c", "user.name=Window Test", "-c", "user.email=test@example.invalid", "-c", "commit.gpgsign=false", ...args], { cwd: project });

// Test-double llama-server runtime: binary + COMMIT + architectures.txt, the
// layout ShadowCode's managed runtime directory has.
const fakeServer = path.join(runtimeDir, "llama-server");
await copyFile(path.join(root, "scripts/fake-llama-server.py"), fakeServer);
await chmod(fakeServer, 0o755);
await writeFile(path.join(runtimeDir, "architectures.txt"), "qwen3\nllama\ngemma4\n");
await writeFile(path.join(runtimeDir, "COMMIT"), "commit=testdouble\nbackend=cpu\nbuilt=2026-09-23T00:00:00Z\n");

// A tiny synthetic GGUF (header + pretend weights), as gguf::test_support
// writes it: a qwen3 model whose chat template supports tools.
function gguf(kv, tensors, padding = 4096) {
  const parts = [];
  const u32 = (v) => { const b = Buffer.alloc(4); b.writeUInt32LE(v); parts.push(b); };
  const u64 = (v) => { const b = Buffer.alloc(8); b.writeBigUInt64LE(BigInt(v)); parts.push(b); };
  const str = (s) => { const b = Buffer.from(s, "utf8"); u64(b.length); parts.push(b); };
  parts.push(Buffer.from("GGUF")); u32(3); u64(tensors.length); u64(kv.length);
  for (const [key, value] of kv) {
    str(key);
    if (typeof value === "number") { u32(4); u32(value); } else { u32(8); str(value); }
  }
  for (const name of tensors) { str(name); u32(2); u64(4); u64(4); u32(0); u64(0); }
  parts.push(Buffer.alloc(padding));
  return Buffer.concat(parts);
}
const modelFile = path.join(modelsDir, "coder-test-1b.gguf");
await writeFile(modelFile, gguf([
  ["general.architecture", "qwen3"], ["general.name", "Coder Test 1B"],
  ["qwen3.context_length", 32768], ["qwen3.embedding_length", 1024], ["qwen3.block_count", 8],
  ["qwen3.attention.head_count", 16], ["qwen3.attention.head_count_kv", 4],
  ["tokenizer.chat_template", "{% if tools %}<tool_call>{% endif %}{% if enable_thinking %}{% endif %}"],
], ["token_embd.weight", "output.weight"]));

// Isolated profile through XDG roots. HOME stays real so the vendor CLIs can
// be probed read-only; the local runtime is pinned to the test double through
// local_engine.llama_binary (it outranks ~/.local/lib/shadowcode) and
// SHADOWCODE_LLAMA_SERVER. No onboarding record: this is a first run.
await writeFile(path.join(configDirectory, "config.yaml"), JSON.stringify({
  local_engine: { llama_binary: fakeServer },
  ui: { notify: false },
}));
// cursor-agent keeps its sign-in under $XDG_CONFIG_HOME/cursor. Link that one
// directory (never copied or written by the test) so the app's own read-only
// probe sees the real account, as it would outside the isolated profile.
const realConfig = process.env.XDG_CONFIG_HOME || path.join(homedir(), ".config");
for (const dir of ["cursor"])
  if (existsSync(path.join(realConfig, dir))) await symlink(path.join(realConfig, dir), path.join(profile, "config", dir));
const nativeEnv = {
  ...process.env,
  XDG_CONFIG_HOME: path.join(profile, "config"),
  XDG_DATA_HOME: path.join(profile, "data"),
  XDG_STATE_HOME: path.join(profile, "state"),
  XDG_CACHE_HOME: path.join(profile, "cache"),
  TMPDIR: path.join(scratch, "tmp"),
  SHADOWCODE_LLAMA_SERVER: fakeServer,
};
delete nativeEnv.NO_CLEANUP;
// Stay on the xvfb display: GTK would otherwise prefer a Wayland session the
// test was started from. Real-desktop qualification requires an explicit opt-in.
delete nativeEnv.WAYLAND_SOCKET;
nativeEnv.GDK_BACKEND = displayBackend;
if (displayBackend === "x11") {
  delete nativeEnv.WAYLAND_DISPLAY;
  nativeEnv.WEBKIT_DISABLE_DMABUF_RENDERER = "1";
} else {
  delete nativeEnv.DISPLAY; // No XWayland fallback can satisfy this run.
  displayEvidence.dmabufOverride = nativeEnv.WEBKIT_DISABLE_DMABUF_RENDERER || null;
}
const launches = () => readFile(path.join(runtimeDir, "launches.jsonl"), "utf8").then((t) => t.trim().split("\n").filter(Boolean).map(JSON.parse), () => []);
const modelRequests = () => readFile(path.join(runtimeDir, "requests.jsonl"), "utf8").then((t) => t.trim().split("\n").filter(Boolean).map(JSON.parse), () => []);

// Vendors on this machine (for rows the picker must show).
const VENDOR_BINARIES = { codex: "codex", claude: "claude", cursor: "cursor-agent", antigravity: "agy", grok: "grok" };
const installed = Object.entries(VENDOR_BINARIES)
  .filter(([, bin]) => (process.env.PATH || "").split(":").some((dir) => dir && existsSync(path.join(dir, bin))))
  .map(([vendor]) => vendor);
const expectedVendors = Object.fromEntries((process.env.SHADOW_EXPECT_VENDORS || "").split(",").filter(Boolean).map((pair) => pair.split("=").map((s) => s.trim())));

// ---------------------------------------------------------------- helpers
async function until(label, fn, timeout = 15000) {
  const end = Date.now() + timeout;
  let last;
  while (Date.now() < end) {
    try { const value = await fn(); if (value) return value; } catch (error) { last = error; }
    await delay(100);
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
/** Every live descendant of `pid` (walks /proc ppid links). */
async function descendants(pid) {
  const parents = new Map();
  for (const name of await readdir("/proc")) {
    if (!/^\d+$/.test(name)) continue;
    try {
      const stat = await readFile(`/proc/${name}/stat`, "utf8");
      const rest = stat.slice(stat.lastIndexOf(")") + 2).split(" ");
      if (rest[0] === "Z") continue;
      parents.set(Number(name), Number(rest[1]));
    } catch { /* exited */ }
  }
  const out = [];
  const walk = (p) => { for (const [child, parent] of parents) if (parent === p) { out.push(child); walk(child); } };
  walk(pid);
  return out;
}
async function commandLine(pid) {
  try { return (await readFile(`/proc/${pid}/cmdline`, "utf8")).split("\0").join(" ").trim(); } catch { return "(exited)"; }
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
async function composerControlsFit(label) {
  const problems = await execute(`
    const panel = document.querySelector('form.composer').getBoundingClientRect();
    const controls = [...document.querySelectorAll('.composer-footer button, .composer-footer select')]
      .filter(el => el.getClientRects().length && getComputedStyle(el).visibility !== 'hidden')
      .map(el => ({el, name: el.getAttribute('aria-label') || el.textContent.trim(), box: el.getBoundingClientRect()}));
    const problems = controls.length < 4 ? ['missing composer controls'] : [];
    for (const [index, control] of controls.entries()) {
      const r = control.box;
      if (r.left < panel.left - 1 || r.right > panel.right + 1 || r.top < panel.top - 1 || r.bottom > panel.bottom + 1)
        problems.push('outside composer: ' + control.name);
      // Nothing invisible (such as a closed menu's content) sits over it.
      const hit = document.elementFromPoint(r.left + r.width / 2, r.top + r.height / 2);
      if (!hit || !(control.el === hit || control.el.contains(hit)))
        problems.push('covered: ' + control.name);
      for (const other of controls.slice(index + 1)) {
        const b = other.box;
        if (Math.min(r.right, b.right) - Math.max(r.left, b.left) > 1 && Math.min(r.bottom, b.bottom) - Math.max(r.top, b.top) > 1)
          problems.push('overlap: ' + control.name + ' / ' + other.name);
      }
    }
    return problems;
  `);
  assert.deepEqual(problems, [], label);
}
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
async function clickButton(label, scope = "") {
  const xpath = `${scope}//button[(normalize-space(.)='${label}' or @aria-label='${label}') and not(@disabled)]`;
  await until(`Button ready: ${label}`, () => execute(
    "const r=document.evaluate(arguments[0],document,null,XPathResult.ORDERED_NODE_SNAPSHOT_TYPE,null);for(let i=0;i<r.snapshotLength;i++){if(r.snapshotItem(i).getClientRects().length)return true}return false", [xpath]));
  const found = await wd("POST", `/session/${session}/elements`, { using: "xpath", value: xpath });
  for (const item of found) {
    const shown = await wd("GET", `/session/${session}/element/${item[ELEMENT]}/displayed`).catch(() => false);
    if (shown) { await wd("POST", `/session/${session}/element/${item[ELEMENT]}/click`, {}); return; }
  }
  throw new Error(`No displayed button ${label}`);
}
async function type(selector, value) {
  await wd("POST", `/session/${session}/element/${await element(selector)}/value`, { text: value });
}
async function fill(selector, value) {
  await click(selector);
  // WebKit's element-clear may skip the input event React needs.
  await execute("const el=document.querySelector(arguments[0]);const p=el instanceof HTMLTextAreaElement?HTMLTextAreaElement.prototype:HTMLInputElement.prototype;Object.getOwnPropertyDescriptor(p,'value').set.call(el,'');el.dispatchEvent(new Event('input',{bubbles:true}));", [selector]);
  if (value) await type(selector, value);
  await until(`Filled ${selector}`, async () => (await execute("return document.querySelector(arguments[0]).value", [selector])) === value);
}
async function setWindow({ width, height }) {
  await wd("POST", `/session/${session}/window/rect`, { width, height });
  try {
    await until(`Window ${width}px (inner ${await execute("return window.innerWidth+'x'+window.innerHeight")})`, async () => Math.abs((await execute("return window.innerWidth")) - width) < 40, 8000);
  } catch (error) {
    if (displayBackend !== "wayland" || !/^Window .* timed out$/.test(error.message)) throw error;
    const actual = await execute("return {width:innerWidth,height:innerHeight}");
    displayEvidence.geometryFailures.push({ requested: { width, height }, actual });
    console.log(`  FAIL  Wayland resize ${width}x${height}: actual ${actual.width}x${actual.height}; continuing other checks with overall failure retained`);
    return false;
  }
  await delay(300);
  return true;
}
/** Brightness of the page background (0 black … 1 white), as rendered. */
const pageLuminance = () => execute("const [r,g,b]=getComputedStyle(document.body).backgroundColor.match(/[\\d.]+/g).map(Number);return (0.2126*r+0.7152*g+0.0722*b)/255");
/** The page really shows `theme` (its computed background), not just the
 * attribute: WebKit on a dark desktop reports prefers-color-scheme: dark. */
async function expectTheme(theme, where) {
  await until(`${where}: ${theme} theme applied`, async () => {
    const luminance = await pageLuminance();
    return theme === "dark" ? luminance < 0.25 : luminance > 0.75;
  }, 5000);
}
/** Choose the appearance as Settings does (saved in the config, so config
 * re-reads and reloads keep it), apply it now, and check it took. */
async function setTheme(theme) {
  await api("PUT", "/api/config", { values: { ui: { theme } } });
  await execute("try{localStorage.setItem('shadow:theme',arguments[0])}catch(e){}document.documentElement.dataset.theme=arguments[0]", [theme]);
  await expectTheme(theme, `setTheme(${theme})`);
  await delay(250);
}
async function settle() {
  // Finite transitions only: spinners loop forever.
  await execute("return Promise.race([Promise.all(document.getAnimations().filter(a=>a.effect?.getTiming().iterations!==Infinity).map(a=>a.finished.catch(()=>{}))),new Promise(r=>setTimeout(r,2000))]).then(()=>true)").catch(() => undefined);
  await delay(150);
}
const shots = [];
// Screenshots used in the docs show the window, not a passing notification.
const DOC_SHOTS = new Set(["picker", "composer-more-ready", "task-complete", "workspace-light", "accounts", "settings-advanced-guardian"]);
// They also never show the tester's own account: e-mail addresses from the
// real vendor CLIs are replaced for the capture.
const MASK_EMAILS = "const w=document.createTreeWalker(document.body,NodeFilter.SHOW_TEXT);const re=/[\\w.+-]+@[\\w-]+(\\.[\\w-]+)+/g;for(let n=w.nextNode();n;n=w.nextNode()){if(re.test(n.nodeValue)){re.lastIndex=0;n.nodeValue=n.nodeValue.replace(re,'you@example.com')}}";
async function screenshot(name) {
  if (DOC_SHOTS.has(name)) {
    await execute("document.querySelectorAll('.toast button[aria-label=\"Dismiss notification\"]').forEach(b=>b.click())");
    await execute(MASK_EMAILS);
  }
  await settle();
  // Screenshots are light unless named "-dark".
  await expectTheme(/-dark$/.test(name) ? "dark" : "light", `Screenshot ${name}`);
  const file = path.join(artifacts, `${name}.png`);
  await writeFile(file, Buffer.from(await wd("GET", `/session/${session}/screenshot`), "base64"));
  shots.push(file);
}
const axeFindings = {};
/** axe (WCAG 2 A/AA): no serious or critical violations. */
async function accessibility(name) {
  await settle();
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
function horizontalOverflow() {
  return execute("return document.documentElement.scrollWidth>window.innerWidth+1 || document.body.scrollWidth>window.innerWidth+1");
}
const composer = 'textarea[aria-label="Message ShadowCode"]';
async function openPicker() {
  if (!(await visible(".unified-picker-menu"))) await click(".unified-picker-trigger");
  await until("Picker open", () => visible(".unified-picker-menu"));
}
/** Visible picker rows: [{name, meta, group, blocked}]. */
const pickerRows = () => execute(`return [...document.querySelectorAll('.unified-picker-group')].flatMap(g=>{const group=g.querySelector('.unified-picker-heading').textContent;return [...g.querySelectorAll('.unified-picker-row')].map(r=>({group,id:r.id,name:r.querySelector('strong').textContent,meta:r.querySelector('small').textContent,blocked:r.classList.contains('is-blocked'),selected:r.getAttribute('aria-selected')==='true'}))})`);
async function pickRow(name) {
  await openPicker();
  await until(`Picker row ${name}`, () => execute("const r=[...document.querySelectorAll('.unified-picker-row')].find(r=>r.querySelector('strong').textContent===arguments[0]);if(!r)return false;r.scrollIntoView({block:'nearest'});return true", [name]), 15000);
  const id = await execute("return [...document.querySelectorAll('.unified-picker-row')].find(r=>r.querySelector('strong').textContent===arguments[0]).id", [name]);
  await click(`#${id}`);
  await until(`Picker shows ${name}`, async () => (await execute("return document.querySelector('.unified-picker-trigger').getAttribute('aria-label')")) === `Model for this task: ${name}`);
}
async function openSettings(section) {
  if (await visible('button[aria-label="Show sidebar"]')) await click('button[aria-label="Show sidebar"]');
  const settingsButton = "//aside//button[.//span[normalize-space(.)='Settings']]";
  await until("Sidebar Settings", () => execute("return !!document.evaluate(arguments[0],document,null,XPathResult.FIRST_ORDERED_NODE_TYPE,null).singleNodeValue", [settingsButton]));
  const found = await wd("POST", `/session/${session}/element`, { using: "xpath", value: settingsButton });
  await wd("POST", `/session/${session}/element/${found[ELEMENT]}/click`, {});
  await until("Settings open", () => visible(".settings-nav"));
  if (section) await clickButton(section, "//nav[@aria-label='Settings sections']");
}
async function closeSettings() {
  await clickButton("Close", "//div[contains(@class,'settings-close')]");
  await until("Settings closed", async () => !(await visible(".settings-nav")));
}
async function send(message) {
  await fill(composer, message);
  await click('button[aria-label="Send task"]');
}
async function approve(match) {
  await until(`Approval for ${match}`, () => execute("return [...document.querySelectorAll('.approval')].some(a=>a.textContent.includes(arguments[0]))", [match]), 30000);
  await execute("[...document.querySelectorAll('.approval')].find(a=>a.textContent.includes(arguments[0])).scrollIntoView({block:'center'})", [match]);
  await screenshot(`approval-${match.replace(/[^a-z0-9]+/gi, "-")}`);
  const id = await execute("return [...document.querySelectorAll('.approval')].find(a=>a.textContent.includes(arguments[0])).dataset.approvalId", [match]);
  await clickButton("Allow", `//div[@data-approval-id='${id}']`);
  await until(`Approval ${match} resolved`, async () => !(await execute("return !!document.querySelector(`[data-approval-id='${arguments[0]}']`)", [id])), 15000);
}
/** This conversation's jobs, newest first. */
async function jobsFor(sessionId) {
  const jobs = (await api("GET", "/api/jobs")).jobs.filter((j) => j.session_id === sessionId);
  return jobs.sort((a, b) => String(b.created_at || "").localeCompare(String(a.created_at || "")));
}

let appPid;
try {
  await until("WebDriver startup", async () => { if (spawnError) throw spawnError; return wd("GET", "/status"); });
  const created = await wd("POST", "/session", { capabilities: { alwaysMatch: { "tauri:options": { application: binary, args: binaryArgs } } } });
  session = created.sessionId;
  await wd("POST", `/session/${session}/timeouts`, { script: 60000, implicit: 0, pageLoad: 30000 });
  if (displayBackend === "wayland") {
    const screen = await execute("return {width:screen.availWidth,height:screen.availHeight,scale:devicePixelRatio}");
    assert.ok(screen.width > 0 && screen.height > 0, "Physical display dimensions available");
    displayEvidence.screen = screen;
    WIDE.width = Math.min(WIDE.width, screen.width);
    WIDE.height = Math.min(WIDE.height, screen.height);
  }
  await setWindow(WIDE);

  // ------------------------------------------------------------ first run
  await until("Onboarding dialog", () => visible("#onboarding-folder"), 30000);
  const version = await api("GET", "/api/version");
  appPid = version.pid;
  assert.equal(version.runtime, "rust"); assert.equal(version.transport, "native");
  assert.equal(path.basename(await readlink(`/proc/${appPid}/exe`)), "shadowcode");
  if (displayBackend === "wayland") {
    const environment = (await readFile(`/proc/${appPid}/environ`, "utf8")).split("\0");
    assert.ok(environment.includes("GDK_BACKEND=wayland"), "The actual app preserves native Wayland selection");
    assert.ok(!environment.some(item => item.startsWith("DISPLAY=")), "Actual app has no X11 fallback");
    displayEvidence.executableSha256 = createHash("sha256").update(await readFile(`/proc/${appPid}/exe`)).digest("hex");
  }
  const expectedScript = (await readFile(path.join(root, "ui/dist/index.html"), "utf8")).match(/src="([^"]+\.js)"/)[1];
  assert.equal(await execute("return new URL(document.querySelector('script[type=module]').src).pathname"), expectedScript, "The binary embeds the current ui/dist build");
  const onboarding = await text();
  assert.match(onboarding, /Welcome to ShadowCode/);
  assert.match(onboarding, /Ask before actions/); assert.match(onboarding, /Allow project edits/);
  assert.doesNotMatch(onboarding, /\b(Provider|provider|Endpoint|endpoint|API key|Choose a model|Next)\b/, "Onboarding has no model step");
  assert.deepEqual(await execute("return [...document.querySelectorAll('.wizard input, .wizard select, .wizard textarea')].map(e=>e.type||e.tagName)"), ["text", "radio", "radio"], "Onboarding asks only for the folder and the permission mode");
  assert.equal(await execute("return document.querySelector('input[name=onboarding-mode]:checked').closest('label').textContent.includes('Ask before actions')"), true, "Ask before actions is the default");
  await fill("#onboarding-folder", project);
  await setTheme("light");
  await screenshot("onboarding");
  await accessibility("onboarding");
  await clickButton("Trust and open");
  // Without any ready model (no signed-in subscription on this machine), the
  // first run asks how to get one; this run skips it (test-native-first-run.mjs
  // covers that step).
  const modelStep = await until("Onboarding closes or asks for a model", () => execute("if(!document.querySelector('.wizard'))return 'closed';return /Choose how ShadowCode thinks/.test(document.body.innerText)?'model':false"), 20000);
  if (modelStep === "model") {
    const choices = await text();
    for (const choice of [/Download a free model to run on this computer/, /Use an OpenRouter key/, /Sign in to a subscription/]) assert.match(choices, choice);
    await clickButton("Skip for now");
    note("first run without a ready model offers download, OpenRouter and subscription; skipped here");
  }
  await until("Workspace ready", () => execute("const t=document.querySelector(arguments[0]);return !!t && !t.disabled && !document.querySelector('.wizard')", [composer]), 20000);
  const status = await api("GET", "/api/workspace/status");
  assert.equal(status.workspace, project);
  const config = await api("GET", "/api/config");
  const cfgValues = config.values || config.config || config;
  assert.equal(cfgValues.permissions?.mode, "ask", "Onboarding saved Ask before actions");
  note("first run: folder, trust and permission mode (Ask before actions), no model step");
  // Onboarding saves "system"; the screenshots below choose explicitly.
  await setTheme("light");

  // Quiet welcome.
  const readyAt = Date.now();
  const suggestions = await until("Welcome suggestions", async () => {
    const found = await execute("return [...document.querySelectorAll('.welcome-suggestions button')].map(b=>b.textContent)");
    return found.length > 0 && found;
  });
  assert.ok(suggestions.length <= 3, `At most three suggestions (${suggestions})`);
  assert.equal(await execute("return document.querySelectorAll('.unified-picker-trigger').length"), 1, "Exactly one model picker");
  assert.equal(await execute("return document.querySelectorAll('select').length"), 0, "No select-based model controls in the workspace");
  assert.ok(await visible('button[aria-label^="Attach"]'), "Attachment button");
  await screenshot("welcome-light");
  await accessibility("welcome-light");
  note(`quiet welcome with ${suggestions.length} suggestions, one picker, attachment button`);

  // Infrequent options stay one click away without crowding the composer.
  assert.equal(await execute("return document.querySelector('.composer-more')?.open"), false);
  await click("details.composer-more > summary");
  await until("More task options open", () => execute("return document.querySelector('.composer-more')?.open"));
  const moreOptions = await execute("return document.querySelector('.composer-more-menu').innerText");
  assert.match(moreOptions, /Compare/);
  assert.match(moreOptions, /Worktree/);
  await screenshot("composer-more");
  await accessibility("composer-more");
  await click("details.composer-more > summary");
  await until("More task options close", () => execute("return !document.querySelector('.composer-more')?.open"));
  note("More keeps Compare and Worktree discoverable, closes by clicking its trigger");

  // ------------------------------------------------------------ picker
  await until("Picker loaded", async () => !/Loading models/.test(await execute("return document.querySelector('.unified-picker-current').textContent")), 60000);
  const pickerLoadMs = Date.now() - readyAt;
  console.log(`  ..  picker rows usable ${pickerLoadMs} ms after the workspace opened`);
  await until("Welcome model action", () => visible(".welcome-model-setup button"));
  await screenshot("welcome-model-ready");
  // With a ready model (a signed-in vendor CLI on this machine) the welcome
  // offers "Choose a model"; with none (a clean CI host) it offers a free
  // download, an OpenRouter key or a subscription instead.
  const canChoose = await execute("return [...document.querySelectorAll('.welcome-model-setup button')].some(b => b.textContent.trim() === 'Choose a model' && b.getClientRects().length > 0)");
  if (canChoose) {
    await clickButton("Choose a model", "//div[contains(@class,'welcome-model-setup')]");
    await until("Welcome action opens picker", () => visible(".unified-picker-menu"));
    note("first-run Choose a model opens the unified picker after model discovery");
  } else {
    const setup = await execute("return document.querySelector('.welcome-model-setup').innerText");
    assert.match(setup, /free model|Download/i, "No-model welcome offers a free model");
    assert.match(setup, /OpenRouter/i, "No-model welcome offers an OpenRouter key");
    assert.match(setup, /subscription/i, "No-model welcome offers a subscription");
    note("no model ready: the welcome offers a free model, an OpenRouter key or a subscription");
  }
  await until("Vendor rows probed", async () => {
    const data = await api("GET", "/api/picker");
    const vendorsChecked = installed.every((vendor) => data.targets.some((t) => t.provider === `cli:${vendor}` && t.reason !== "Not checked yet"));
    return vendorsChecked && data;
  }, 90000);
  await openPicker();
  await until("Picker rows rendered", async () => (await pickerRows()).length > 0 || installed.length === 0);
  assert.match(await execute("return document.querySelector('.unified-picker-actions button').textContent"), /Refresh models/);
  await click(".unified-picker-actions button");
  await until("Manual model refresh completed", () => execute("return [...document.querySelectorAll('.unified-picker-actions button')].some(b=>b.textContent.includes('Refresh models') && !b.disabled)"), 90000);
  note("open picker manually refreshes provider rows and remains open");
  // Read the settled manual refresh without starting another provider probe.
  const picker = await api("GET", "/api/picker?cached=1");
  const vendorTargets = picker.targets.filter((target) => target.inference !== "local" && target.group !== "local" && target.group !== "api" && target.provider !== "openrouter");
  const vendorHeading = vendorTargets.some((target) => target.billing === "unknown" || target.billing === "api_key") ? "Vendor CLIs" : "Subscriptions";
  const rows = await pickerRows();
  const groups = await execute("return [...document.querySelectorAll('.unified-picker-heading')].map(h=>h.textContent)");
  assert.deepEqual(groups, [vendorHeading, "On this computer", "API keys"], "Vendor heading reflects observed billing, not CLI installation or availability");
  assert.match(await execute("return [...document.querySelectorAll('.unified-picker-group')].find(g=>g.querySelector('.unified-picker-heading').textContent==='On this computer').textContent"), /No local models added yet/);
  // No OpenRouter key in this profile: API-key rows (if the list loaded) ask for one; none is Ready.
  const apiText = await execute("return [...document.querySelectorAll('.unified-picker-group')].find(g=>g.querySelector('.unified-picker-heading').textContent==='API keys').textContent");
  assert.match(apiText, /Billed per token/);
  assert.doesNotMatch(apiText, /· Ready ·/, "No API-key row is Ready without a key");
  assert.match(apiText, /Add an OpenRouter API key|Add API key|Show all/);
  const vendorSummary = {};
  for (const vendor of installed) {
    const backend = picker.targets.filter((t) => t.provider === `cli:${vendor}`);
    assert.ok(backend.length > 0, `${vendor}: backend has picker rows`);
    const shown = rows.filter((r) => backend.some((t) => t.name === r.name));
    assert.ok(shown.length > 0, `${vendor}: picker shows its rows`);
    for (const row of shown) {
      const target = backend.find((t) => t.name === row.name);
      assert.ok(row.meta.includes(target.availability_label), `${row.name}: shows ${target.availability_label} (${row.meta})`);
      assert.equal(row.blocked, target.availability !== "ready");
      assert.equal(row.group, vendorHeading);
    }
    vendorSummary[vendor] = [...new Set(backend.map((t) => t.availability_label))].join("/");
    if (expectedVendors[vendor]) assert.ok(backend.some((t) => t.availability_label === expectedVendors[vendor]), `${vendor}: expected ${expectedVendors[vendor]}, backend says ${vendorSummary[vendor]} (${[...new Set(backend.map((t) => t.reason))].join("; ")})`);
  }
  for (const [vendor, label] of Object.entries(expectedVendors)) assert.ok(installed.includes(vendor), `${vendor} expected (${label}) but not installed`);
  const names = rows.map((r) => r.name);
  assert.equal(new Set(names).size, names.length, `No duplicate model labels in the picker: ${names}`);
  await screenshot("picker-vendors");
  await accessibility("picker-vendors");
  await execute("document.querySelector('.unified-picker-search input').dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
  await until("Picker closed", async () => !(await visible(".unified-picker-menu")));
  note(`picker: ${vendorHeading} (${Object.entries(vendorSummary).map(([v, s]) => `${v} ${s}`).join(", ") || "no vendor CLIs installed"}), On this computer and API keys (no key)`);

  // ------------------------------------------------------------ local model
  await openPicker();
  await clickButton("Add local model…");
  await until("Local models page", () => visible("#local-gguf"));
  await until("Runtime ready", async () => /Ready · CPU · 0\.0\.0-test/.test(await execute("return document.querySelector('.local-runtime').textContent")), 20000);
  await fill("#local-gguf", modelFile);
  await clickButton("Add");
  await until("Local model listed", () => execute("return !!document.querySelector('article.local-model[aria-label=\"Coder Test 1B\"]')"), 15000);
  const catalog = await api("GET", "/api/local-models");
  const localEntry = catalog.models.find((m) => m.path === modelFile);
  assert.ok(localEntry?.compatible, `Synthetic GGUF is compatible: ${JSON.stringify(localEntry)}`);
  assert.equal(catalog.runtime.path, fakeServer, "Runtime is the test double, never the real managed llama-server");
  assert.equal(catalog.loaded, null, "Adding does not load");
  await execute("document.querySelector('.settings-body').scrollTop=0");
  await screenshot("local-models");
  await accessibility("local-models");
  await closeSettings();
  note("local model added from Settings › Local models (runtime, hardware, GGUF entry)");

  const localTarget = (await api("GET", "/api/picker")).targets.find((t) => t.id === localEntry.id);
  assert.ok(localTarget, "Picker has the local row");
  await pickRow(localTarget.name);
  assert.equal(localTarget.group, "local");
  await openPicker();
  const withLocal = await pickerRows();
  assert.ok(withLocal.some((r) => r.group === "On this computer" && r.name === localTarget.name && r.selected), "Local row selected under On this computer");
  await screenshot("picker");
  await accessibility("picker");
  await execute("document.querySelector('.unified-picker-search input').dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
  await until("Picker closed", async () => !(await visible(".unified-picker-menu")));
  note(`selected local row "${localTarget.name}"`);
  await composerControlsFit("Desktop composer controls fit after selecting a model");
  // With a task typed and a model chosen, More offers Worktree and Compare
  // without an "unavailable" reason.
  await fill(composer, "Add a regression test for the parser");
  await click("details.composer-more > summary");
  await until("More task options open", () => execute("return document.querySelector('.composer-more')?.open"));
  assert.equal(await execute("return document.querySelectorAll('.composer-more-menu .composer-more-reason').length"), 0, "No unavailable reason once a task and model are chosen");
  assert.equal(await execute("return document.querySelector('.composer-more-menu .worktree-btn').getAttribute('aria-disabled')"), "false", "Worktree is available");
  await screenshot("composer-more-ready");
  await accessibility("composer-more-ready");
  await click("details.composer-more > summary");
  await until("More task options close", () => execute("return !document.querySelector('.composer-more')?.open"));
  await fill(composer, "");

  // ------------------------------------------------------------ local task
  await send("Create hello.txt with a greeting, then check it.");
  await until("Model server launched", async () => (await launches()).length > 0, 20000);
  await approve("hello.txt");
  await approve("cat hello.txt");
  await until("Summary card", () => visible('section[aria-label="Task summary"]'), 30000);
  assert.equal(await readFile(path.join(project, "hello.txt"), "utf8"), "Hello from ShadowCode\n");
  const summary = await execute("return document.querySelector('section[aria-label=\"Task summary\"]').innerText");
  assert.match(summary, /Finished/); assert.match(summary, /hello\.txt/);
  const steps = await execute("return [...document.querySelectorAll('.msg-with-activity .activity-step, .msg-summary .activity-step')].map(s=>s.innerText.replace(/\\s+/g,' ').trim())");
  assert.ok(steps.length >= 2, `Activity timeline has steps: ${steps}`);
  assert.match(await text(), /Created hello\.txt with a greeting/);
  const firstJob = (await api("GET", "/api/jobs")).jobs.find((j) => /Create hello\.txt/.test(j.task || ""));
  const sessionId = firstJob.session_id;
  assert.equal(firstJob.status, "completed");
  assert.equal(firstJob.routing?.model_id, localEntry.id, "Job keeps the exact picker id");
  const launchRecords = await launches();
  assert.ok(launchRecords.every((l) => l.key_env && !l.key_in_argv), "API key only in the environment");
  assert.ok(launchRecords[0].argv.includes("--jinja") && launchRecords[0].argv.includes("--no-webui"));
  assert.ok((await modelRequests()).every((r) => r.auth), "Every model request authenticated");
  await screenshot("task-complete");
  await accessibility("task-complete");
  note(`local task: write_file and exec approved, timeline [${steps.join(" | ")}], summary lists hello.txt`);

  // Changes drawer.
  await clickButton("Review changes");
  await until("Changes diff", () => execute("return /Hello from ShadowCode/.test(document.querySelector('[aria-label=\"Drawer\"], .drawer')?.innerText||'')"), 15000);
  await screenshot("changes");
  await accessibility("changes");
  const editorContext = { wd, session, execute, until, visible, click, clickButton, type, note, screenshot, project, artifacts };
  await nativeEditorBeforeReload(editorContext);
  await click("button.drawer-close");
  note("Changes drawer shows the hello.txt diff");

  // Workspace screenshots with a finished conversation.
  await screenshot("workspace-light");
  await accessibility("workspace-light");
  await setTheme("dark");
  await screenshot("workspace-dark");
  await accessibility("workspace-dark");
  await setTheme("light");

  // ------------------------------------------------------------ reload
  await wd("POST", `/session/${session}/refresh`, {});
  await until("Conversation restored after reload", async () => {
    const body = await text();
    return /Create hello\.txt with a greeting/.test(body) && /Created hello\.txt with a greeting/.test(body) && await visible('section[aria-label="Task summary"]');
  }, 20000);
  await until("Picker keeps the conversation's local row after reload", async () => (await execute("return document.querySelector('.unified-picker-trigger').getAttribute('aria-label')")) === `Model for this task: ${localTarget.name}`);
  assert.equal(await execute("return document.querySelector('.unified-picker-current').textContent"), localTarget.name.replace(/ · This computer$/, ""), "The Local badge replaces the suffix");
  await screenshot("reloaded");
  assert.equal(await execute("return document.documentElement.dataset.theme"), "light", "The saved light theme survives a reload");
  note("reload restores the conversation, summary card and the conversation's model");
  await nativeEditorAfterReload(editorContext);
  await nativeRunCheck({ wd, session, execute, until, visible, clickButton, fill, approve, screenshot, note, jobsFor, modelRequests, sessionId, project, artifacts });
  if (process.env.SHADOW_NATIVE_ENDURANCE === "1")
    await nativeEndurance({ wd, session, execute, until, visible, clickButton, fill, screenshot, note, api, jobsFor, modelRequests, sessionId, project, profile, artifacts, appPid, descendants });
  const recoveryContext = { wd, session, execute, until, visible, click, clickButton, fill, send, approve, screenshot, accessibility, note, api, jobsFor, modelRequests, launches, sessionId, project, runtimeDir, artifacts };
  await nativeLocalMemory(recoveryContext);
  await nativeReconnect(recoveryContext);

  // ------------------------------------------------------------ settings
  await openSettings("Accounts");
  await until("Accounts statuses", () => execute("return document.querySelectorAll('.settings-body article, .settings-body .account-card, .settings-body section').length>0"));
  await delay(500);
  const accounts = await execute("return document.querySelector('.settings-body').innerText");
  for (const vendor of installed) {
    const label = picker.vendors?.[`cli-${vendor}`]?.label;
    if (label) assert.ok(accounts.includes(label), `Accounts lists ${label}`);
  }
  await screenshot("accounts");
  await accessibility("accounts");
  await clickButton("Local models", "//nav[@aria-label='Settings sections']");
  await until("Local models page", () => execute("return !!document.querySelector('article.local-model[aria-label=\"Coder Test 1B\"]')"));
  assert.match(await execute("return document.querySelector('.settings-body').innerText"), /Hardware/);
  await screenshot("settings-local-models");
  await clickButton("Permissions & network", "//nav[@aria-label='Settings sections']");
  await until("Permissions page", () => execute("return /Ask before actions/.test(document.querySelector('.settings-body').innerText)"));
  await screenshot("permissions");
  await accessibility("permissions");
  // The remaining pages only read status here: nothing is installed, turned
  // on or connected. Each shows its content from the top (no stretched rows,
  // no "ApiError" prefix) and passes axe.
  const settingsText = () => execute("return document.querySelector('.settings-body').innerText");
  const pageSettled = () => execute("return !/^(Reading|Loading|Running) .*…$/m.test(document.querySelector('.settings-body').innerText)");
  for (const [section, ready] of [["Roles", /Run Code tasks as Plan → Implement → Review/], ["Code intelligence", /Language servers|language server/i], ["Voice", /Transcribe with/i], ["Appearance", /Theme/], ["Remote access", /Turn on remote access/], ["Your data", /Back up now/], ["About", /About ShadowCode/]]) {
    await clickButton(section, "//nav[@aria-label='Settings sections']");
    await until(`${section} page`, async () => (await pageSettled()) && ready.test(await settingsText()), 20000);
    assert.doesNotMatch(await settingsText(), /ApiError/, `${section} shows no raw error`);
    await accessibility(`settings-${section.toLowerCase().replace(/\W+/g, "-")}`);
  }
  // Rules & skills: a profile rule written in the window is saved in the
  // isolated profile folder (nothing outside it) and reaches the next local
  // task's system prompt (checked in the Stop step below).
  await clickButton("Rules & skills", "//nav[@aria-label='Settings sections']");
  await until("Rules & skills page", async () => (await pageSettled()) && /Your rules/i.test(await settingsText()), 20000);
  await fill("#profile-agents-md", WINDOW_RULE);
  await clickButton("Save rules");
  const profileRules = path.join(profile, "config/shadowcode/profile/AGENTS.md");
  await until("Profile rules saved", () => readFile(profileRules, "utf8").then((text) => text === WINDOW_RULE, () => false));
  assert.equal((await stat(path.dirname(profileRules))).mode & 0o777, 0o700, "The profile folder is private");
  await until("What each agent reads", () => execute("return !!document.querySelector('ul[aria-label^=\"What ShadowCode\"]')"), 20000);
  assert.doesNotMatch(await settingsText(), /ApiError/, "Rules & skills shows no raw error");
  await screenshot("settings-rules");
  await accessibility("settings-rules-and-skills");
  await clickButton("Advanced", "//nav[@aria-label='Settings sections']");
  for (const tab of ["Skills", "Health", "MCP", "Plugins", "Hooks", "Guardian", "Vendor tools"]) {
    await clickButton(tab, "//div[@role='tablist']");
    await until(`Advanced ${tab}`, async () => (await execute("return document.querySelector('[role=tab][aria-selected=true]')?.textContent")) === tab && (await pageSettled()), 20000);
    await settle();
    const bar = await execute("return document.querySelector('.advanced-tabs').getBoundingClientRect().height");
    assert.ok(bar < 60, `Advanced ${tab}: the tab bar stays one row (${bar}px)`);
    assert.doesNotMatch(await settingsText(), /ApiError/, `Advanced ${tab} shows no raw error`);
    const name = `settings-advanced-${tab.toLowerCase().replace(/\W+/g, "-")}`;
    if (["Health", "Guardian"].includes(tab)) await screenshot(name);
    await accessibility(name);
  }
  await closeSettings();
  note("Settings › Accounts, Local models, Permissions & network, Roles, Code intelligence, Voice, Appearance, Remote access, Your data, About and every Advanced tab render and pass axe (read-only)");
  note("Settings › Rules & skills saves a profile rule in the isolated profile folder (mode 700) and shows what each agent reads");

  // ------------------------------------------------------------ drawer panels
  await clickButton("Review changes");
  await until("Drawer open", () => visible(".drawer"));
  const closeBox = await execute("const c=document.querySelector('.drawer-close').getBoundingClientRect(),t=document.querySelector('.drawer-tab-list button').getBoundingClientRect();return Math.abs(c.top-t.top)");
  assert.ok(closeBox < 8, "Drawer Close sits on the first row of tabs");
  // Git: the second-opinion review before commit is offered (read only: no
  // review starts, nothing is staged or committed).
  await clickButton("Git", "//div[contains(@class,'drawer-tab-list')]");
  await until("Git tab offers a review before commit", () => visible('section[aria-label="Review before commit"] select[aria-label="Reviewer model"]'), 20000);
  await settle();
  assert.doesNotMatch(await execute("return document.querySelector('.drawer').innerText"), /ApiError/, "Git tab shows no raw error");
  await screenshot("drawer-git");
  await accessibility("drawer-git");
  note("Drawer: Git tab offers Review before commit with a reviewer picker (read-only)");
  await clickButton("Tasks", "//div[contains(@class,'drawer-tab-list')]");
  await until("Tasks panel", () => visible('input[aria-label="Search tasks"].search'));
  await screenshot("drawer-tasks");
  await accessibility("drawer-tasks");
  await clickButton("Tools", "//div[contains(@class,'drawer-tab-list')]");
  for (const view of ["Goals", "Automations", "Issues", "Processes", "Worktrees"]) {
    await clickButton(view, "//section[@aria-label='Tools']");
    await until(`Tools ${view}`, async () => (await execute("return [...document.querySelectorAll('.tools-tab .seg button')].find(b=>b.getAttribute('aria-pressed')==='true')?.textContent")) === view);
    await settle();
    assert.doesNotMatch(await execute("return document.querySelector('.drawer').innerText"), /ApiError/, `Tools ${view} shows no raw error`);
    if (view === "Processes") await screenshot("tools-processes");
    await accessibility(`tools-${view.toLowerCase()}`);
  }
  await click("button.drawer-close");
  note("Drawer: Close on the tab row; Tasks and every Tools view pass axe");

  // ------------------------------------------------------------ cloud consent
  const cloud = (await api("GET", "/api/picker")).targets.find((t) => t.inference === "cloud" && t.availability === "ready");
  if (cloud) {
    const before = (await jobsFor(sessionId)).length;
    // Guard: the page may only send this conversation's first cloud request
    // without consent (the backend must answer needs_consent); any request
    // with consent, or for another conversation, is refused in the page.
    // Tauri's invoke is read-only; the guard sits on the IPC transport
    // (fetch to ipc://localhost/<command>), which every invoke goes through.
    await execute(`
      const SID = ${JSON.stringify(sessionId)};
      const originalFetch = window.fetch;
      window.__ipcSeen = 0;
      window.__cloudRequests = [];
      window.fetch = function(input, init) {
        const url = String((input && input.url) || input);
        if (/^(ipc:\\/\\/localhost|https?:\\/\\/ipc\\.localhost)\\/api(\\?|$)/.test(url)) {
          window.__ipcSeen++;
          let args = null;
          try { args = JSON.parse(init && init.body); } catch (e) {}
          const r = args && args.request;
          if (r && r.method === 'POST' && r.path === '/api/jobs' && !String((r.body && r.body.model) || '').startsWith('local:')) {
            window.__cloudRequests.push(r.body);
            if (r.body.handoff_consent || r.body.session_id !== SID || !r.body.model) return Promise.reject(new Error('window test: cloud job blocked'));
          }
        }
        return originalFetch.apply(this, arguments);
      };
      return true;`);
    await api("GET", "/api/version");
    assert.ok(await execute("return window.__ipcSeen > 0"), "Cloud request guard sees IPC traffic");
    const lastJob = (await jobsFor(sessionId))[0];
    assert.equal(lastJob?.routing?.inference || "local", "local", "Previous turn ran on this computer, so consent is required");
    await pickRow(cloud.name);
    await send("Summarize what changed.");
    await until("Consent dialog", () => visible(".consent-dialog"), 15000);
    const consent = await execute("return document.querySelector('.consent-dialog').innerText");
    assert.match(consent, /Send to/); assert.match(consent, new RegExp(cloud.name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")));
    await screenshot("consent");
    await accessibility("consent");
    await clickButton("Cancel", "//div[contains(@class,'consent-dialog')]");
    await until("Consent closed", async () => !(await visible(".consent-dialog")));
    assert.equal(await execute("return document.querySelector(arguments[0]).value", [composer]), "Summarize what changed.", "Cancel keeps the message");
    const cloudRequests = await execute("return window.__cloudRequests");
    assert.equal(cloudRequests.length, 1); assert.equal(cloudRequests[0].handoff_consent, undefined);
    assert.equal((await jobsFor(sessionId)).length, before, "No job was created for the cloud row");
    note(`cloud row "${cloud.name}": consent dialog shown and cancelled; no vendor turn ran`);
    await fill(composer, "");
    await pickRow(localTarget.name);
  } else note("no Ready cloud row on this machine: consent step skipped");

  // ------------------------------------------------------------ pause/resume/stop
  await send("stop-probe: look around the project.");
  await until("Local task streaming", async () => (await modelRequests()).some((r) => r.request.includes("stop-probe")), 20000);
  const probeRequest = (await modelRequests()).find((r) => r.request.includes("stop-probe"));
  assert.ok(probeRequest.system.includes(WINDOW_RULE), "The profile rule saved in Settings reaches the local model's system prompt");
  assert.ok(probeRequest.system.includes("User instructions from the user's ShadowCode profile"), "Profile rules are labelled as the user's instructions");
  note("a profile rule saved in Rules & skills reaches the next local task's system prompt, labelled as the user's");
  await until("Stop button", () => visible('button[aria-label="Stop task"]'));
  await composerControlsFit("Desktop Stop and follow-up controls fit while running");
  await screenshot("running");
  await accessibility("running");
  const steeringButton = (label) => execute("return [...document.querySelectorAll('.steer-bar button')].some(b => b.textContent.trim() === arguments[0] && b.getClientRects().length > 0)", [label]);
  await clickButton("Pause task");
  await until("Resume control reflects paused job", () => steeringButton("Resume task"));
  assert.ok(await visible('button[aria-label="Stop task"]'), "Paused task remains active");
  const paused = (await jobsFor(sessionId)).find((j) => /stop-probe/.test(j.task || ""));
  assert.equal(paused?.status, "paused", "Pause saved by the native engine");
  await screenshot("paused");
  await clickButton("Resume task");
  await until("Pause control returns after resume", () => steeringButton("Pause task"));
  assert.ok(await visible('button[aria-label="Stop task"]'), "Resumed task remains active");
  const resumed = (await jobsFor(sessionId)).find((j) => j.id === paused.id);
  assert.equal(resumed?.status, "running", "Resume saved by the native engine");
  note("Pause and Resume update live controls without completing the task");
  await click('button[aria-label="Stop task"]');
  await until("Task stopped", async () => !(await visible('button[aria-label="Stop task"]')) && /Stopped|cancelled|Cancelled/.test(await text()), 20000);
  const stopped = (await jobsFor(sessionId)).find((j) => /stop-probe/.test(j.task || ""));
  assert.ok(!stopped || ["cancelled", "cancelling"].includes(stopped.status), `Stopped job status ${stopped?.status}`);
  await screenshot("stopped");
  note("Stop ends a running local task");

  // ------------------------------------------------------------ compact
  if (await setWindow({ width: 520, height: 860 })) {
    assert.equal(await horizontalOverflow(), false, "No horizontal scroll at 520 px");
    await composerControlsFit("Compact composer controls fit without overlaps");
    await screenshot("compact");
    await accessibility("compact");
    await setTheme("dark");
    await accessibility("compact-dark");
    await setTheme("light");
    await openPicker();
    assert.equal(await horizontalOverflow(), false, "Picker fits at 520 px");
    await screenshot("compact-picker");
    await accessibility("compact-picker");
    await execute("document.querySelector('.unified-picker-search input').dispatchEvent(new KeyboardEvent('keydown',{key:'Escape',bubbles:true}))");
    await setWindow(WIDE);
    note("520 px compact layout without horizontal scroll, light and dark axe clean");
    note("light screenshots render light and dark ones dark (computed page background)");
  }

  // ------------------------------------------------------------ quit
  const children = await descendants(appPid);
  const serverPids = (await launches()).map((l) => l.pid);
  const described = await Promise.all(children.map(async (pid) => `${pid} ${await commandLine(pid)}`));
  await writeFile(path.join(artifacts, "children-before-quit.txt"), described.join("\n"));
  await execute("setTimeout(()=>window.__TAURI_INTERNALS__.invoke('desktop_quit'),30);return true;");
  await until("App exited", () => dead(appPid), 30000);
  for (const pid of [...children, ...serverPids]) await until(`Child ${pid} exited (${await commandLine(pid)})`, () => dead(pid), 15000);
  const strays = (await run("pgrep", ["-f", runtimeDir]).catch(() => ({ stdout: "" }))).stdout.trim();
  assert.equal(strays, "", "No llama-server test double remains");
  note(`quit: app and ${children.length} child processes (${serverPids.length} llama-server launches) exited`);
  await wd("DELETE", `/session/${session}`).catch(() => {}); session = undefined;

  finalReport = { passed: displayEvidence.geometryFailures.length === 0, version: version.version, vendors: vendorSummary, checks, axe: axeFindings, screenshots: shots };
} catch (error) {
  finalReport = { passed: false, checks, failure: error.stack || String(error) };
  if (session) {
    await writeFile(path.join(artifacts, "failure.png"), Buffer.from(await wd("GET", `/session/${session}/screenshot`), "base64")).catch(() => {});
    await text().then((body) => writeFile(path.join(artifacts, "failure.txt"), body)).catch(() => {});
  }
  throw error;
} finally {
  if (session) await wd("DELETE", `/session/${session}`).catch(() => {});
  if (appPid && !(await dead(appPid))) {
    // A failed run must not leave the window (and xvfb-run) behind.
    const stray = await descendants(appPid);
    for (const pid of [appPid, ...stray]) try { process.kill(pid, "SIGKILL"); } catch { /* exited */ }
  }
  try { process.kill(-driver.pid, "SIGTERM"); } catch { /* exited */ }
  await delay(300);
  try { process.kill(-driver.pid, "SIGKILL"); } catch { /* exited */ }
  for (const record of await launches()) try { process.kill(record.pid, "SIGKILL"); } catch { /* exited */ }
  const busCleanup = await privateSession.cleanup();
  await writePrivateSessionReport(privateSession, busCleanup);
  finalReport ||= { passed: false, checks, failure: "Window test did not complete" };
  finalReport.display = displayEvidence;
  finalReport.private_session_cleanup = busCleanup;
  if (!busCleanup.ok) { finalReport.passed = false; process.exitCode = 1; }
  if (!finalReport.passed) process.exitCode = 1;
  await writeFile(path.join(artifacts, "result.json"), JSON.stringify(finalReport, null, 2) + "\n");
  console.log(`Native desktop window ${finalReport.passed ? "passed" : "failed"} (${checks.length} checks). Screenshots in ${artifacts}`);
  driverLog.end();
  if (!process.env.SHADOW_KEEP_SCRATCH) await rm(scratch, { recursive: true, force: true });
}
