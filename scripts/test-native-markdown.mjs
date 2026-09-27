// Isolated native WebKit qualification of the packaged Markdown worker.
// No model turns, subscriptions, account probes, or production configuration.
//
// Build ui/dist and the desktop binary first, then run:
//   xvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- \
//     node scripts/test-native-markdown.mjs
// This qualifies the supplied native binary under Xvfb/X11 and WebKitGTK.
// It does not qualify Wayland, an AppImage package, or native UI performance.
//
// SHADOW_DESKTOP_BINARY  binary (default target/debug/shadowcode)
// SHADOW_TAURI_DRIVER    tauri-driver executable (default tauri-driver on PATH)
// SHADOW_WEBKIT_DRIVER   optional WebKitWebDriver path (--native-driver)
// SHADOW_NATIVE_ARTIFACTS output parent (default artifacts/native-markdown)
// Each run creates its own output directory; previous evidence is retained.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createServer } from 'node:net';
import { spawn } from 'node:child_process';
import { createWriteStream } from 'node:fs';
import { mkdir, mkdtemp, readFile, readdir, readlink, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = fileURLToPath(new URL('../', import.meta.url));
const binary = await realpath(process.env.SHADOW_DESKTOP_BINARY || path.join(repo, 'target/debug/shadowcode'));
const driverBinary = process.env.SHADOW_TAURI_DRIVER || 'tauri-driver';
const webkitDriver = process.env.SHADOW_WEBKIT_DRIVER;
const artifactParent = path.resolve(process.env.SHADOW_NATIVE_ARTIFACTS || path.join(repo, 'artifacts/native-markdown'));
await mkdir(artifactParent, { recursive: true });
const artifacts = await mkdtemp(path.join(artifactParent, 'run-'));
const scratch = await mkdtemp(path.join(tmpdir(), 'shadowcode-native-markdown-'));
const profile = path.join(scratch, 'profile');
const project = path.join(scratch, 'project');
await mkdir(project, { recursive: true });
await mkdir(path.join(profile, 'config'), { recursive: true });
await writeFile(path.join(project, 'README.md'), '# Disposable native worker probe\n');
await writeFile(path.join(profile, 'config/config.yaml'), JSON.stringify({
  cli_agents: { enabled: false, claude_enabled: false },
  network: { mode: 'offline' },
  onboarding: { completed: true, workspace: project },
  trusted_workspaces: [project],
  ui: { notify: false },
}));
const html = await readFile(path.join(repo, 'ui/dist/index.html'), 'utf8');
const expectedScript = html.match(/src="([^"]+\.js)"/)[1];
const workers = (await readdir(path.join(repo, 'ui/dist/assets'))).filter((p) => /^markdown\.worker-.+\.js$/.test(p));
assert.equal(workers.length, 1, 'Exactly one production Markdown worker asset');
const workerPath = `/assets/${workers[0]}`;
const workerBytes = await readFile(path.join(repo, 'ui/dist', workerPath.slice(1)));
const expectedWorkerSha256 = createHash('sha256').update(workerBytes).digest('hex');
const delay = (ms) => new Promise((r) => setTimeout(r, ms));
// A PID alone is not an ownership token: session deletion can terminate the
// process before cleanup runs, and Linux may reuse its PID.
async function processIdentity(pid) {
  if (!Number.isSafeInteger(pid) || pid <= 0) return null;
  try {
    const stat = await readFile(`/proc/${pid}/stat`, 'utf8');
    const fields = stat.slice(stat.lastIndexOf(') ') + 2).split(' ');
    return { pid, group: Number(fields[2]), start: fields[19], state: fields[0] };
  } catch { return null; }
}
async function sameProcess(identity) {
  const current = identity && await processIdentity(identity.pid);
  return !!current && current.state !== 'Z' && current.start === identity.start;
}
const interrupted = new AbortController();
const onInterrupt = (signal) => {
  process.exitCode = signal === 'SIGINT' ? 130 : 143;
  interrupted.abort(new Error(`Probe interrupted by ${signal}`));
};
process.on('SIGINT', onInterrupt);
process.on('SIGTERM', onInterrupt);
async function unusedPort() {
  const server = createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const { port } = server.address();
  await new Promise((resolve) => server.close(resolve));
  return port;
}
const driverPort = await unusedPort();
const nativePort = await unusedPort();
const env = {
  ...process.env,
  XDG_CONFIG_HOME: path.join(profile, 'xdg-config'),
  XDG_DATA_HOME: path.join(profile, 'xdg-data'),
  XDG_STATE_HOME: path.join(profile, 'xdg-state'),
  XDG_CACHE_HOME: path.join(profile, 'xdg-cache'),
  GDK_BACKEND: 'x11', WEBKIT_DISABLE_DMABUF_RENDERER: '1',
};
delete env.WAYLAND_DISPLAY;
delete env.WAYLAND_SOCKET;
const log = createWriteStream(path.join(artifacts, 'webdriver.log'));
const driver = spawn(driverBinary, [
  '--port', String(driverPort), '--native-port', String(nativePort),
  ...(webkitDriver ? ['--native-driver', webkitDriver] : []),
], { cwd: project, env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
driver.stdout.pipe(log); driver.stderr.pipe(log);
let spawnError, session, appIdentity, driverIdentity;
const ownedGroup = new Map();
driver.on('error', (error) => { spawnError = error; });
async function wd(method, route, body, timeout = 30000, interruptible = true) {
  const response = await fetch(`http://127.0.0.1:${driverPort}${route}`, {
    method, headers: { 'Content-Type': 'application/json' },
    ...(body === undefined ? {} : { body: JSON.stringify(body) }),
    signal: interruptible
      ? AbortSignal.any([interrupted.signal, AbortSignal.timeout(timeout)])
      : AbortSignal.timeout(timeout),
  });
  const data = await response.json();
  if (!response.ok || data.value?.error) throw new Error(JSON.stringify(data.value));
  return data.value;
}
const execute = (script, args = [], timeout = 30000) => wd('POST', `/session/${session}/execute/sync`, { script, args }, timeout);
async function until(label, test, timeout = 30000) {
  const deadline = Date.now() + timeout;
  let last;
  while (Date.now() < deadline) {
    interrupted.signal.throwIfAborted();
    if (spawnError) throw spawnError;
    try { const value = await test(Math.max(1, deadline - Date.now())); if (value) return value; } catch (error) { last = error; }
    await delay(100);
  }
  throw new Error(`${label} timed out: ${last || ''}`);
}
async function native(command, args = {}) {
  const value = await wd('POST', `/session/${session}/execute/async`, {
    script: 'const done=arguments[arguments.length-1]; window.__TAURI_INTERNALS__.invoke(arguments[0],arguments[1]).then(value=>done({value}),error=>done({error:String(error)}));',
    args: [command, args],
  });
  if (value.error) throw new Error(value.error);
  return value.value;
}
const api = (route) => native('api', { request: { method: 'GET', path: route, body: null } });
const report = {
  scope: 'Native Tauri Markdown module worker under Xvfb/X11 WebKitGTK; not Wayland, AppImage package, or UI performance qualification',
  binary, artifacts, workerPath, expectedScript, expectedWorkerSha256,
  profile, project, cli_agents_enabled: false, network_mode: 'offline',
};
async function rememberOwnedGroup() {
  // Only enumerate the group while an already-owned member still exists.
  // A live member prevents this process-group ID from being recycled.
  if (!(await Promise.all([...ownedGroup.values()].map(sameProcess))).some(Boolean)) return;
  for (const name of await readdir('/proc')) {
    if (!/^\d+$/.test(name)) continue;
    const identity = await processIdentity(Number(name));
    if (identity?.group === driver.pid) ownedGroup.set(identity.pid, identity);
  }
}
async function signalOwnedGroup(signal) {
  await rememberOwnedGroup();
  if ((await Promise.all([...ownedGroup.values()].map(sameProcess))).some(Boolean)) {
    try { process.kill(-driver.pid, signal); } catch {}
  }
}
async function signalApp(signal) {
  if (!await sameProcess(appIdentity)) return;
  try {
    const executable = await readlink(`/proc/${appIdentity.pid}/exe`);
    const args = (await readFile(`/proc/${appIdentity.pid}/cmdline`, 'utf8')).split('\0');
    if (executable !== binary || args[args.indexOf('--profile') + 1] !== profile) return;
    if (await sameProcess(appIdentity)) process.kill(appIdentity.pid, signal);
  } catch {}
}
try {
  driverIdentity = await processIdentity(driver.pid);
  if (driverIdentity) ownedGroup.set(driver.pid, driverIdentity);
  report.binarySha256 = createHash('sha256').update(await readFile(binary)).digest('hex');
  await until('WebDriver startup', (remaining) => wd('GET', '/status', undefined, remaining));
  session = (await wd('POST', '/session', { capabilities: { alwaysMatch: { 'tauri:options': {
    application: binary, args: ['--profile', profile, '--workspace', project],
  } } } })).sessionId;
  await wd('POST', `/session/${session}/timeouts`, { script: 20000, implicit: 0, pageLoad: 30000 });
  await until('Native application ready', (remaining) => execute('return !!window.__TAURI_INTERNALS__ && !!document.querySelector(".app-shell, textarea, #onboarding-folder")', [], remaining));
  report.version = await api('/api/version');
  assert.equal(report.version.transport, 'native');
  assert.equal(report.version.runtime, 'rust');
  const candidate = await processIdentity(report.version.pid);
  assert.ok(candidate, 'Native API reports a live process');
  assert.equal(await readlink(`/proc/${candidate.pid}/exe`), binary);
  const appArgs = (await readFile(`/proc/${candidate.pid}/cmdline`, 'utf8')).split('\0');
  assert.equal(appArgs[appArgs.indexOf('--profile') + 1], profile, 'Native process belongs to this isolated profile');
  assert.equal(await sameProcess(candidate), true);
  appIdentity = candidate;
  await rememberOwnedGroup();
  report.page = await execute('return { url: location.href, userAgent: navigator.userAgent, mainScript: new URL(document.querySelector("script[type=module]").src).pathname, csp: document.querySelector("meta[http-equiv=Content-Security-Policy]")?.content || null }');
  assert.equal(report.page.mainScript, expectedScript, 'Fresh embedded production main bundle');
  assert.match(report.page.url, /^tauri:\/\/localhost/);
  const cfg = await api('/api/config');
  report.config = { cli_agents_enabled: cfg.cli_agents.enabled, network_mode: cfg.network.mode };
  assert.equal(cfg.cli_agents.enabled, false);
  assert.equal(cfg.network.mode, 'offline');
  const text = '# Native worker proof\n\n[Reference][later]\n\n' + 'A complete recorded paragraph.\n\n'.repeat(400) +
    '```ts\nconst answer = 42;\n```\n\n[later]: https://example.com\n\n| Left | Right |\n| --- | --- |\n| A | B |\n';
  report.worker = await wd('POST', `/session/${session}/execute/async`, {
    args: [workerPath, text],
    script: `
      const done = arguments[arguments.length - 1];
      const url = new URL(arguments[0], location.href), text = arguments[1];
      const result = { url: url.href, violations: [] };
      const requests = new AbortController();
      let worker, finished = false;
      const violation = (e) => result.violations.push({ directive: e.effectiveDirective, blocked: e.blockedURI });
      document.addEventListener('securitypolicyviolation', violation);
      const finish = (extra) => { if (finished) return; finished = true; clearTimeout(timer); requests.abort(); worker?.terminate(); document.removeEventListener('securitypolicyviolation', violation); done({ ...result, ...extra }); };
      const timer = setTimeout(() => finish({ ok: false, failure: 'worker timeout' }), 10000);
      (async () => {
        const documentResponse = await fetch(location.href, { signal: requests.signal });
        result.documentCsp = documentResponse.headers.get('content-security-policy');
        const response = await fetch(url, { signal: requests.signal });
        const bytes = await response.arrayBuffer();
        result.asset = { status: response.status, contentType: response.headers.get('content-type'), bytes: bytes.byteLength,
          sha256: Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)), b => b.toString(16).padStart(2, '0')).join('') };
        if (finished) return;
        worker = new Worker(url, { type: 'module' });
        worker.onerror = e => finish({ ok: false, failure: 'worker error', message: e.message, filename: e.filename, line: e.lineno });
        worker.onmessageerror = () => finish({ ok: false, failure: 'message error' });
        worker.onmessage = ({ data }) => {
          const tags = {}, links = [], headings = [], code = [];
          const plain = node => node.value || (node.children || []).map(plain).join('');
          const visit = node => {
            if (node.type === 'element') {
              tags[node.tagName] = (tags[node.tagName] || 0) + 1;
              if (node.tagName === 'a') links.push(node.properties.href);
              if (node.tagName === 'h1') headings.push(plain(node));
              if (node.tagName === 'code') code.push(plain(node));
            }
            (node.children || []).forEach(visit);
          };
          if (data.tree) visit(data.tree);
          finish({ ok: !!data.tree, replyId: data.id, documentError: data.error || null, rootType: data.tree?.type, tags, links, headings, code });
        };
        worker.postMessage({ id: 41, text });
      })().catch(error => finish({ ok: false, failure: 'constructor or asset error', message: String(error) }));
    `,
  });
  assert.equal(report.worker.ok, true, JSON.stringify(report.worker));
  const scriptSources = report.worker.documentCsp.split(';').map((directive) => directive.trim().split(/\s+/)).find(([name]) => name === 'script-src')?.slice(1);
  assert.ok(scriptSources?.includes("'self'"), 'Native script CSP retains self');
  assert.ok(scriptSources.every((source) => source === "'self'" || /^'sha(?:256|384|512)-[A-Za-z0-9+/]+={0,2}'$/.test(source)), 'Script sources are only self and Tauri hashes');
  assert.doesNotMatch(report.worker.documentCsp, /'unsafe-eval'|worker-src\s+\*/);
  assert.equal(report.worker.asset.sha256, expectedWorkerSha256, 'Served embedded worker matches production asset bytes');
  assert.equal(report.worker.asset.status, 200);
  assert.match(report.worker.asset.contentType, /^(?:text|application)\/javascript(?:;|$)/);
  assert.equal(report.worker.asset.bytes, workerBytes.length);
  assert.equal(report.worker.replyId, 41);
  assert.equal(report.worker.rootType, 'root');
  assert.deepEqual(report.worker.headings, ['Native worker proof']);
  assert.equal(report.worker.tags.table, 1);
  assert.deepEqual(report.worker.links, ['https://example.com']);
  assert.deepEqual(report.worker.code, ['const answer = 42;\n']);
  assert.equal(report.worker.violations.length, 0);
  report.jobs = (await api('/api/jobs')).jobs.length;
  assert.equal(report.jobs, 0, 'No model tasks were created');
  report.ok = true;
} catch (error) {
  report.ok = false; report.failure = error.stack || String(error); process.exitCode ||= 1;
} finally {
  await rememberOwnedGroup();
  if (session) {
    try { await wd('DELETE', `/session/${session}`, undefined, 3000, false); } catch {}
  }
  await signalOwnedGroup('SIGTERM');
  await signalApp('SIGTERM');
  await delay(400);
  await signalOwnedGroup('SIGKILL');
  await signalApp('SIGKILL');
  await delay(200);
  const groupAlive = (await Promise.all([...ownedGroup.values()].map(async (id) => await sameProcess(id) ? id.pid : null))).filter(Boolean);
  report.cleanup = { driver_alive: await sameProcess(driverIdentity), app_alive: await sameProcess(appIdentity), group_alive: groupAlive };
  if (report.cleanup.driver_alive || report.cleanup.app_alive || groupAlive.length) {
    report.ok = false;
    report.cleanup.failure = 'Owned probe process remained after bounded cleanup';
    process.exitCode = 1;
  }
  log.end();
  // Scratch contains only this test's generated profile and project.
  if (!report.cleanup.driver_alive && !report.cleanup.app_alive && !groupAlive.length) {
    await rm(scratch, { recursive: true, force: true });
    report.cleanup.profile_removed = true;
  }
  await writeFile(path.join(artifacts, 'result.json'), JSON.stringify(report, null, 2) + '\n');
  process.off('SIGINT', onInterrupt);
  process.off('SIGTERM', onInterrupt);
  console.log(JSON.stringify({ ok: report.ok, artifacts, worker: report.worker, failure: report.failure }, null, 2));
}
