// Linux native test-session ownership, not a general process cleanup API.
import assert from 'node:assert/strict';
import { readFile, readlink, readdir, stat, writeFile } from 'node:fs/promises';

const gone = error => ['ENOENT', 'ESRCH'].includes(error.code);
const alive = value => value && !['Z', 'X'].includes(value.state);
const same = (a, b) => alive(b) && a.pid === b.pid && a.start === b.start && a.exe === b.exe && a.uid === b.uid;
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
function addressPath(address) {
  assert.match(address, /^unix:(?:path|abstract)=[^;,]+,guid=[a-f0-9]{32}$/, 'Expected one private UNIX bus address including GUID');
  const [transport] = address.split(',');
  const abstract = transport.startsWith('unix:abstract=');
  const value = decodeURIComponent(transport.slice(transport.indexOf('=') + 1));
  assert(value && !value.includes('\0'), 'Invalid private UNIX bus path');
  return (abstract ? '@' : '') + value;
}
export const linuxProc = {
  self: process.pid,
  async basic(pid) {
    try {
      const [raw, status] = await Promise.all([
        readFile(`/proc/${pid}/stat`, 'utf8'), readFile(`/proc/${pid}/status`, 'utf8'),
      ]);
      const fields = raw.slice(raw.lastIndexOf(') ') + 2).split(' ');
      const ids = /^Uid:\s+(\d+)\s+(\d+)\s+(\d+)\s+(\d+)/m.exec(status);
      assert(ids && ids.slice(1).every(id => id === ids[1]), 'Unexpected process credential transition');
      return { pid, parent: Number(fields[1]), start: fields[19], state: fields[0], uid: Number(ids[1]) };
    } catch (error) { if (gone(error)) return null; throw error; }
  },
  async identity(pid) {
    const basic = await this.basic(pid);
    if (!basic) return null;
    try { return { ...basic, exe: await readlink(`/proc/${pid}/exe`) }; }
    catch (error) { if (gone(error)) return null; throw error; }
  },
  async pids() {
    const list = (await readdir('/proc')).filter(name => /^\d+$/.test(name));
    assert(list.length <= 32768, 'Process inventory exceeds test cleanup limit');
    return list.map(Number);
  },
  async uid(pid) { try { return (await stat(`/proc/${pid}`)).uid; } catch (error) { if (gone(error)) return null; throw error; } },
  async activation(pid) {
    try {
      const bytes = await readFile(`/proc/${pid}/environ`);
      assert(bytes.length <= 1024 * 1024, 'Process environment exceeds test cleanup limit');
      // Retain only routing markers, never diagnostic copies of full environments.
      const names = new Set(['DBUS_SESSION_BUS_ADDRESS', 'DBUS_STARTER_ADDRESS', 'DBUS_STARTER_BUS_TYPE']);
      return Object.fromEntries(bytes.toString('utf8').split('\0').flatMap(item => {
        const split = item.indexOf('='); const name = item.slice(0, split);
        return split >= 0 && names.has(name) ? [[name, item.slice(split + 1)]] : [];
      }));
    } catch (error) { if (gone(error)) return null; throw error; }
  },
  async ownsAddress(pid, address) {
    const wanted = addressPath(address);
    const rows = (await readFile('/proc/net/unix', 'utf8')).split('\n').flatMap(line => {
      const row = /^\s*\S+:\s+\S+\s+\S+\s+(\S+)\s+(\S+)\s+(\S+)\s+(\d+)(?:\s+(.*))?$/.exec(line);
      return row && row[1] === '00010000' && row[2] === '0001' && row[5] === wanted ? [row[4]] : [];
    });
    if (rows.length !== 1) return false;
    const links = await Promise.all((await readdir(`/proc/${pid}/fd`)).map(fd => readlink(`/proc/${pid}/fd/${fd}`).catch(error => {
      if (gone(error)) return ''; throw error;
    })));
    return links.includes(`socket:[${rows[0]}]`);
  },
  signal(pid, signal) { process.kill(pid, signal); },
  delay,
  now: Date.now,
};

export async function capturePrivateSession({ address = process.env.DBUS_SESSION_BUS_ADDRESS, io = linuxProc,
  launcherExe = '/usr/bin/dbus-run-session', daemonExe = '/usr/bin/dbus-daemon' } = {}) {
  addressPath(address || '');
  const self = await io.identity(io.self);
  assert(alive(self), 'Cannot identify test process');
  const launcher = await io.identity(self.parent);
  assert(alive(launcher) && launcher.exe === launcherExe && launcher.uid === self.uid, 'Test must be a direct child of the expected dbus-run-session');
  const basic = pid => io.basic ? io.basic(pid) : io.identity(pid);
  const candidates = [];
  for (const pid of await io.pids()) {
    if (await io.uid(pid) !== self.uid) continue;
    const info = await basic(pid);
    if (!alive(info) || info.parent !== launcher.pid || BigInt(info.start) < BigInt(launcher.start) || BigInt(info.start) > BigInt(self.start)) continue;
    const value = await io.identity(pid);
    if (alive(value) && value.exe === daemonExe && await io.ownsAddress(pid, address)) candidates.push(value);
  }
  assert.equal(candidates.length, 1, 'Private bus address is not owned by exactly one child of this test session');
  const daemon = candidates[0];
  async function anchored(client = io) {
    assert(same(launcher, await client.identity(launcher.pid)), 'Private session launcher identity changed');
    assert(same(daemon, await client.identity(daemon.pid)) && await client.ownsAddress(daemon.pid, address)
      && same(daemon, await client.identity(daemon.pid)) && same(launcher, await client.identity(launcher.pid)), 'Private session bus identity changed');
  }
  await anchored();
  const activated = env => env?.DBUS_SESSION_BUS_ADDRESS === address && env.DBUS_STARTER_ADDRESS === address && env.DBUS_STARTER_BUS_TYPE === 'session';
  async function inventory(client = io) {
    await anchored(client);
    return inspectPrivateActivations({ address, self, launcher, daemon }, { io: client });
  }
  return {
    identity: { address, self, launcher, daemon },
    async cleanup() {
      const services = [], failures = [];
      const deadline = io.now() + 5000;
      const withinDeadline = () => assert(io.now() < deadline, 'Private service cleanup exceeded its fixed deadline');
      const client = { ...io };
      for (const name of ['identity', 'basic', 'pids', 'uid', 'activation', 'ownsAddress', 'signal', 'delay']) {
        if (!io[name]) continue;
        client[name] = async (...args) => {
          withinDeadline();
          let timer;
          try {
            const result = await Promise.race([
              io[name](...args),
              new Promise((_, reject) => { timer = setTimeout(() => reject(new Error('Private service cleanup IO exceeded its fixed deadline')), Math.max(1, deadline - io.now())); }),
            ]);
            withinDeadline();
            return result;
          } finally { clearTimeout(timer); }
        };
      }
      try {
        const owned = await inventory(client);
        for (const identity of owned) {
          const record = { ...identity, signals: [] }; services.push(record);
          for (const signal of ['SIGTERM', 'SIGKILL']) {
            withinDeadline();
            await anchored(client);
            const current = await client.identity(identity.pid);
            if (!alive(current)) break;
            assert(same(identity, current) && activated(await client.activation(identity.pid)), 'Refusing to signal changed or unrelated process');
            await anchored(client);
            assert(same(identity, await client.identity(identity.pid)), 'Refusing to signal a process changed during ownership validation');
            try { await client.signal(identity.pid, signal); record.signals.push(signal); }
            catch (error) { if (!gone(error)) throw error; }
            const until = io.now() + (signal === 'SIGTERM' ? 1000 : 500);
            while (io.now() < until) {
              withinDeadline();
              const live = await client.identity(identity.pid);
              if (!alive(live)) break;
              assert(same(identity, live), 'Service PID changed during cleanup');
              await client.delay(20);
            }
          }
          const last = await client.identity(identity.pid);
          assert(!alive(last), 'Activated service remains after bounded cleanup');
        }
        // No repeating kill scan: activation churn is an incomplete cleanup,
        // not authority for an unbounded stream of newly discovered signals.
        withinDeadline();
        assert.equal((await inventory(client)).length, 0, 'New private-session services appeared during cleanup');
      } catch (error) { failures.push(error.message); }
      return { ok: failures.length === 0, services, failures };
    },
  };
}

// Read-only after the bus exits too: late activated writers may have closed
// their output but still be alive. Detection is never permission to signal.
export async function inspectPrivateActivations(session, { io = linuxProc } = {}) {
  const { address, self, launcher, daemon } = session;
  addressPath(address);
  const excluded = new Set([self.pid, launcher.pid, daemon.pid]);
  const owned = [];
  for (const pid of await io.pids()) {
    if (excluded.has(pid) || await io.uid(pid) !== self.uid) continue;
    const info = await (io.basic ? io.basic(pid) : io.identity(pid));
    if (!alive(info) || BigInt(info.start) < BigInt(daemon.start)) continue;
    const env = await io.activation(pid);
    if (!(env?.DBUS_SESSION_BUS_ADDRESS === address && env.DBUS_STARTER_ADDRESS === address && env.DBUS_STARTER_BUS_TYPE === 'session')) continue;
    const value = await io.identity(pid);
    if (!alive(value)) continue;
    assert(info.start === value.start && info.uid === value.uid && same(value, await io.identity(pid)), 'Activated service identity changed while recording');
    owned.push(value);
    assert(owned.length <= 32, 'Private activated service count exceeds test cleanup limit');
  }
  return owned;
}

export async function writePrivateSessionReport(session, cleanup) {
  const file = process.env.SHADOW_NATIVE_SESSION_REPORT;
  if (!file) return;
  assert(file.startsWith('/'), 'Private session report path must be absolute');
  await writeFile(file, JSON.stringify({ schema: 1, identity: session.identity, cleanup }, null, 2) + '\n');
}
