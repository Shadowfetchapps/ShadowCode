import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm, stat, mkdir, chmod, readdir } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { runNativeX11 } from './run-native-x11.mjs';
import { compositorCommand, runNativeWayland } from './run-native-wayland.mjs';
import { createServer } from 'node:net';
import { capturePrivateSession, inspectPrivateActivations } from './native-test-session.mjs';
const address = 'unix:path=/tmp/private-test-bus,guid=' + 'a'.repeat(32);
const host = 'unix:path=/run/user/1000/bus,guid=' + 'b'.repeat(32);
function fixture() {
  const proc = (pid, parent, start, exe, uid = 1000) => ({ pid, parent, start: String(start), exe, uid, state: 'S' });
  const rows = new Map([
    [1, proc(1, 0, 10, '/usr/bin/dbus-run-session')],
    [2, proc(2, 1, 11, '/usr/bin/dbus-daemon')],
    [3, proc(3, 1, 12, '/usr/bin/node')],
    [4, proc(4, 2, 13, '/usr/libexec/fixture-portal')],
  ]);
  const marker = bus => ({ DBUS_SESSION_BUS_ADDRESS: bus, DBUS_STARTER_ADDRESS: bus, DBUS_STARTER_BUS_TYPE: 'session' });
  const environments = new Map([[4, marker(address)]]), signals = [];
  let clock = 0;
  const io = {
    self: 3,
    identity: async pid => rows.has(pid) ? { ...rows.get(pid) } : null,
    pids: async () => [...rows.keys()], uid: async pid => rows.get(pid)?.uid,
    activation: async pid => environments.get(pid), ownsAddress: async (pid, value) => pid === 2 && value === address,
    signal: async (pid, signal) => { signals.push({ pid, signal }); rows.delete(pid); },
    now: () => clock, delay: async ms => { clock += ms; },
  };
  return { rows, environments, signals, io, proc, marker };
}
test('only a new service activated on the proven private bus is signaled', async () => {
  const f = fixture();
  f.rows.set(5, f.proc(5, 0, 5, '/usr/libexec/old-service')); f.environments.set(5, f.marker(address));
  f.rows.set(6, f.proc(6, 0, 14, '/usr/libexec/host-service')); f.environments.set(6, f.marker(host));
  f.rows.set(7, f.proc(7, 3, 14, '/usr/bin/vendor-cli')); f.environments.set(7, { DBUS_SESSION_BUS_ADDRESS: address });
  f.rows.set(8, f.proc(8, 0, 14, '/usr/libexec/other-user', 1001)); f.environments.set(8, f.marker(address));
  const session = await capturePrivateSession({ address, io: f.io });
  const result = await session.cleanup();
  assert.equal(result.ok, true); assert.deepEqual(f.signals, [{ pid: 4, signal: 'SIGTERM' }]);
  for (const id of [1,2,3,5,6,7,8]) assert(f.rows.has(id), `unowned ${id}`);
});
for (const change of ['launcher', 'bus', 'address', 'unrelated-parent', 'missing-proc']) {
  test(`capture refuses ${change} identity without signals`, async () => {
    const f = fixture();
    if (change === 'launcher') f.rows.get(1).exe = '/usr/bin/bash';
    if (change === 'bus') f.rows.get(2).parent = 50;
    if (change === 'address') f.io.ownsAddress = async () => false;
    if (change === 'unrelated-parent') f.rows.get(3).parent = 50;
    if (change === 'missing-proc') f.io.pids = async () => { throw Object.assign(new Error('proc unavailable'), { code: 'EACCES' }); };
    await assert.rejects(capturePrivateSession({ address, io: f.io }));
    assert.deepEqual(f.signals, []);
  });
}
for (const change of ['start', 'exe', 'uid', 'routing', 'anchor']) {
  test(`cleanup refuses ${change} change after service recording`, async () => {
    const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
    const activation = f.io.activation; let reads = 0;
    f.io.activation = async pid => {
      if (pid === 4 && ++reads === 1) {
        // Inventory identity's second read still matches. Change the captured
        // process only after inventory finishes, when signal-time anchor checks run.
        const owns = f.io.ownsAddress; let checks = 0;
        f.io.ownsAddress = async (...args) => {
          if (++checks === 1) {
            if (['start', 'exe', 'uid'].includes(change)) f.rows.get(4)[change] = change === 'start' ? '99' : change === 'uid' ? 1001 : '/usr/bin/unrelated';
            if (change === 'routing') f.environments.set(4, f.marker(host));
            if (change === 'anchor') f.rows.get(2).start = '99';
          }
          return owns(...args);
        };
      }
      return activation(pid);
    };
    const result = await session.cleanup();
    assert.equal(result.ok, false); assert.deepEqual(f.signals, []); assert(f.rows.has(4));
  });
}
test('an uncooperative owned service gets bounded escalation and incomplete cleanup', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  f.io.signal = async (pid, signal) => f.signals.push({ pid, signal });
  const result = await session.cleanup();
  assert.equal(result.ok, false);
  assert.deepEqual(f.signals, [{ pid: 4, signal: 'SIGTERM' }, { pid: 4, signal: 'SIGKILL' }]);
  assert(f.io.now() <= 1520);
});
test('new activation after cleanup fails without an unbounded kill scan', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  const signal = f.io.signal;
  f.io.signal = async (...args) => { await signal(...args); f.rows.set(5, f.proc(5, 2, 15, '/usr/libexec/late-service')); f.environments.set(5, f.marker(address)); };
  const result = await session.cleanup();
  assert.equal(result.ok, false); assert.deepEqual(f.signals, [{ pid: 4, signal: 'SIGTERM' }]); assert(f.rows.has(5));
});
test('disappeared service is already gone and not signaled', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  f.rows.delete(4);
  assert.equal((await session.cleanup()).ok, true); assert.deepEqual(f.signals, []);
});
for (const code of ['EACCES', 'EPERM']) {
  for (const after of ['missing', 'Z', 'X']) {
    test(`activation ${code} after candidate exits to ${after} does not obstruct owned cleanup`, async () => {
      const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
      f.rows.set(5, f.proc(5, 0, 14, '/fixture/unrelated'));
      let basicReads = 0, activationReads = 0;
      f.io.basic = async pid => { if (pid === 5) basicReads++; return f.io.identity(pid); };
      const activation = f.io.activation;
      f.io.activation = async pid => {
        if (pid !== 5) return activation(pid);
        activationReads++;
        assert.equal(basicReads, 1, 'the candidate was observed live before its environment read');
        // Deterministic kernel-exit boundary: no timing or process sleeps.
        if (after === 'missing') f.rows.delete(5);
        else f.rows.get(5).state = after;
        throw Object.assign(new Error('fixture environment denied'), { code });
      };
      const result = await session.cleanup();
      assert.equal(result.ok, true, JSON.stringify(result.failures));
      assert(basicReads >= 2, 'a fresh basic identity must establish exit');
      assert.equal(activationReads, 1, 'permission failures are not retried');
      assert.deepEqual(result.services.map(service => service.pid), [4]);
      assert.deepEqual(f.signals, [{ pid: 4, signal: 'SIGTERM' }], 'the vanished or terminal candidate never acquires signal authority');
    });
  }
  for (const after of ['live', 'changed-start-Z', 'changed-uid-X', 'changed-pid-Z', 'recheck-error']) {
    test(`activation ${code} with ${after} still fails before any signal`, async () => {
      const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
      f.rows.set(5, f.proc(5, 0, 14, '/fixture/unrelated'));
      let denied = false;
      f.io.basic = async pid => {
        if (pid === 5 && denied && after === 'recheck-error') {
          throw Object.assign(new Error('fixture recheck denied'), { code: 'EACCES' });
        }
        return f.io.identity(pid);
      };
      const activation = f.io.activation;
      f.io.activation = async pid => {
        if (pid !== 5) return activation(pid);
        denied = true;
        if (after === 'changed-start-Z') Object.assign(f.rows.get(5), { start: '99', state: 'Z' });
        if (after === 'changed-uid-X') Object.assign(f.rows.get(5), { uid: 1001, state: 'X' });
        if (after === 'changed-pid-Z') Object.assign(f.rows.get(5), { pid: 99, state: 'Z' });
        throw Object.assign(new Error('fixture environment denied'), { code });
      };
      const result = await session.cleanup();
      assert.equal(result.ok, false);
      assert.match(result.failures.join(' '), /fixture (environment|recheck) denied/);
      assert.deepEqual(f.signals, [], 'one unverified candidate invalidates the full inventory, including the readable owned service');
      assert(f.rows.has(4));
    });
  }
}
test('activation errors other than permission denial remain fatal even if the candidate exits', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  f.rows.set(5, f.proc(5, 0, 14, '/fixture/unrelated'));
  let basicReads = 0;
  f.io.basic = async pid => { if (pid === 5) basicReads++; return f.io.identity(pid); };
  const activation = f.io.activation;
  f.io.activation = async pid => {
    if (pid !== 5) return activation(pid);
    f.rows.delete(5);
    throw Object.assign(new Error('fixture environment IO failure'), { code: 'EIO' });
  };
  const result = await session.cleanup();
  assert.equal(result.ok, false); assert.match(result.failures.join(' '), /fixture environment IO failure/);
  assert.equal(basicReads, 1); assert.deepEqual(f.signals, []);
});
for (const field of ['start', 'exe']) {
  test(`signal-time ${field} mutation during routing read is refused`, async () => {
    const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
    const activation = f.io.activation; let reads = 0;
    f.io.activation = async pid => {
      if (pid === 4 && ++reads === 2) f.rows.get(4)[field] = field === 'start' ? '99' : '/usr/bin/replacement';
      return activation(pid);
    };
    const result = await session.cleanup();
    assert.equal(result.ok, false); assert.deepEqual(f.signals, []);
  });
}
test('slow final inventory cannot report success beyond the fixed cleanup deadline', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  let calls = 0; const list = f.io.pids;
  f.io.pids = async () => { if (++calls === 2) await f.io.delay(6000); return list(); };
  const result = await session.cleanup();
  assert.equal(result.ok, false); assert.match(result.failures.join(' '), /deadline/);
});
test('pending IO cannot keep cleanup alive indefinitely or signal after the deadline', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  let pending; let clock = 0;
  f.io.now = () => clock;
  f.io.pids = () => { clock = 4990; return new Promise(resolve => { pending = resolve; }); };
  const result = await session.cleanup();
  assert.equal(result.ok, false); assert.match(result.failures.join(' '), /deadline/);
  pending([1,2,3,4]); await Promise.resolve();
  assert.deepEqual(f.signals, []);
});

const record = (pid, exe) => ({ pid, start: '13', exe, uid: process.getuid() });
function passedSession() {
  return { schema:1,identity:{ self:record(12340,'/fixture/node'),launcher:record(12341,'/usr/bin/dbus-run-session'),daemon:record(12342,'/usr/bin/dbus-daemon') },cleanup:{ok:true,services:[record(12343,'/fixture/portal')]} };
}
async function scratch(fn) {
  const parent = await mkdtemp(path.join(tmpdir(),'x11-supervisor-'));
  let report;
  try { report = await fn(parent); }
  finally {
    if (report?.runtime) await rm(report.runtime,{recursive:true,force:true});
    // Failed cases also deliberately preserve runtime; identify only paths
    // produced under this fixture's own report files, never scan global/tmp.
    const { readdir } = await import('node:fs/promises');
    for (const run of await readdir(parent)) {
      try { const value=JSON.parse(await readFile(path.join(parent,run,'result.json'),'utf8')); await rm(value.runtime,{recursive:true,force:true}); } catch {}
    }
    await rm(parent,{recursive:true,force:true});
  }
}
function commandPath(command, name) {
  return new RegExp(`'${name}=([^']+)'`).exec(command)?.[1];
}
for (const scenario of ['passed','missing-report','live-daemon','late-activation','failed-inner','nonzero','signal','held-output']) {
  test(`external X11 boundary ${scenario}`, () => scratch(async parent => {
    let seen;
    const report = await runNativeX11(['node','a probe.mjs','literal $(nope)'], {
      artifactParent: parent,
      activationInventory: async () => scenario==='late-activation' ? [record(12399,'/fixture/late-portal')] : [],
      identity: async pid => scenario==='live-daemon'&&pid===12342 ? {...record(12342,'/usr/bin/dbus-daemon'),state:'S'} : null,
      execute: async command => {
        seen=command;
        const runtime=commandPath(command,'XDG_RUNTIME_DIR');
        assert.equal((await stat(runtime)).mode & 0o777,0o700);
        if(scenario!=='missing-report') {
          const session=passedSession(); if(scenario==='failed-inner')session.cleanup.ok=false;
          await writeFile(commandPath(command,'SHADOW_NATIVE_SESSION_REPORT'),JSON.stringify(session));
        }
        if(scenario==='signal')throw new Error('Gate terminated by SIGTERM');
        if(scenario==='held-output')throw new Error('Gate output streams remained open 1000ms after process exit; verification output is incomplete');
        return scenario==='nonzero'?7:0;
      },
    });
    assert.match(seen, /'GDK_BACKEND=x11'/); assert.match(seen, /'XDG_SESSION_TYPE=x11'/);
    assert.match(seen, /'a probe.mjs' 'literal \$\(nope\)'$/);
    assert.doesNotMatch(seen, /'(?:HOME|XDG_CONFIG_HOME|XDG_DATA_HOME|XDG_STATE_HOME|XDG_CACHE_HOME)=/);
    assert.equal(report.ok,scenario==='passed');
    if(scenario==='passed'){assert.equal(report.runtime_removed,true);assert.equal(report.external_boundary.length,4);}
    if(scenario==='nonzero')assert.equal(report.command_exit,7);
    if(scenario==='signal')assert.equal(report.signal,'SIGTERM');
    if(scenario==='held-output'){assert.equal(report.output_complete,false);assert.equal(report.external_boundary.length,4);}
    return report;
  }));
}

test('actual isolated D-Bus activation and external wrapper exit without a display', async () => {
  const directory = await mkdtemp(path.join(tmpdir(), 'native-session-fixture-'));
  const execute = promisify(execFile);
  const helperUrl = new URL('./native-test-session.mjs', import.meta.url).href;
  const wrapper = new URL('./run-native-x11.mjs', import.meta.url).pathname;
  const quote = value => `'${value.replaceAll("'", "'\\''")}'`;
  let runtime;
  try {
    await mkdir(path.join(directory, 'services')); await mkdir(path.join(directory, 'bin'));
    await writeFile(path.join(directory, 'service.mjs'), `import {writeFileSync,existsSync} from 'node:fs';
const root=process.argv[2];
writeFileSync(root+'/ready',String(process.pid));
const deadline=Date.now()+10000;
const timer=setInterval(()=>{if(existsSync(root+'/release')||Date.now()>=deadline)clearInterval(timer);},20);\n`);
    await writeFile(path.join(directory, 'services/org.shadowcode.NativeFixture.service'), `[D-BUS Service]\nName=org.shadowcode.NativeFixture\nExec=${process.execPath} ${directory}/service.mjs ${directory}\n`);
    await writeFile(path.join(directory, 'bus.conf'), `<busconfig><type>session</type><listen>unix:tmpdir=/tmp</listen><auth>EXTERNAL</auth><servicedir>${directory}/services</servicedir><policy context="default"><allow own="*"/><allow send_destination="*"/><allow receive_sender="*"/></policy><limit name="service_start_timeout">2000</limit></busconfig>`);
    // Intercept Xvfb only. The production wrapper still creates its private
    // runtime and executes a real dbus-run-session with only our fake service.
    await writeFile(path.join(directory, 'bin/xvfb-run'), '#!/bin/bash\nset -euo pipefail\n[[ "$1" = -a && "$2" = -s && "$3" = "-screen 0 1440x1100x24 -extension GLX" ]]\nshift 3\nexec "$@"\n');
    await writeFile(path.join(directory, 'bin/dbus-run-session'), `#!/bin/bash\nexec /usr/bin/dbus-run-session --config-file ${quote(path.join(directory,'bus.conf'))} "$@"\n`);
    for (const file of ['xvfb-run','dbus-run-session']) await chmod(path.join(directory,'bin',file),0o755);
    await writeFile(path.join(directory, 'harness.mjs'), `import assert from 'node:assert/strict';
import {readFile,writeFile} from 'node:fs/promises';
import {spawn} from 'node:child_process';
import {capturePrivateSession,writePrivateSessionReport,linuxProc} from ${JSON.stringify(helperUrl)};
const root=process.argv[2], session=await capturePrivateSession();
let cleanup;
try {
 const request=spawn('/usr/bin/dbus-send',['--session','--print-reply','--reply-timeout=1500','--dest=org.freedesktop.DBus','/org/freedesktop/DBus','org.freedesktop.DBus.StartServiceByName','string:org.shadowcode.NativeFixture','uint32:0'],{stdio:'ignore'});
 const finished=new Promise(resolve=>request.once('exit',resolve));
 const deadline=Date.now()+3000; let pid;
 while(!pid&&Date.now()<deadline){try{pid=Number(await readFile(root+'/ready','utf8'));}catch(e){if(e.code!=='ENOENT')throw e;}if(!pid)await new Promise(r=>setTimeout(r,10));}
 assert(pid,'Activation must reach the explicit ready barrier');
 cleanup=await session.cleanup();
 assert.equal(cleanup.ok,true,JSON.stringify(cleanup));
 assert.deepEqual(cleanup.services.map(p=>p.pid),[pid]);
 assert.equal(await linuxProc.identity(pid),null);
 await finished;
} finally {
 await writeFile(root+'/release','release');
 await writePrivateSessionReport(session,cleanup||{ok:false,services:[],failures:['Fixture stopped before cleanup']});
}\n`);
    const env = { ...process.env, PATH:`${directory}/bin:${process.env.PATH}`, SHADOW_X11_ARTIFACTS:path.join(directory,'artifacts') };
    const result = await execute(process.execPath,[wrapper,process.execPath,path.join(directory,'harness.mjs'),directory],{env,timeout:15000,maxBuffer:1024*1024});
    const runs=await readdir(path.join(directory,'artifacts')); assert.equal(runs.length,1);
    const report=JSON.parse(await readFile(path.join(directory,'artifacts',runs[0],'result.json'),'utf8')); runtime=report.runtime;
    assert.equal(report.ok,true,result.stdout+'\n'+result.stderr);
    assert.equal(report.output_complete,true); assert.equal(report.command_exit,0);
    assert.equal(report.external_boundary.length,4);
    assert(report.external_boundary.every(p=>p.same_live_process===false));
    assert.deepEqual(report.late_activations,[]); assert.equal(report.runtime_removed,true);
    await assert.rejects(stat(runtime),{code:'ENOENT'});
  } finally {
    await writeFile(path.join(directory,'release'),'release');
    if(runtime)await rm(runtime,{recursive:true,force:true});
    await rm(directory,{recursive:true,force:true});
  }
});

test('post-bus inventory detects late activation without live anchors or signals', async () => {
  const f=fixture(); const session=await capturePrivateSession({address,io:f.io});
  f.rows.delete(1);f.rows.delete(2);f.rows.delete(3);
  const found=await inspectPrivateActivations(session.identity,{io:f.io});
  assert.deepEqual(found.map(p=>p.pid),[4]);assert.deepEqual(f.signals,[]);
});

test('both required CI paths install D-Bus before session fixtures and preserve X11 wrapper qualification', async () => {
  const { GATES } = await import('./native-release-verification.mjs');
  assert(GATES['release-tests'].script.includes('scripts/test-native-test-session.mjs'));
  assert.equal(GATES['native-window'].script.split('node scripts/run-native-x11.mjs node scripts/test-native-').length,3);
  assert(GATES['packaged-behavior'].script.includes('node scripts/run-native-x11.mjs node scripts/test-native-desktop.mjs'));
  assert(GATES['packaged-behavior'].script.includes('node scripts/run-native-x11.mjs node scripts/test-native-markdown.mjs'));
  assert(GATES['native-endurance'].script.includes('node scripts/run-native-x11.mjs node scripts/test-native-desktop.mjs'));
  for(const name of ['native.yml','release.yml']) {
    const text=await readFile(new URL(`../.github/workflows/${name}`,import.meta.url),'utf8');
    const fixtures=text.indexOf(name==='native.yml'?'run: node --test scripts/test-native-release.mjs':'run: node scripts/native-release-verification.mjs run release-tests');
    assert(fixtures>0);
    const dependencies=text.indexOf('dbus-daemon dbus-bin'); assert(dependencies>=0&&dependencies<fixtures);
    assert(text.includes('artifacts/native-x11'));
    assert(text.includes('run: node scripts/native-release-verification.mjs run native-endurance'));
    assert(text.includes('artifacts/native-endurance'));
    if(name==='native.yml') {
      assert(text.includes('scripts/test-native-test-session.mjs'));
      assert.equal(text.split('run: node scripts/run-native-x11.mjs node scripts/test-native-').length,5);
      assert(text.includes("'scripts/*native*.mjs'"));
    }
  }
});


for (const phase of ['capture', 'inventory']) {
  test(`${phase} discovery admits at most eight read-only candidates and scans every PID in stable order`, async () => {
    const f = fixture();
    const session = await capturePrivateSession({ address, io: f.io });
    const ids = Array.from({ length: 24 }, (_, i) => 123 - i);
    for (const pid of ids) {
      f.rows.set(pid, f.proc(pid, 0, 13, '/fixture/service'));
      f.environments.set(pid, f.marker(address));
    }
    f.io.pids = async () => phase === 'capture' ? [...ids, 1, 2, 3] : ids;
    let release, reached, active = 0, maximum = 0;
    const barrier = new Promise(resolve => { release = resolve; });
    const eighth = new Promise(resolve => { reached = resolve; });
    const seen = [];
    f.io.uid = async pid => {
      seen.push(pid); active++; maximum = Math.max(maximum, active);
      if (seen.length === 8) reached();
      await barrier;
      active--;
      return f.rows.get(pid)?.uid;
    };
    const pending = phase === 'capture'
      ? capturePrivateSession({ address, io: f.io })
      : inspectPrivateActivations(session.identity, { io: f.io });
    let timer, admitted = false, result;
    try {
      admitted = await Promise.race([eighth.then(() => true), new Promise(resolve => { timer = setTimeout(() => resolve(false), 1000); })]);
      assert.equal(seen.length, 8, 'fixed pool admits eight blocked readers, not one serial reader or an unbounded inventory');
    } finally { clearTimeout(timer); release(); result = await pending; }
    assert(admitted, 'eight reads must reach the explicit admission barrier');
    assert.equal(maximum, 8);
    assert.deepEqual(seen, phase === 'capture' ? [...ids, 1, 2, 3] : ids);
    if (phase === 'capture') assert.equal(result.identity.daemon.pid, 2);
    else assert.deepEqual(result.map(value => value.pid), ids, 'completion scheduling cannot reorder the result inventory');
    assert.deepEqual(f.signals, []);
  });
}

test('discovery propagates an unreadable candidate and retires every admitted reader before rejecting', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  const ids = Array.from({ length: 24 }, (_, i) => 100 + i);
  f.io.pids = async () => ids;
  let release, failureReached, active = 0, maximum = 0;
  const barrier = new Promise(resolve => { release = resolve; });
  const failed = new Promise(resolve => { failureReached = resolve; });
  const seen = [];
  f.io.uid = async pid => {
    seen.push(pid); active++; maximum = Math.max(maximum, active);
    try {
      if (pid === 100) { failureReached(); throw Object.assign(new Error('fixture proc denied'), { code: 'EACCES' }); }
      await barrier; return 1001;
    } finally { active--; }
  };
  let settled = false;
  const pending = inspectPrivateActivations(session.identity, { io: f.io }).then(
    result => ({ result }), error => ({ error }),
  ).finally(() => { settled = true; });
  await failed;
  // Flush the rejecting UID's microtasks while its other admitted readers
  // stay blocked. No wall-clock sleeps or real process operations are used.
  for (let i = 0; i < 10; i++) await Promise.resolve();
  const beforeRelease = { settled, active, seen: seen.length };
  release(); const outcome = await pending;
  assert.equal(beforeRelease.settled, false, 'read-only work must be joined before rejection');
  assert.equal(beforeRelease.active, 7);
  assert.equal(beforeRelease.seen, 8);
  assert.equal(active, 0); assert.equal(maximum, 7);
  assert.match(outcome.error.message, /fixture proc denied/);
  assert.deepEqual(seen, ids.slice(0, 8), 'no new readers admitted after a discovery failure');
  assert.deepEqual(f.signals, []);
});

test('discovery still refuses more than 32 matching private activations without signaling', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  const ids = Array.from({ length: 48 }, (_, i) => 100 + i);
  for (const pid of ids) {
    f.rows.set(pid, f.proc(pid, 0, 13, '/fixture/service'));
    f.environments.set(pid, f.marker(address));
  }
  f.io.pids = async () => ids;
  await assert.rejects(inspectPrivateActivations(session.identity, { io: f.io }), /service count exceeds/);
  assert.deepEqual(f.signals, []);
});

test('discovery preserves PID inventory order when matching reads finish in reverse', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  const ids = Array.from({ length: 8 }, (_, i) => 100 + i);
  for (const pid of ids) {
    f.rows.set(pid, f.proc(pid, 0, 13, '/fixture/service'));
    f.environments.set(pid, f.marker(address));
  }
  f.io.pids = async () => ids;
  let reached; const allAdmitted = new Promise(resolve => { reached = resolve; });
  const releases = new Map(), completions = new Map(), completionOrder = [];
  f.io.activation = pid => new Promise(resolve => { releases.set(pid, resolve); if (releases.size === ids.length) reached(); });
  const counts = new Map(), identity = f.io.identity;
  const done = new Map(ids.map(pid => [pid, new Promise(resolve => { completions.set(pid, resolve); })]));
  f.io.identity = async pid => {
    const count = (counts.get(pid) || 0) + 1; counts.set(pid, count);
    if (count === 3) { completionOrder.push(pid); completions.get(pid)(); }
    return identity(pid);
  };
  const pending = inspectPrivateActivations(session.identity, { io: f.io });
  let timer;
  try {
    await Promise.race([allAdmitted, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('readers did not reach activation barrier')), 1000); })]);
    clearTimeout(timer);
    for (const pid of [...ids].reverse()) { releases.get(pid)(f.marker(address)); await done.get(pid); }
    const found = await pending;
    assert.deepEqual(completionOrder, [...ids].reverse());
    assert.deepEqual(found.map(value => value.pid), ids);
    assert.deepEqual(f.signals, []);
  } finally { clearTimeout(timer); for (const [pid, release] of releases) release(f.marker(address)); }
});

test('concurrent discovery deadline joins eight readers and late IO cannot signal or admit more work', async () => {
  const f = fixture(); const session = await capturePrivateSession({ address, io: f.io });
  let clock = 0;
  f.io.now = () => clock;
  const ids = Array.from({ length: 24 }, (_, i) => 100 + i);
  for (const pid of ids) f.rows.set(pid, f.proc(pid, 0, 13, '/fixture/service'));
  f.io.pids = async () => { clock = 4990; return ids; };
  const seen = [], releases = [];
  f.io.uid = pid => new Promise(resolve => { seen.push(pid); releases.push(resolve); });
  let deeperReads = 0;
  f.io.basic = async pid => { deeperReads++; return f.io.identity(pid); };
  const result = await session.cleanup();
  assert.equal(result.ok, false); assert.match(result.failures.join(' '), /deadline/);
  assert.deepEqual(seen, ids.slice(0, 8));
  assert.equal(deeperReads, 0); assert.deepEqual(f.signals, []);
  for (const release of releases) release(1000);
  await new Promise(resolve => setImmediate(resolve));
  assert.deepEqual(seen, ids.slice(0, 8), 'expired workers cannot admit queued candidates');
  assert.equal(deeperReads, 0, 'late UID reads cannot continue to candidate identity checks');
  assert.deepEqual(f.signals, [], 'late results cannot become partial signal authority');
});

test('private Wayland compositor command: X11 backend inside a private Xvfb, or headless', () => {
  const x11 = compositorCommand({ weston: '/opt/weston', backend: 'x11', runtime: '/run/private', config: '/run/private/weston.ini' });
  assert.equal(x11.exe, 'xvfb-run');
  assert.deepEqual(x11.args.slice(0, 4), ['-a', '-s', '-screen 0 1600x1100x24 -extension GLX', '/opt/weston']);
  assert(x11.args.includes('--backend=x11') && x11.args.includes('--renderer=pixman') && x11.args.includes('--socket=wayland-shadowcode-test'));
  const headless = compositorCommand({ weston: '/opt/weston', backend: 'headless', runtime: '/run/private', config: '/run/private/weston.ini' });
  assert.equal(headless.exe, '/opt/weston');
  assert(headless.args.includes('--backend=headless'));
  assert.throws(() => compositorCommand({ weston: 'weston', backend: 'drm', runtime: '/r', config: '/r/w.ini' }), /x11 or headless/);
});

/** A stand-in compositor: listens on the socket path the runner waits for. */
function fakeCompositor(scenario) {
  const calls = { env: null, stopped: false };
  const launch = ({ env }) => {
    calls.env = env;
    const handle = { child: { pid: 2 ** 22 + 17 }, exited: false, code: null, signal: null, output: 'fixture compositor\n' };
    const socket = path.join(env.XDG_RUNTIME_DIR, 'wayland-shadowcode-test');
    let server;
    if (scenario === 'compositor-dies') { handle.exited = true; handle.code = 1; }
    else server = createServer().listen(socket);
    handle.stop = async () => { calls.stopped = true; await new Promise(resolve => server ? server.close(resolve) : resolve()); handle.exited = true; return true; };
    return handle;
  };
  return { calls, launch };
}
for (const scenario of ['passed', 'compositor-dies', 'failed-inner', 'nonzero']) {
  test(`private Wayland session ${scenario}`, () => scratch(async parent => {
    let seen;
    const fake = fakeCompositor(scenario);
    const report = await runNativeWayland(['node', 'a probe.mjs'], {
      artifactParent: parent, weston: '/fixture/weston', backend: 'x11',
      libraryPath: '/fixture/lib', moduleMap: 'x11-backend.so=/fixture/x11.so',
      compositor: fake.launch, identity: async () => null, activationInventory: async () => [],
      execute: async command => {
        seen = command;
        const runtime = commandPath(command, 'XDG_RUNTIME_DIR');
        assert.equal((await stat(runtime)).mode & 0o777, 0o700);
        const session = passedSession(); if (scenario === 'failed-inner') session.cleanup.ok = false;
        await writeFile(commandPath(command, 'SHADOW_NATIVE_SESSION_REPORT'), JSON.stringify(session));
        return scenario === 'nonzero' ? 3 : 0;
      },
    });
    if (scenario === 'compositor-dies') {
      assert.equal(report.ok, false); assert.equal(seen, undefined, 'the command never runs without a compositor');
      assert.match(report.failures.join(' '), /compositor exited before its socket appeared/);
      return report;
    }
    // The app reaches only the private compositor: Wayland declared, the
    // host display and bus removed, the unpacked Weston library path not leaked.
    for (const want of [/'GDK_BACKEND=wayland'/, /'XDG_SESSION_TYPE=wayland'/, /'WAYLAND_DISPLAY=wayland-shadowcode-test'/, /'SHADOW_NATIVE_DISPLAY=wayland'/, /'-u' 'DISPLAY'/, /'-u' 'DBUS_SESSION_BUS_ADDRESS'/, /'-u' 'LD_LIBRARY_PATH'/, /'dbus-run-session' '--' 'node' 'a probe.mjs'$/])
      assert.match(seen, want);
    assert.equal(fake.calls.env.LD_LIBRARY_PATH, '/fixture/lib');
    assert.equal(fake.calls.env.WESTON_MODULE_MAP, 'x11-backend.so=/fixture/x11.so');
    assert.equal(fake.calls.env.DISPLAY, undefined, 'Weston gets its X server from xvfb-run, never the host display');
    assert.equal(fake.calls.stopped, true, 'the compositor is stopped in every outcome');
    assert.equal(report.ok, scenario === 'passed');
    if (scenario === 'passed') { assert.equal(report.runtime_removed, true); assert.equal(report.external_boundary.length, 4); }
    if (scenario === 'nonzero') assert.equal(report.command_exit, 3);
    return report;
  }));
}
