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

// Only discovery reads overlap. Keep the full inventory and its input order;
// anchors, identity revalidation and every signal remain sequential below.
async function discoverProcesses(pids, inspect) {
  const results = new Array(pids.length);
  let next = 0, failure;
  await Promise.all(Array.from({ length: Math.min(8, pids.length) }, async () => {
    while (!failure) {
      const index = next++;
      if (index >= pids.length) return;
      try { results[index] = await inspect(pids[index]); }
      catch (error) { failure ??= { error }; }
    }
  }));
  // Join already-admitted reads before reporting an error. A failure never
  // becomes a partial inventory or permits subsequent cleanup signals.
  if (failure) throw failure.error;
  return results.filter(Boolean);
}

export async function capturePrivateSession({ address = process.env.DBUS_SESSION_BUS_ADDRESS, io = linuxProc,
  launcherExe = '/usr/bin/dbus-run-session', daemonExe = '/usr/bin/dbus-daemon' } = {}) {
  addressPath(address || '');
  const self = await io.identity(io.self);
  assert(alive(self), 'Cannot identify test process');
  const launcher = await io.identity(self.parent);
  assert(alive(launcher) && launcher.exe === launcherExe && launcher.uid === self.uid, 'Test must be a direct child of the expected dbus-run-session');
  const basic = pid => io.basic ? io.basic(pid) : io.identity(pid);
  const candidates = await discoverProcesses(await io.pids(), async pid => {
    if (await io.uid(pid) !== self.uid) return;
    const info = await basic(pid);
    if (!alive(info) || info.parent !== launcher.pid || BigInt(info.start) < BigInt(launcher.start) || BigInt(info.start) > BigInt(self.start)) return;
    const value = await io.identity(pid);
    if (alive(value) && value.exe === daemonExe && await io.ownsAddress(pid, address)) return value;
  });
  assert.equal(candidates.length, 1, 'Private bus address is not owned by exactly one child of this test session');
  const daemon = candidates[0];
  async function anchored(client = io) {
    assert(same(launcher, await client.identity(launcher.pid)), 'Private session launcher identity changed');
    assert(same(daemon, await client.identity(daemon.pid)) && await client.ownsAddress(daemon.pid, address)
      && same(daemon, await client.identity(daemon.pid)) && same(launcher, await client.identity(launcher.pid)), 'Private session bus identity changed');
  }
  await anchored();
  const activated = env => env?.DBUS_SESSION_BUS_ADDRESS === address && env.DBUS_STARTER_ADDRESS === address && env.DBUS_STARTER_BUS_TYPE === 'session';
  async function inventory(client = io, ignored = []) {
    await anchored(client);
    return inspectPrivateActivations({ address, self, launcher, daemon }, { io: client, ignored });
  }
  return {
    identity: { address, self, launcher, daemon },
    async cleanup() {
      const services = [], failures = [], ignored = [];
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
        const owned = await inventory(client, ignored);
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
        assert.equal((await inventory(client, ignored)).length, 0, 'New private-session services appeared during cleanup');
      } catch (error) { failures.push(error.message); }
      // One entry per process, although both inventories may see it.
      const unrelated = [...new Map(ignored.map(entry => [`${entry.pid}:${entry.start}`, entry])).values()];
      return { ok: failures.length === 0, services, failures, ignored: unrelated };
    },
  };
}

// A process whose environment cannot be read (non-dumpable: setuid, file
// capabilities, prctl) can still be proven unrelated by where it hangs in the
// process tree. Everything the private daemon starts, including orphans the
// kernel re-parents to init or a subreaper, starts after the daemon and sits
// below a process of the daemon's own ancestor chain. So walk the candidate's
// parents up to the first shared ancestor: the candidate is unrelated only if
// the branch below that point started before the daemon. Passing through the
// daemon, a vanished or changed parent, or a too-deep chain proves nothing.
const MAX_DEPTH = 64;
// The daemon's live ancestor chain up to the root, or null when it cannot be
// read whole (after the bus and its launcher exited, for example): without
// it an orphan re-parented to init would look like an old branch.
async function daemonAncestors(io, daemon) {
  const basic = pid => io.basic ? io.basic(pid) : io.identity(pid);
  const daemonNow = await basic(daemon.pid);
  if (!alive(daemonNow) || daemonNow.start !== daemon.start) return null;
  const ancestors = new Map([[0, null]]);
  let pid = daemonNow.parent;
  for (let depth = 0; pid > 0; depth++) {
    if (depth >= MAX_DEPTH) return null;
    const info = await basic(pid);
    if (!alive(info)) return null;
    ancestors.set(pid, info.start);
    pid = info.parent;
  }
  return ancestors;
}
async function unrelatedBranch(io, candidate, daemon, ancestors) {
  if (!ancestors) return false;
  const basic = pid => io.basic ? io.basic(pid) : io.identity(pid);
  let node = candidate;
  for (let depth = 0; depth < MAX_DEPTH; depth++) {
    const parent = node.parent;
    if (parent === daemon.pid) return false;
    if (ancestors.has(parent)) {
      const start = ancestors.get(parent);
      if (start !== null) {
        const current = await basic(parent);
        if (!alive(current) || current.start !== start) return false;
      }
      return BigInt(node.start) < BigInt(daemon.start);
    }
    const next = await basic(parent);
    if (!alive(next)) return false;
    node = next;
  }
  return false;
}

// Read-only after the bus exits too: late activated writers may have closed
// their output but still be alive. Detection is never permission to signal.
// `ignored` collects live processes whose environment could not be read but
// that are proven unrelated to the private bus (see `unrelatedBranch`).
export async function inspectPrivateActivations(session, { io = linuxProc, ignored = [] } = {}) {
  const { address, self, launcher, daemon } = session;
  addressPath(address);
  const excluded = new Set([self.pid, launcher.pid, daemon.pid]);
  let matched = 0;
  let ancestors;
  return discoverProcesses(await io.pids(), async pid => {
    if (excluded.has(pid) || await io.uid(pid) !== self.uid) return;
    const info = await (io.basic ? io.basic(pid) : io.identity(pid));
    if (!alive(info) || BigInt(info.start) < BigInt(daemon.start)) return;
    let env;
    try { env = await io.activation(pid); }
    catch (error) {
      if (!['EACCES', 'EPERM'].includes(error.code)) throw error;
      // Linux may revoke environ access during exit after the live basic read.
      // Disregard only a proven exit; unreadable live or reused PIDs still fail.
      const current = await (io.basic ? io.basic(pid) : io.identity(pid));
      if (!current || (current.pid === info.pid && current.start === info.start
        && current.uid === info.uid && ['Z', 'X'].includes(current.state))) return;
      // Live and unreadable: skip it only when its branch of the process tree
      // is proven older than the private daemon, and it is still the same
      // process afterwards.
      if (current.pid === info.pid && current.start === info.start && current.uid === info.uid) {
        ancestors ??= await daemonAncestors(io, daemon);
        if (await unrelatedBranch(io, current, daemon, ancestors)) {
          const again = await (io.basic ? io.basic(pid) : io.identity(pid));
          if (again && again.pid === info.pid && again.start === info.start && again.uid === info.uid) {
            ignored.push({ pid, start: info.start, reason: `environment unreadable (${error.code}); its process branch predates the private bus` });
            return;
          }
        }
      }
      throw error;
    }
    if (!(env?.DBUS_SESSION_BUS_ADDRESS === address && env.DBUS_STARTER_ADDRESS === address && env.DBUS_STARTER_BUS_TYPE === 'session')) return;
    const value = await io.identity(pid);
    if (!alive(value)) return;
    assert(info.start === value.start && info.uid === value.uid && same(value, await io.identity(pid)), 'Activated service identity changed while recording');
    assert(++matched <= 32, 'Private activated service count exceeds test cleanup limit');
    return value;
  });
}

export async function writePrivateSessionReport(session, cleanup) {
  const file = process.env.SHADOW_NATIVE_SESSION_REPORT;
  if (!file) return;
  assert(file.startsWith('/'), 'Private session report path must be absolute');
  await writeFile(file, JSON.stringify({ schema: 1, identity: session.identity, cleanup }, null, 2) + '\n');
}
