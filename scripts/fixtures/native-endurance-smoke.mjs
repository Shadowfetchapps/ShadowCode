// UI-03: 100 sequential real native-window tasks, including command approvals.
// The caller owns the isolated X11 window/profile and process cleanup.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { readFile, readlink, readdir, writeFile } from "node:fs/promises";
import path from "node:path";

const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
const ELEMENT = "element-6066-11e4-a52e-4f735466cecf";
const composer = 'textarea[aria-label="Message ShadowCode"]';
const dialog = '[role="dialog"][aria-label="Run a check"]';
const summary = 'section[aria-label="Task summary"]';
const terminal = new Set(["completed", "failed", "cancelled", "limit_reached"]);

async function executableHash(pid) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(`/proc/${pid}/exe`)) hash.update(chunk);
  return hash.digest("hex");
}

async function threadInventory(pid) {
  for (let attempt = 0; attempt < 5; attempt++) {
    const before = (await readdir(`/proc/${pid}/task`)).sort();
    const records = await Promise.all(before.map(async tid => {
      try {
        const raw = await readFile(`/proc/${pid}/task/${tid}/stat`, "utf8");
        const end = raw.lastIndexOf(") ");
        const fields = raw.slice(end + 2).split(" ");
        return { tid: Number(tid), name: raw.slice(raw.indexOf("(") + 1, end), start: fields[19], state: fields[0] };
      } catch (error) {
        if (error.code === "ENOENT" || error.code === "ESRCH") return null;
        throw error;
      }
    }));
    const after = (await readdir(`/proc/${pid}/task`)).sort();
    if (records.every(Boolean) && before.join(",") === after.join(",")) return records;
    await delay(50);
  }
  throw new Error(`Owned process ${pid} has no stable thread inventory after five reads`);
}

async function processSample(pid) {
  const before = await readFile(`/proc/${pid}/stat`, "utf8");
  const fields = before.slice(before.lastIndexOf(") ") + 2).split(" ");
  assert.ok(!["Z", "X"].includes(fields[0]), `Owned process ${pid} is live`);
  const [status, rollup, fds, executable, threadIdentities] = await Promise.all([
    readFile(`/proc/${pid}/status`, "utf8"), readFile(`/proc/${pid}/smaps_rollup`, "utf8"),
    readdir(`/proc/${pid}/fd`), readlink(`/proc/${pid}/exe`), threadInventory(pid),
  ]);
  const after = await readFile(`/proc/${pid}/stat`, "utf8");
  assert.equal(after.slice(after.lastIndexOf(") ") + 2).split(" ")[19], fields[19], "PID identity did not change during sample");
  const number = (text, name) => {
    const value = new RegExp(`^${name}:\\s+(\\d+)`, "m").exec(text);
    assert.ok(value, `${name} available for owned process ${pid}`);
    return Number(value[1]);
  };
  return { pid, start: fields[19], executable, rssKiB: number(status, "VmRSS"),
    pssKiB: number(rollup, "Pss"), threads: threadIdentities.length,
    reportedThreads: number(status, "Threads"), threadIdentities, fds: fds.length };
}

// Converge on per-process named thread pools, not a rising aggregate ceiling.
// Thread identities are retained for diagnosis; replaceable workers with the
// same name may rotate without being mistaken for accumulating live threads.
function warmupConverged(samples, required) {
  if (samples.length < required) return false;
  const recent = samples.slice(-required);
  const identities = point => point.processes.map(item => `${item.pid}:${item.start}`).sort().join(",");
  if (!recent.every(point => identities(point) === identities(recent[0]))) return false;
  for (const process of recent[0].processes) {
    const pools = recent.map(point => {
      const current = point.processes.find(item => item.pid === process.pid);
      const counts = new Map();
      for (const thread of current.threadIdentities) counts.set(thread.name, (counts.get(thread.name) || 0) + 1);
      return counts;
    });
    for (const name of new Set(pools.flatMap(pool => [...pool.keys()]))) {
      const counts = pools.map(pool => pool.get(name) || 0);
      if (Math.max(...counts) - Math.min(...counts) > 1 || counts.at(-1) > counts[0]) return false;
    }
  }
  return true;
}

// Read the actual Tauri callback and event-plugin registries. No replacement of
// IPC, promises, event handlers, or GC is used to make resource counts pass.
async function rendererSample(ctx) {
  return ctx.execute(`
    const callbacks = window.__TAURI_INTERNALS__?.callbacks;
    const unregister = window.__TAURI_EVENT_PLUGIN_INTERNALS__?.unregisterListener;
    if (!(callbacks instanceof Map) || typeof unregister !== 'function')
      throw new Error('Native callback registry unavailable; cannot qualify listeners');
    const registryName = /window\\['([^']+)'\\]/.exec(String(unregister))?.[1];
    const registry = registryName && Object.getOwnPropertyDescriptor(window, registryName)?.value;
    if (!registry) throw new Error('Native event registry unavailable; cannot qualify listeners');
    const events = {};
    for (const name of Object.getOwnPropertyNames(registry)) {
      const records = Object.getOwnPropertyNames(registry[name]).map(id => registry[name][id]);
      events[name] = { retained: records.length, active: records.filter(item => callbacks.has(item.handlerId)).length };
    }
    return { callbacks: callbacks.size, events,
      activeListeners: Object.values(events).reduce((sum, item) => sum + item.active, 0),
      retainedListenerRecords: Object.values(events).reduce((sum, item) => sum + item.retained, 0),
      elements: document.getElementsByTagName('*').length,
      summaries: document.querySelectorAll(arguments[0]).length,
      // A random mark set on the first reading survives only if the page is
      // never reloaded; WebKit coarsens performance.timeOrigin, so two reads
      // of one document can differ by a millisecond.
      pageMark: (window.__shadowEndurancePageMark ??= crypto.randomUUID()),
      documentTimeOrigin: performance.timeOrigin };
  `, [summary]);
}

async function clickXPath(ctx, expression) {
  const found = await ctx.wd("POST", `/session/${ctx.session}/element`, { using: "xpath", value: expression });
  await ctx.wd("POST", `/session/${ctx.session}/element/${found[ELEMENT]}/click`, {});
}

export async function nativeEndurance(ctx) {
  // Fail up front when the required runtime cannot read persisted task rows.
  const { DatabaseSync } = await import("node:sqlite");
  const limits = {
    appPssGrowthKiB: 64 * 1024, treePssGrowthKiB: 192 * 1024,
    lateTreePssGrowthKiB: 64 * 1024, treeRssCeilingKiB: 2 * 1024 * 1024,
    fdGrowth: 8, threadGrowth: 8, callbackGrowth: 6,
    activeListenerGrowth: 1, retainedListenerRecordGrowth: 2,
    mountedElementGrowth: 15000, lateMountedElementGrowth: 1000,
    keyboardRoundtripMs: 2000,
  };
  const report = {
    schema: 1, passed: false, required: 100, warmupTasks: 0, warmupTaskIds: [], warmupObservations: [],
    warmupPolicy: { minimum: 25, maximum: 50, batch: 5, stableSamples: 3,
      rule: "Same process identities and three consecutive named-thread-pool samples with range at most one and no net growth; otherwise fail after 50 warm-up tasks." },
    completed: 0, tasks: [], samples: [], limits,
    scope: "Real X11/WebKit native window; 100 UI command tasks, each explicitly approved and awaited. App plus owned subprocess PSS/RSS/fds/threads; actual Tauri callback/listener registries and mounted DOM. No reload or forced GC. Does not cover all DOM listener registrations, GPU/model inference growth, Wayland, or UI-01 long-stream input p95.",
    limitRationale: "A bounded measured warm-up separates lazy native/WebKit thread-pool initialization from sustained accumulation. Its observations and task identities remain in this receipt; no convergence means failure. Memory/fd/listener bounds also apply during warm-up. The unchanged post-baseline thread allowance is eight. Retained transcript evidence may grow while the rendered row window stabilizes: allow 64 MiB app and 192 MiB whole-tree PSS, at most 64 MiB in the last 50 tasks. Fixed descriptor/callback allowances cover transient polling; surviving process count may not grow. Native listener metadata must remain bounded independently of transcript history.",
  };
  const destination = path.join(ctx.artifacts, "native-endurance.json");
  const requestsBefore = (await ctx.modelRequests()).length;
  const initialJobs = await ctx.jobsFor(ctx.sessionId);
  const known = new Set(initialJobs.map(job => job.id));
  const originalDraft = await ctx.execute("return document.querySelector(arguments[0]).value", [composer]);
  const draft = "UI-03 unsent draft stays intact.";
  let database;
  try {
    report.binarySha256 = await executableHash(ctx.appPid);
    report.platform = { node: process.version, arch: process.arch,
      kernel: (await readFile("/proc/sys/kernel/osrelease", "utf8")).trim(),
      cpu: /^model name\s*:\s*(.+)$/m.exec(await readFile("/proc/cpuinfo", "utf8"))?.[1] || "unknown",
      memoryKiB: Number(/^MemTotal:\s+(\d+)/m.exec(await readFile("/proc/meminfo", "utf8"))?.[1]),
    };
    await ctx.fill(composer, draft);
    async function oneTask(label) {
      const command = `test -f hello.txt && printf 'UI-03 ${label} passed\\n'`;
      await ctx.until(`UI-03 ${label}: launch action ready`, () => ctx.execute(`
        return [...document.querySelectorAll('button')].some(button => button.textContent.trim() === 'Run a check…' && !button.disabled);
      `));
      await clickXPath(ctx, "(//button[normalize-space(.)='Run a check…' and not(@disabled)])[last()]");
      await ctx.until(`UI-03 ${label}: dialog`, () => ctx.visible(dialog));
      await ctx.fill(`${dialog} input`, command);
      await ctx.clickButton("Run check", "//div[@role='dialog' and @aria-label='Run a check']");
      await ctx.until(`UI-03 ${label}: admission`, async () => !(await ctx.visible(dialog)));
      const approvalId = await ctx.until(`UI-03 ${label}: approval`, () => ctx.execute(`
        return [...document.querySelectorAll('.approval')].find(node => node.textContent.includes(arguments[0]))?.dataset.approvalId || null;
      `, [command]));
      const admitted = (await ctx.jobsFor(ctx.sessionId)).filter(job => !known.has(job.id));
      assert.equal(admitted.length, 1, `UI-03 ${label}: one new task`);
      const pending = admitted[0];
      assert.ok(!terminal.has(pending.status), "No command completion before Allow");
      assert.equal(pending.mode, "command");
      await ctx.clickButton("Allow", `//div[@data-approval-id='${approvalId}']`);
      let done;
      await ctx.until(`UI-03 ${label}: durable terminal task`, async () => {
        const current = await ctx.api("GET", `/api/jobs/${pending.id}`);
        if (!terminal.has(current.status)) return false;
        done = current;
        return true;
      }, 30000);
      assert.equal(done.status, "completed", `UI-03 ${label}: ${done.summary}`);
      assert.equal(done.workspace, ctx.project);
      assert.equal(done.session_id, ctx.sessionId);
      assert.equal(done.timings?.model_requests, 0);
      assert.equal(done.result?.verification?.verified, true);
      await ctx.until(`UI-03 ${label}: visible completed receipt`, () => ctx.execute(`
        return [...document.querySelectorAll(arguments[0])].some(section => [...section.querySelectorAll('li')].some(item =>
          item.querySelector('code')?.textContent === arguments[1] && item.querySelector('.ok')?.textContent === 'passed'));
      `, [summary, command]));
      assert.equal(await ctx.execute("return document.querySelector(arguments[0]).value", [composer]), draft);
      known.add(done.id);
      return { job_id: done.id, task_id: done.task_id, command };
    }
    async function sample(index, warmup = false) {
      // Permit the just-completed task's asynchronous unsubscribe to settle.
      await delay(500);
      const pids = [ctx.appPid, ...await ctx.descendants(ctx.appPid)];
      const processes = await Promise.all(pids.map(processSample));
      assert.ok(processes.some(item => /WebKitWebProcess$/.test(item.executable)), "Sample includes real WebKit renderer");
      const renderer = await rendererSample(ctx);
      const app = processes.find(item => item.pid === ctx.appPid);
      const totals = Object.fromEntries(["rssKiB", "pssKiB", "fds", "threads"].map(key => [key, processes.reduce((sum, item) => sum + item[key], 0)]));
      // This includes WebDriver transport and UI input handling. It is an upper
      // bound for this short idle-task probe, not long-stream render latency.
      const started = performance.now();
      await ctx.fill(composer, `UI-03 keyboard probe ${index}`);
      const keyboardRoundtripMs = performance.now() - started;
      await ctx.fill(composer, draft);
      const point = { index, processes, processCount: processes.length, appPssKiB: app.pssKiB,
        totals, renderer, keyboardRoundtripMs };
      (warmup ? report.warmupObservations : report.samples).push(point);
      await writeFile(destination, JSON.stringify(report, null, 2) + "\n");
      console.log(`  ..  native UI-03 ${warmup ? "warm-up " : ""}${index}/${warmup ? report.warmupPolicy.maximum : 100}; PSS ${totals.pssKiB} KiB; ${totals.fds} fds; ${totals.threads} threads; ${renderer.activeListeners} active listeners / ${renderer.retainedListenerRecords} retained records`);
      return point;
    }
    const cold = await sample(0, true);
    for (let index = 1; index <= report.warmupPolicy.maximum; index++) {
      report.warmupTaskIds.push(await oneTask(`warmup-${index}`));
      report.warmupTasks = index;
      if (index % report.warmupPolicy.batch !== 0) continue;
      const point = await sample(index, true);
      assert.equal(point.renderer.pageMark, cold.renderer.pageMark, "No reload during warm-up");
      assert.ok(point.processCount <= cold.processCount, "No process growth hidden by warm-up");
      assert.ok(point.appPssKiB <= cold.appPssKiB + limits.appPssGrowthKiB, "App memory growth during warm-up");
      assert.ok(point.totals.pssKiB <= cold.totals.pssKiB + limits.treePssGrowthKiB, "Owned memory growth during warm-up");
      assert.ok(point.totals.fds <= cold.totals.fds + limits.fdGrowth, "Descriptor growth during warm-up");
      assert.ok(point.renderer.callbacks <= cold.renderer.callbacks + limits.callbackGrowth, "Callback growth during warm-up");
      assert.ok(point.renderer.activeListeners <= cold.renderer.activeListeners + limits.activeListenerGrowth, "Active listener growth during warm-up");
      assert.ok(point.renderer.retainedListenerRecords <= cold.renderer.retainedListenerRecords + limits.retainedListenerRecordGrowth, "Retained listener growth during warm-up");
      if (index >= report.warmupPolicy.minimum && warmupConverged(report.warmupObservations, report.warmupPolicy.stableSamples)) {
        report.warmupConverged = true;
        break;
      }
    }
    assert.equal(report.warmupConverged, true, "Named thread pools did not converge within bounded warm-up");
    const baseline = await sample(0);
    for (let index = 1; index <= 100; index++) {
      report.tasks.push(await oneTask(String(index).padStart(3, "0")));
      report.completed = report.tasks.length;
      if (index % 25 === 0) await sample(index);
    }
    assert.equal(new Set(report.tasks.map(item => item.job_id)).size, 100);
    assert.equal(new Set(report.tasks.map(item => item.task_id)).size, 100);
    const warmupIds = new Set(report.warmupTaskIds.map(item => item.job_id));
    assert.ok(report.tasks.every(item => !warmupIds.has(item.job_id)), "All 100 qualification tasks occur after warm-up");
    database = new DatabaseSync(path.join(ctx.profile, "state/shadow-agent/shadow-agent.db"), { readOnly: true });
    const persisted = database.prepare("SELECT payload FROM desktop_jobs WHERE id=?");
    for (const item of report.tasks) {
      const row = JSON.parse(persisted.get(item.job_id)?.payload || "null");
      assert.equal(row?.status, "completed", `Persisted completion for ${item.job_id}`);
      assert.equal(row?.task_id, item.task_id);
      assert.equal(row?.session_id, ctx.sessionId);
    }
    report.databaseIntegrity = database.prepare("PRAGMA integrity_check").get().integrity_check;
    assert.equal(report.databaseIntegrity, "ok");
    assert.equal(database.prepare("SELECT count(*) AS n FROM desktop_jobs WHERE json_extract(payload,'$.status') IN ('queued','running','paused','cancelling')").get().n, 0);
    assert.equal((await ctx.modelRequests()).length, requestsBefore, "Endurance commands invoke no model");
    for (const point of report.samples.slice(1)) {
      assert.equal(point.renderer.pageMark, baseline.renderer.pageMark, "No page reload masks accumulation");
      assert.ok(point.processCount <= baseline.processCount, "No owned process accumulation");
      for (const initial of baseline.processes.filter(item => item.pid === ctx.appPid || /WebKitWebProcess$/.test(item.executable)))
        assert.ok(point.processes.some(item => item.pid === initial.pid && item.start === initial.start), "App and renderer have not restarted");
      assert.ok(point.appPssKiB <= baseline.appPssKiB + limits.appPssGrowthKiB, "App PSS growth exceeds allowance");
      assert.ok(point.totals.pssKiB <= baseline.totals.pssKiB + limits.treePssGrowthKiB, "Owned-tree PSS growth exceeds allowance");
      assert.ok(point.totals.rssKiB <= limits.treeRssCeilingKiB, "Owned-tree RSS exceeds ceiling");
      assert.ok(point.totals.fds <= baseline.totals.fds + limits.fdGrowth, "File descriptor accumulation");
      assert.ok(point.totals.threads <= baseline.totals.threads + limits.threadGrowth, "Thread accumulation");
      assert.ok(point.renderer.callbacks <= baseline.renderer.callbacks + limits.callbackGrowth, "Native callback accumulation");
      assert.ok(point.renderer.activeListeners <= baseline.renderer.activeListeners + limits.activeListenerGrowth, "Active native event-listener accumulation");
      assert.ok(point.renderer.retainedListenerRecords <= baseline.renderer.retainedListenerRecords + limits.retainedListenerRecordGrowth, "Retired native event-listener metadata accumulates");
      assert.ok(point.renderer.elements <= baseline.renderer.elements + limits.mountedElementGrowth, "Mounted DOM exceeds bounded history allowance");
      assert.ok(point.keyboardRoundtripMs < limits.keyboardRoundtripMs, "Idle-task keyboard round-trip exceeds 2 seconds");
    }
    const middle = report.samples.find(item => item.index === 50);
    const last = report.samples.at(-1);
    assert.ok(last.totals.pssKiB - middle.totals.pssKiB <= limits.lateTreePssGrowthKiB, "Late owned-tree memory growth exceeds allowance");
    assert.ok(last.renderer.elements - middle.renderer.elements <= limits.lateMountedElementGrowth, "Rendered history does not stabilize");
    await ctx.fill(composer, originalDraft);
    await ctx.screenshot("endurance-100-tasks");
    report.passed = true;
    ctx.note("UI-03: 100 sequential native-window tasks with real approvals; durable completions and bounded owned-process/listener resources");
  } catch (error) {
    report.failure = error.stack || String(error);
    throw error;
  } finally {
    database?.close();
    await writeFile(destination, JSON.stringify(report, null, 2) + "\n");
  }
}
