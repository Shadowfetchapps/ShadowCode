// Controlled Wayland test session: a private Weston compositor (never the
// user's desktop), a private runtime directory and a private D-Bus session.
// This qualifies the app on a real Wayland protocol connection. It is not
// physical-desktop qualification (no real GPU output, input devices, IME,
// compositor shortcuts, cross-app clipboard or mixed DPI).
//
//   node scripts/run-native-wayland.mjs node scripts/test-native-desktop.mjs
//
// Environment:
//   SHADOW_WESTON              weston executable (default: weston on PATH)
//   SHADOW_WESTON_LIBRARY_PATH library path for an unpacked, uninstalled Weston
//   SHADOW_WESTON_MODULE_MAP   WESTON_MODULE_MAP for an unpacked Weston
//   SHADOW_WAYLAND_BACKEND     x11 (default): Weston's X11 backend inside a
//                              private Xvfb, which gives the compositor a
//                              keyboard/pointer seat; headless: no input seat
//   SHADOW_WAYLAND_ARTIFACTS   artifacts parent (default artifacts/native-wayland)
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { chmod, lstat, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from 'node:fs/promises';
import { constants, tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { executeScript } from './native-release-verification.mjs';
import { inspectPrivateActivations, linuxProc } from './native-test-session.mjs';

const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
// Variables that would let the app, the driver or D-Bus reach the user's
// desktop session instead of the private compositor.
const HOST_SESSION = ['DISPLAY', 'XAUTHORITY', 'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'XDG_CURRENT_DESKTOP', 'XDG_SESSION_DESKTOP',
  'DESKTOP_SESSION', 'XDG_SESSION_ID', 'DBUS_SESSION_BUS_ADDRESS', 'DBUS_SESSION_BUS_PID', 'DBUS_SESSION_BUS_WINDOWID',
  'DBUS_STARTER_ADDRESS', 'DBUS_STARTER_BUS_TYPE', 'WESTON_CONFIG_FILE', 'WESTON_MODULE_MAP'];
const SOCKET = 'wayland-shadowcode-test';

/** The command line that starts the private compositor. Pure, for tests. */
export function compositorCommand({ weston, backend, runtime, config }) {
  assert(['x11', 'headless'].includes(backend), 'SHADOW_WAYLAND_BACKEND must be x11 or headless');
  const args = [`--backend=${backend}`, '--renderer=pixman', '--shell=desktop', `--socket=${SOCKET}`,
    `--config=${config}`, '--idle-time=0', '--width=1600', '--height=1100'];
  if (backend === 'headless') return { exe: weston, args };
  // xvfb-run owns the private X server (fresh cookie) and stops it when
  // Weston exits. GLX is disabled as in the X11 runner: the compositor uses
  // the Pixman software renderer.
  return { exe: 'xvfb-run', args: ['-a', '-s', '-screen 0 1600x1100x24 -extension GLX', weston, ...args] };
}

/** Start the compositor as the leader of its own process group. */
function startCompositor({ exe, args, env, log }) {
  const child = spawn(exe, args, { env, detached: true, stdio: ['ignore', 'pipe', 'pipe'] });
  const handle = { child, exited: false, code: null, signal: null, output: '' };
  const record = text => { handle.output = (handle.output + text).slice(-65536); };
  child.stdout.setEncoding('utf8').on('data', record);
  child.stderr.setEncoding('utf8').on('data', record);
  handle.closed = new Promise(resolve => child.on('close', (code, signal) => {
    Object.assign(handle, { exited: true, code, signal });
    resolve();
  }));
  child.on('error', error => { record(`\n${error.message}\n`); });
  handle.stop = async () => {
    if (!handle.exited) {
      try { process.kill(-child.pid, 'SIGTERM'); } catch { /* group gone */ }
      await Promise.race([handle.closed, delay(5000)]);
    }
    if (!handle.exited) {
      try { process.kill(-child.pid, 'SIGKILL'); } catch { /* group gone */ }
      await Promise.race([handle.closed, delay(2000)]);
    }
    await writeFile(log, handle.output);
    return handle.exited;
  };
  return handle;
}

async function socketReady(socket, compositor, timeout = 20000) {
  const end = Date.now() + timeout;
  while (Date.now() < end) {
    assert(!compositor.exited, `The private compositor exited before its socket appeared (${compositor.code ?? compositor.signal}): ${compositor.output.slice(-2000)}`);
    try {
      const info = await stat(socket);
      if (info.isSocket()) return { socket, uid: info.uid, inode: info.ino };
    } catch { /* not yet */ }
    await delay(100);
  }
  throw new Error('The private compositor socket did not appear within 20 s');
}

/** Processes still in the compositor's group (Weston, its shell helpers, Xvfb). */
async function groupMembers(groupId, io = linuxProc) {
  const members = [];
  for (const pid of await io.pids()) {
    try {
      const raw = await readFile(`/proc/${pid}/stat`, 'utf8');
      const fields = raw.slice(raw.lastIndexOf(') ') + 2).split(' ');
      if (Number(fields[2]) === groupId && !['Z', 'X'].includes(fields[0])) members.push(pid);
    } catch { /* exited */ }
  }
  return members;
}

async function removeRuntime(directory, owned) {
  const current = await lstat(directory);
  assert(current.isDirectory() && !current.isSymbolicLink() && current.ino === owned.ino && current.dev === owned.dev, 'Private runtime root identity changed');
  async function inspect(dir) {
    for (const name of await readdir(dir)) {
      const item = path.join(dir, name), info = await lstat(item);
      assert.equal(info.dev, owned.dev, 'Private runtime filesystem boundary changed');
      if (info.isDirectory() && !info.isSymbolicLink()) await inspect(item);
    }
  }
  await inspect(directory);
  await rm(directory, { recursive: true });
}

export async function runNativeWayland(argv, {
  artifactParent = process.env.SHADOW_WAYLAND_ARTIFACTS || path.resolve('artifacts/native-wayland'),
  weston = process.env.SHADOW_WESTON || 'weston',
  backend = process.env.SHADOW_WAYLAND_BACKEND || 'x11',
  libraryPath = process.env.SHADOW_WESTON_LIBRARY_PATH || '',
  moduleMap = process.env.SHADOW_WESTON_MODULE_MAP || '',
  execute = executeScript, compositor: launch = startCompositor,
  identity = pid => linuxProc.identity(pid), activationInventory = inspectPrivateActivations,
} = {}) {
  assert(argv.length && argv.every(arg => typeof arg === 'string' && !arg.includes('\0')), 'Supply COMMAND [ARGUMENT...]');
  artifactParent = path.resolve(artifactParent);
  await mkdir(artifactParent, { recursive: true });
  const artifacts = await mkdtemp(path.join(artifactParent, 'run-'));
  const runtime = await mkdtemp(path.join(tmpdir(), 'shadowcode-wayland-'));
  await chmod(runtime, 0o700);
  const runtimeIdentity = await lstat(runtime);
  const sessionFile = path.join(artifacts, 'private-session.json');
  const config = path.join(runtime, 'weston.ini');
  // No panel or background client: fewer helper processes, nothing on screen
  // but the app window.
  await writeFile(config, '[core]\nidle-time=0\n\n[shell]\npanel-position=none\nbackground-color=0xff202020\nlocking=false\n', { mode: 0o600 });
  const command = compositorCommand({ weston, backend, runtime, config });
  const report = { schema: 1, scope: `Private Weston (${backend} backend${backend === 'x11' ? ' in a private Xvfb, virtual keyboard/pointer seat' : ', no input seat'}, Pixman renderer), private D-Bus/runtime; HOME and account config roots preserved. Real Wayland protocol connection; not physical desktop, GPU output, IME, compositor shortcuts, cross-app clipboard or mixed-DPI qualification.`,
    argv, artifacts, runtime, runtime_mode: '0700', backend, compositor: command, command_exit: null, output_complete: false,
    external_boundary: [], failures: [], ok: false };
  const compositorEnv = { ...process.env, XDG_RUNTIME_DIR: runtime };
  for (const key of HOST_SESSION) delete compositorEnv[key];
  if (libraryPath) compositorEnv.LD_LIBRARY_PATH = libraryPath;
  if (moduleMap) compositorEnv.WESTON_MODULE_MAP = moduleMap;
  let server;
  try {
    server = launch({ ...command, env: compositorEnv, log: path.join(artifacts, 'compositor.log') });
    report.compositor_socket = await socketReady(path.join(runtime, SOCKET), server);
    assert.equal(report.compositor_socket.uid, process.getuid(), 'The compositor socket must belong to this user');
    const run = ['env', ...HOST_SESSION.flatMap(key => ['-u', key]), '-u', 'LD_LIBRARY_PATH',
      'GDK_BACKEND=wayland', 'XDG_SESSION_TYPE=wayland', 'XDG_CURRENT_DESKTOP=ShadowCodePrivateWeston',
      `XDG_RUNTIME_DIR=${runtime}`, `WAYLAND_DISPLAY=${SOCKET}`, 'SHADOW_NATIVE_DISPLAY=wayland',
      `SHADOW_NATIVE_SESSION_REPORT=${sessionFile}`, 'dbus-run-session', '--', ...argv];
    try { report.command_exit = await execute(run.map(quote).join(' '), () => {}); report.output_complete = true; }
    catch (error) {
      report.failures.push(error.message);
      const signal = /^Gate terminated by (SIG\w+)$/.exec(error.message)?.[1];
      if (signal) report.signal = signal;
    }
    if (report.command_exit !== 0) report.failures.push(`Native Wayland command did not exit successfully (${report.command_exit ?? report.signal ?? 'incomplete output'})`);
    if (server.exited) report.failures.push('The private compositor exited while the command was running');
    const raw = await readFile(sessionFile, 'utf8');
    assert(Buffer.byteLength(raw) <= 256 * 1024, 'Private session report too large');
    const session = JSON.parse(raw);
    assert.equal(session.schema, 1);
    if (session.cleanup?.ok !== true) report.failures.push('Inner private-session cleanup did not pass');
    assert(Array.isArray(session.cleanup.services) && session.cleanup.services.length <= 32);
    const expected = [session.identity.self, session.identity.launcher, session.identity.daemon, ...session.cleanup.services];
    assert.equal(session.identity.launcher.exe, '/usr/bin/dbus-run-session');
    assert.equal(session.identity.daemon.exe, '/usr/bin/dbus-daemon');
    const deadline = Date.now() + 5000;
    for (const record of expected) {
      assert(Number.isSafeInteger(record.pid) && record.pid > 0 && /^\d+$/.test(record.start) && typeof record.exe === 'string' && record.uid === process.getuid(), 'Invalid recorded private-session identity');
      let current;
      for (;;) {
        current = await identity(record.pid);
        const live = current && !['Z', 'X'].includes(current.state) && current.start === record.start;
        if (!live || Date.now() >= deadline) break;
        await delay(50);
      }
      const sameLive = current && !['Z', 'X'].includes(current.state) && current.start === record.start;
      report.external_boundary.push({ pid: record.pid, start: record.start, exe: record.exe, same_live_process: !!sameLive });
      assert(!sameLive, 'Recorded private-session process still alive after command/output closure');
    }
    report.late_activations = await activationInventory(session.identity);
    assert.equal(report.late_activations.length, 0, 'Private activation survived outside the inner cleanup snapshot; runtime preserved');
  } catch (error) { report.failures.push(error.message); }
  if (server) {
    const group = server.child.pid;
    report.compositor_stopped = await server.stop();
    if (!report.compositor_stopped) report.failures.push('The private compositor did not exit after SIGTERM/SIGKILL');
    const survivors = group ? await groupMembers(group) : [];
    report.compositor_group_survivors = survivors;
    if (survivors.length) report.failures.push(`Compositor processes survived: ${survivors.join(', ')}`);
  }
  report.ok = report.failures.length === 0;
  if (report.ok) {
    try { await removeRuntime(runtime, runtimeIdentity); report.runtime_removed = true; }
    catch (error) { report.ok = false; report.failures.push(error.message); }
  }
  await writeFile(path.join(artifacts, 'result.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ native_wayland: report.ok, artifacts, failures: report.failures }));
  return report;
}

if (import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  try {
    const report = await runNativeWayland(process.argv.slice(2));
    process.exitCode = report.ok ? 0 : report.command_exit > 0 ? report.command_exit : report.signal ? 128 + constants.signals[report.signal] : 1;
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
