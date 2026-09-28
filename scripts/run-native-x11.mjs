// Controlled X11 test session. This is not physical-desktop qualification.
import assert from 'node:assert/strict';
import { chmod, mkdir, mkdtemp, readFile, readdir, lstat, rm, writeFile } from 'node:fs/promises';
import { tmpdir, constants } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { executeScript } from './native-release-verification.mjs';
import { linuxProc, inspectPrivateActivations } from './native-test-session.mjs';
const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
const decodeMount = value => value.replace(/\\([0-7]{3})/g, (_, code) => String.fromCharCode(parseInt(code, 8)));
async function removeRuntime(directory, owned) {
  const current = await lstat(directory);
  assert(current.isDirectory() && !current.isSymbolicLink() && current.ino === owned.ino && current.dev === owned.dev, 'Private runtime root identity changed');
  const mounts = (await readFile('/proc/self/mountinfo', 'utf8')).split('\n').filter(Boolean).map(row => decodeMount(row.split(' ')[4]));
  assert(!mounts.some(mount => mount === directory || mount.startsWith(directory + '/')), 'Private runtime still contains a mount; preserved');
  // Root is generated with mode 0700 and all admitted activations have exited. Do not
  // traverse symlinks; reject a changed filesystem boundary before removal.
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
export async function runNativeX11(argv, {
  artifactParent = process.env.SHADOW_X11_ARTIFACTS || path.resolve('artifacts/native-x11'),
  execute = executeScript, identity = pid => linuxProc.identity(pid), activationInventory = inspectPrivateActivations,
} = {}) {
  assert(argv.length && argv.every(arg => typeof arg === 'string' && !arg.includes('\0')), 'Supply COMMAND [ARGUMENT...]');
  artifactParent = path.resolve(artifactParent);
  await mkdir(artifactParent, { recursive: true });
  const artifacts = await mkdtemp(path.join(artifactParent, 'run-'));
  const runtime = await mkdtemp(path.join(tmpdir(), 'shadowcode-x11-'));
  await chmod(runtime, 0o700);
  const runtimeIdentity = await lstat(runtime);
  const sessionFile = path.join(artifacts, 'private-session.json');
  const command = ['env', ...['DISPLAY','XAUTHORITY','WAYLAND_DISPLAY','WAYLAND_SOCKET','XDG_CURRENT_DESKTOP','XDG_SESSION_DESKTOP','DESKTOP_SESSION','DBUS_SESSION_BUS_ADDRESS','DBUS_SESSION_BUS_PID','DBUS_SESSION_BUS_WINDOWID','DBUS_STARTER_ADDRESS','DBUS_STARTER_BUS_TYPE'].flatMap(key => ['-u', key]),
    'GDK_BACKEND=x11', 'XDG_SESSION_TYPE=x11', `XDG_RUNTIME_DIR=${runtime}`, `SHADOW_NATIVE_SESSION_REPORT=${sessionFile}`,
    // This UI exercises no OpenGL surface. Some host NVIDIA EGL stacks crash
    // Xvfb while initializing GLX, leaving a display number but no X server.
    // Disabling GLX keeps the controlled UI session on software rendering.
    'xvfb-run', '-a', '-s', '-screen 0 1440x1100x24 -extension GLX', 'dbus-run-session', '--', ...argv];
  const report = { schema: 1, scope: 'Controlled Xvfb/X11, private D-Bus/runtime; HOME and account config/data/state/cache roots preserved. Not physical Wayland/desktop or portal feature qualification.',
    argv, artifacts, runtime, runtime_mode: '0700', command_exit: null, output_complete: false, external_boundary: [], failures: [], ok: false };
  try {
    try { report.command_exit = await execute(command.map(quote).join(' '), () => {}); report.output_complete = true; }
    catch (error) {
      report.failures.push(error.message);
      const signal = /^Gate terminated by (SIG\w+)$/.exec(error.message)?.[1];
      if (signal) report.signal = signal;
    }
    if (report.command_exit !== 0) report.failures.push(`Native X11 command did not exit successfully (${report.command_exit ?? report.signal ?? 'incomplete output'})`);
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
      let timer;
      const remaining = deadline - Date.now(); assert(remaining > 0, 'External process check deadline reached');
      let current;
      try { current = await Promise.race([identity(record.pid), new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('External process check timed out')), remaining); })]); }
      finally { clearTimeout(timer); }
      const sameLive = current && !['Z','X'].includes(current.state) && current.start === record.start;
      report.external_boundary.push({ pid: record.pid, start: record.start, exe: record.exe, same_live_process: !!sameLive });
      assert(!sameLive, 'Recorded private-session process still alive after command/output closure');
    }
    let timer;
    const remaining = deadline - Date.now(); assert(remaining > 0, 'External process check deadline reached');
    try {
      report.late_activations = await Promise.race([activationInventory(session.identity), new Promise((_, reject) => {
        timer = setTimeout(() => reject(new Error('External activation inventory timed out')), remaining);
      })]);
    } finally { clearTimeout(timer); }
    assert.equal(report.late_activations.length, 0, 'Private activation survived outside the inner cleanup snapshot; runtime preserved');
    assert(Date.now() < deadline, 'External process check deadline reached');
    report.ok = report.failures.length === 0;
    if (report.ok) { await removeRuntime(runtime, runtimeIdentity); report.runtime_removed = true; }
  } catch (error) { report.ok = false; report.failures.push(error.message); }
  await writeFile(path.join(artifacts, 'result.json'), JSON.stringify(report, null, 2) + '\n');
  console.log(JSON.stringify({ native_x11: report.ok, artifacts, failures: report.failures }));
  return report;
}
if (import.meta.url === pathToFileURL(process.argv[1] || '').href) {
  try {
    const report = await runNativeX11(process.argv.slice(2));
    process.exitCode = report.ok ? 0 : report.command_exit > 0 ? report.command_exit : report.signal ? 128 + constants.signals[report.signal] : 1;
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
