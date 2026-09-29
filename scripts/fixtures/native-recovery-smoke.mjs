// Real-window recovery phases, called after the local task, editor, reload
// and Run a check phases (the local model is added and selected; the first
// conversation is open). Both use only the fake llama-server, so they run the
// same with or without vendor CLIs or a ready cloud model.
//
// nativeLocalMemory (LOC-03, RUN-02 preparation failure): the local model
// cannot allocate its KV cache. The conversation ends with a plain reason and
// no spinner, nothing in the project changes, and "Use a smaller context and
// retry" loads the model with half the context and finishes the task.
//
// nativeReconnect (UI-02, RUN-05): conversation A streams while its window
// drops, triplicates and re-reads engine events (stale cursors, failed reads)
// and then reloads mid-stream; conversation B has a queued check that runs
// after A. Each conversation shows its own rows once, the shell effect runs
// once, B's approval is answered once, and the durable event log has one
// terminal event per task.
import assert from "node:assert/strict";
import { readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import path from "node:path";

const composer = 'textarea[aria-label="Message ShadowCode"]';
const summaries = 'section[aria-label="Task summary"]';

async function projectSnapshot(project) {
  const files = {};
  async function walk(dir) {
    for (const name of (await readdir(dir)).sort()) {
      if (name === ".git") continue;
      const file = path.join(dir, name);
      const info = await stat(file);
      if (info.isDirectory()) await walk(file);
      else files[path.relative(project, file)] = (await readFile(file)).toString("base64");
    }
  }
  await walk(project);
  files[".git/index"] = (await readFile(path.join(project, ".git/index"))).toString("base64");
  return files;
}

const occurrences = (haystack, needle) => haystack.split(needle).length - 1;
const transcriptText = (ctx) => ctx.execute("return document.querySelector('main')?.innerText || document.body.innerText");

export async function nativeLocalMemory(ctx) {
  const card = 'section[aria-label="Not enough memory"]';
  const before = await projectSnapshot(ctx.project);
  const launchesBefore = (await ctx.launches()).length;
  // The next launch of the loaded model needs a fresh process: unload it
  // (as Settings › Local models does), then only an 8,192-token context fits.
  await ctx.api("POST", "/api/local-models/unload", {});
  await writeFile(path.join(ctx.runtimeDir, "oom-above-ctx"), "8192\n");
  const jobsBefore = await ctx.jobsFor(ctx.sessionId);
  try {
    await ctx.send("memory-probe: what is in this project?");
    await ctx.until("Memory failure card", () => ctx.visible(card), 30000);
    const failed = (await ctx.jobsFor(ctx.sessionId)).find((job) => !jobsBefore.some((b) => b.id === job.id));
    assert.equal(failed?.status, "failed", `the task ended: ${JSON.stringify(failed)}`);
    const memory = failed.result?.local_out_of_memory;
    assert.equal(memory?.memory, "system");
    assert.equal(memory?.smaller_context, 8192);
    const context = memory.context_tokens.toLocaleString("en-US");
    const body = await transcriptText(ctx);
    assert.ok(body.includes(`Not enough free system memory to load Coder Test 1B with a ${context}-token context. Nothing in your project was changed.`), "Plain reason in the conversation");
    assert.ok(!/llama_kv_cache|failed to allocate buffer/.test(body), "No raw load log in the conversation");
    assert.equal(await ctx.visible('button[aria-label="Stop task"]'), false, "No running task left behind (no endless spinner)");
    assert.equal(await ctx.execute("return document.querySelectorAll(arguments[0]).length", [card]), 1);
    assert.deepEqual(await projectSnapshot(ctx.project), before, "The project is unchanged");
    assert.equal((await ctx.modelRequests()).filter((r) => r.request.includes("memory-probe")).length, 0, "The model never received the request");
    await ctx.screenshot("local-memory");
    await ctx.accessibility("local-memory");

    await ctx.clickButton("Use a 8,192-token context and retry", "//section[@aria-label='Not enough memory']");
    const retried = await ctx.until("Retried task finished", async () => {
      const jobs = (await ctx.jobsFor(ctx.sessionId)).filter((job) => !jobsBefore.some((b) => b.id === job.id) && job.id !== failed.id);
      if (jobs.some((job) => ["failed", "cancelled"].includes(job.status))) throw new Error(`Retry failed: ${JSON.stringify(jobs)}`);
      return jobs.find((job) => job.status === "completed");
    }, 90000);
    assert.match(retried.task, /memory-probe/);
    await ctx.until("Retried answer shown", async () => (await transcriptText(ctx)).includes("The project has a README and a greeting file."), 15000);
    const launches = (await ctx.launches()).slice(launchesBefore);
    assert.deepEqual(launches.map((l) => l.argv[l.argv.indexOf("--ctx-size") + 1]), [String(memory.context_tokens), "8192"], "One failed load, then one at the smaller context");
    const config = await ctx.api("GET", "/api/config");
    assert.equal((config.values || config.config || config).local_engine?.context_size, 8192, "The smaller context was saved");
    await ctx.until("Memory card closed after the retry", async () => !(await ctx.execute("return [...document.querySelectorAll(arguments[0]+' button')].length", [card])));
    ctx.note(`local model out of memory: plain reason, no spinner, project unchanged; "Use a 8,192-token context and retry" finished the task`);
  } finally {
    await rm(path.join(ctx.runtimeDir, "oom-above-ctx"), { force: true });
    await ctx.api("PUT", "/api/config", { values: { local_engine: { context_size: 0 } } });
  }
}

/** Replace the page's IPC transport with one that re-reads old event rows,
 * fails some event reads, and drops then triplicates engine wake-ups. The
 * engine is untouched; only what the page receives changes. */
const INJECT = `
  const jobId = arguments[0];
  if (window.__recovery) return window.__recovery;
  const state = window.__recovery = { rewritten: 0, failed: 0, dropped: 0, tripled: 0, banner: false, reads: 0 };
  const originalFetch = window.fetch;
  window.fetch = function(input, init) {
    const url = String((input && input.url) || input);
    if (/^(ipc:\\/\\/localhost|https?:\\/\\/ipc\\.localhost)\\/api(\\?|$)/.test(url) && init && typeof init.body === 'string') {
      let args = null;
      try { args = JSON.parse(init.body); } catch (e) {}
      const request = args && args.request;
      const match = request && /^\\/api\\/jobs\\/([^/]+)\\/events\\?after=(\\d+)/.exec(request.path || '');
      if (match && match[1] === jobId) {
        state.reads++;
        // Every third read fails (twice in all): the window shows it is
        // reconnecting and recovers by polling from its last row.
        if (state.failed < 2 && state.reads % 3 === 0) {
          state.failed++;
          return Promise.resolve(new Response(JSON.stringify('window test: event read dropped'), { status: 500, headers: { 'Content-Type': 'application/json', 'Tauri-Response': 'error' } }));
        }
        // Other reads start four rows early: the engine sends rows the page
        // already has.
        const after = Number(match[2]);
        if (after > 0) {
          state.rewritten++;
          request.path = request.path.replace('after=' + after, 'after=' + Math.max(0, after - 4));
          init = Object.assign({}, init, { body: JSON.stringify(args) });
        }
      }
    }
    return originalFetch.call(this, input, init);
  };
  const callbacks = window.__TAURI_INTERNALS__.callbacks;
  const unregister = window.__TAURI_EVENT_PLUGIN_INTERNALS__.unregisterListener;
  const registryName = /window\\['([^']+)'\\]/.exec(String(unregister))[1];
  const registry = window[registryName]['shadowcode:events'];
  let wrapped = 0;
  for (const id of Object.getOwnPropertyNames(registry)) {
    const handler = registry[id].handlerId;
    const original = callbacks.get(handler);
    if (!original) continue;
    wrapped++;
    callbacks.set(handler, (payload) => {
      // The first wake-ups are lost (a gap only polling can close); later
      // ones arrive three times.
      if (state.dropped < 3) { state.dropped++; return; }
      state.tripled++;
      original(payload); original(payload); original(payload);
    });
  }
  if (!wrapped) throw new Error('No shadowcode:events listener to disturb');
  new MutationObserver(() => {
    if (/Reconnecting… Your task continues in the background/.test(document.body.innerText)) state.banner = true;
  }).observe(document.body, { subtree: true, childList: true, characterData: true });
  return state;
`;

export async function nativeReconnect(ctx) {
  const A = ctx.sessionId;
  const effects = (name) => readFile(path.join(ctx.project, name), "utf8").catch(() => "");
  const openConversation = async (id) => {
    await ctx.click(`button.task-link[data-session-id="${id}"]`);
    await ctx.until(`Conversation ${id} open`, () => ctx.execute("return document.querySelector(arguments[0])?.getAttribute('aria-current')==='page'", [`button.task-link[data-session-id="${id}"]`]));
  };
  const jobsBefore = await ctx.jobsFor(A);
  await ctx.send("reconnect-probe: record one effect, then report slowly.");
  const jobA = await ctx.until("Task A started", async () => (await ctx.jobsFor(A)).find((job) => !jobsBefore.some((b) => b.id === job.id)), 15000);
  await ctx.until("A's effect approval", () => ctx.execute("return [...document.querySelectorAll('.approval')].some(a=>a.textContent.includes('effects-A.log'))"), 60000);
  // Conversation B gets a check queued behind A in the same project: two
  // conversations with live tasks during the disturbance and the reloads.
  const B = (await ctx.api("POST", "/api/sessions", { title: "Reconnect check B" })).id;
  assert.ok(B && B !== A);
  const checkB = "printf 'B\\n' >> effects-B.log && printf 'reconnect check B done\\n'";
  const jobB = await ctx.api("POST", "/api/jobs/test", { command: checkB, session_id: B, queue: true });
  assert.equal(jobB.session_id, B);
  assert.equal(jobB.status, "queued", "B waits for A");
  // A reload while A waits for approval: the card comes back, once.
  await ctx.wd("POST", `/session/${ctx.session}/refresh`, {});
  await ctx.until("B listed after the first reload", () => ctx.visible(`button.task-link[data-session-id="${B}"]`), 20000);
  await openConversation(A);
  await ctx.until("A's approval restored", () => ctx.execute("return [...document.querySelectorAll('.approval')].filter(a=>a.textContent.includes('effects-A.log')).length===1"), 20000);
  const injected = await ctx.execute(INJECT, [jobA.id]);
  assert.ok(injected, "Event disturbance installed");
  await ctx.approve("effects-A.log");
  await ctx.until("A streams while disturbed", async () => (await transcriptText(ctx)).includes("part 6"), 30000);
  await ctx.until("Fake model holds the stream", () => stat(path.join(ctx.runtimeDir, "reconnect-holding")).then(() => true, () => false), 10000);
  const disturbed = await ctx.until("Disturbance exercised", async () => {
    const s = await ctx.execute("return window.__recovery");
    return s.rewritten >= 2 && s.failed === 2 && s.dropped === 3 && s.tripled > 0 && s.banner && s;
  }, 20000);
  await ctx.until("Reconnected", async () => !(await transcriptText(ctx)).includes("Reconnecting…"), 15000);
  const live = await transcriptText(ctx);
  assert.equal(occurrences(live, "reconnect-probe: record one effect"), 1, "User message once");
  assert.equal(occurrences(live, "Reconnect probe: part 1 part 2 part 3 part 4 part 5 part 6"), 1, "Streamed text once, in order");
  assert.ok(await ctx.visible('button[aria-label="Stop task"]'), "A is still running");
  assert.equal(await effects("effects-A.log"), "A\n", "The shell effect ran once");
  await ctx.screenshot("reconnect-disturbed");

  // Reload the window while A's stream is held open and B is queued.
  await ctx.wd("POST", `/session/${ctx.session}/refresh`, {});
  await ctx.until("B listed after reload", () => ctx.visible(`button.task-link[data-session-id="${B}"]`), 20000);
  await openConversation(A);
  await ctx.until("A restored mid-stream", async () => (await transcriptText(ctx)).includes("part 6") && await ctx.visible('button[aria-label="Stop task"]'), 20000);
  const restored = await transcriptText(ctx);
  assert.equal(occurrences(restored, "reconnect-probe: record one effect"), 1);
  assert.equal(occurrences(restored, "Reconnect probe: part 1 part 2 part 3 part 4 part 5 part 6"), 1);
  assert.equal(await ctx.execute("return document.querySelectorAll('.approval').length"), 0, "No stale approval after the reload");
  assert.equal(await ctx.execute("return document.querySelector(arguments[0])?.dataset.badge", [`button.task-link[data-session-id="${B}"]`]), "queued", "B's check still waits");

  await writeFile(path.join(ctx.runtimeDir, "release-reconnect"), "");
  await ctx.until("A finished after the reload", async () => (await transcriptText(ctx)).includes("Finished after the reload."), 30000);
  await ctx.until("A's summary", async () => (await ctx.jobsFor(A)).find((job) => job.id === jobA.id)?.status === "completed", 15000);
  // A's view never shows B's approval; B's card waits in B.
  await ctx.until("B needs approval", async () => (await ctx.api("GET", `/api/approvals?session_id=${B}`)).approvals.length === 1, 20000);
  assert.equal(await ctx.execute("return document.querySelectorAll('.approval').length"), 0, "B's approval stays in B");
  const doneA = await transcriptText(ctx);
  assert.equal(occurrences(doneA, "Finished after the reload."), 1, "Final text once");
  assert.ok(!doneA.includes("reconnect check B"), "No B rows in A");

  await openConversation(B);
  const approvalId = await ctx.until("B's approval card", () => ctx.execute("return [...document.querySelectorAll('.approval')].find(a=>a.textContent.includes('effects-B.log'))?.dataset.approvalId"), 20000);
  await ctx.approve("effects-B.log");
  await ctx.until("B's check finished", async () => (await ctx.jobsFor(B)).find((job) => job.id === jobB.id)?.status === "completed", 30000);
  const again = await ctx.api("POST", `/api/approvals/${approvalId}`, { decision: "approve", scope: "once", session_id: B }).then(() => null, (error) => String(error));
  assert.match(again || "", /expired or was already answered/, "A second answer is refused");
  await ctx.until("B's summary", () => ctx.visible(summaries), 15000);
  const doneB = await transcriptText(ctx);
  assert.ok(!doneB.includes("Reconnect probe"), "No A rows in B");
  assert.equal(await effects("effects-A.log"), "A\n");
  assert.equal(await effects("effects-B.log"), "B\n");

  // The durable log: unique rows, one terminal event per task, one tool run.
  for (const job of [jobA, jobB]) {
    const page = await ctx.api("GET", `/api/jobs/${job.id}/events?after=0&limit=2000`);
    const rows = page.events.filter((e) => e.task_id === job.task_id);
    assert.equal(new Set(rows.map((e) => e.id)).size, rows.length, "Event ids are unique");
    assert.equal(rows.filter((e) => e.type === "agent.completed").length, 1, "One terminal event");
    const tools = rows.filter((e) => e.type === "tool.completed").map((e) => e.payload.call_id);
    assert.equal(new Set(tools).size, tools.length, "Each tool call completed once");
    assert.equal(tools.length, 1, "One shell run");
  }
  await openConversation(A);
  await ctx.until("Back on A", async () => (await transcriptText(ctx)).includes("Finished after the reload."));
  ctx.note(`reconnect: A streamed through ${disturbed.dropped} dropped and ${disturbed.tripled} tripled wake-ups, ${disturbed.rewritten} stale re-reads, ${disturbed.failed} failed reads and a reload; B's queued check ran after; rows, effects and approvals once each`);
}
