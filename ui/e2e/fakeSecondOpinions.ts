/**
 * Fake second-opinion routes (`/api/second-opinions…`) for the Playwright
 * suite. Install it after `installFakeBackend` and `installFakeTools`: it
 * wraps that bridge, answers the second-opinion routes and passes everything
 * else through. Reviews move from queued to running to completed on timers,
 * waking the window like the engine's `second_opinion.updated` broadcast.
 * Consent and offline rules follow the engine: work that ran on this
 * computer reaches a cloud reviewer only with consent.
 *
 * Self-contained like fakeBackend.ts (Playwright serialises it).
 */
export function installFakeSecondOpinions() {
  type Json = Record<string, any>;
  const inner = (window as any).__SHADOW_TEST_TRANSPORT__;
  if (!inner) throw new Error("installFakeBackend must run first");
  const fake = (window as any).__SHADOW_FAKE__;
  const log: { method: string; path: string; body: any }[] = fake.log;
  const state = fake.state;
  const workspace = "/work/demo";
  const records: Json[] = [];
  const prefs: Json = { model: null, before_commit: false };
  const listeners: ((payload: unknown) => void)[] = [];
  let serial = 0;
  const wake = (sessionId: string | null) =>
    setTimeout(
      () =>
        listeners.forEach((fn) =>
          fn({ type: "second_opinion.updated", session_id: sessionId }),
        ),
      5,
    );
  const fail = (message: string, extra: Json = {}): never => {
    throw new Error(JSON.stringify({ error: message, ...extra }));
  };
  const staged = () => fake.tools?.git?.staged || 0;
  const stagedHash = () =>
    `staged-${fake.tools?.git?.commits?.length || 0}-${staged()}`;
  const targets = async (): Promise<Json[]> =>
    ((await inner.request("/api/picker", "GET", null)) as Json).targets;
  const route = (rows: Json[], id: string) => {
    const row = rows.find((t) => t.id === id);
    return row
      ? { model: row.id, label: row.name, local: row.inference === "local" }
      : null;
  };
  const jobOfTask = (task: string) =>
    [...state.jobs].reverse().find((j: Json) => j.task_id === task);
  const lastWriter = () =>
    [...state.jobs]
      .reverse()
      .find((j: Json) => j.model && j.mode !== "command" && j.task_id);

  const STAGED_DIFF = {
    path: "src/math.js",
    status: "modified",
    binary: false,
    diff: "@@ -1,3 +1,3 @@\n export function add(a, b) {\n-  return a - b;\n+  return a + b;\n }\n",
  };
  const TASK_DIFF = {
    path: "src/app.ts",
    status: "modified",
    binary: false,
    diff: "@@ -1,1 +1,1 @@\n-export const add = (a, b) => a - b;\n+export const add = (a, b) => a + b;\n@@ -12,1 +12,1 @@\n-export const VERSION = 1;\n+export const VERSION = 2;\n",
  };

  function findings(source: string): Json[] {
    const file = source === "staged" ? "src/math.js" : "src/app.ts";
    return [
      source === "staged"
        ? {
            id: "f1",
            file,
            line: 2,
            hunk: "@@ -1,3 +1,3 @@",
            severity: "high",
            title: "Negative numbers are not covered",
            explanation:
              "add is fixed, but nothing checks it with negative numbers.",
            suggested_fix: "Add a test for add(-1, 1).",
            status: "open",
          }
        : {
            id: "f1",
            file,
            line: 12,
            hunk: "@@ -12,1 +12,1 @@",
            severity: "medium",
            title: "Version bumped without a release note",
            explanation: "VERSION changed but the changelog does not say why.",
            suggested_fix: "Add a line to CHANGELOG.md.",
            status: "open",
          },
      {
        id: "f2",
        file: "",
        line: null,
        severity: "low",
        title: "No test covers the change",
        explanation: "The change has no test of its own.",
        suggested_fix: "",
        status: "open",
      },
    ];
  }

  async function start(body: Json) {
    const rows = await targets();
    const reviewer = route(rows, body.model);
    if (!reviewer) fail("Choose a model for the second opinion");
    const source = body.source || (body.task_id ? "task" : "staged");
    let session = body.session_id || null;
    let writerJob: Json | undefined;
    if (source === "task") {
      writerJob = jobOfTask(body.task_id);
      if (!writerJob) fail("Task not found");
      session = writerJob!.session_id;
    } else {
      if (!staged())
        fail("Nothing is staged yet. Stage the changes to review first.");
      writerJob = lastWriter();
      session = session || writerJob?.session_id || null;
    }
    const writer = writerJob ? route(rows, writerJob.model) : null;
    if (state.config.network?.mode === "offline" && !reviewer!.local)
      fail("Offline mode: choose a model that runs on this computer");
    const localWork =
      Boolean(writer?.local) ||
      Boolean(session && state.lastRoute?.[session] === "local");
    if (!reviewer!.local && localWork && !body.consent)
      fail(
        "Confirm before continuing: this work ran on this computer; a second opinion sends it to a cloud service",
        {
          needs_consent: true,
          status: 409,
          handoff: {
            from: writer?.label || "qwen3:14b",
            to: reviewer!.label,
            excerpt_chars: 1840,
            images: 0,
            purpose: "second_opinion",
            files: 1,
          },
        },
      );
    const id = `op${++serial}`;
    const record: Json = {
      id,
      kind: body.kind || "review",
      workspace,
      source,
      session_id: session,
      task_id: source === "task" ? body.task_id : null,
      question: body.question || "",
      reviewer: reviewer,
      writer,
      same_model: writer?.model === reviewer!.model,
      consented: Boolean(body.consent),
      job_id: `rj${serial}`,
      review_session: `rs${serial}`,
      review_task: `rt${serial}`,
      status: "queued",
      created_at: Date.now() / 1000,
      finished_at: null,
      diff_hash: source === "staged" ? stagedHash() : `task-${body.task_id}`,
      files: [source === "staged" ? "src/math.js" : "src/app.ts"],
      omitted: [],
      diff: [source === "staged" ? STAGED_DIFF : TASK_DIFF],
      truncated: false,
      context_chars: 1840,
      summary: "",
      findings: [],
      format_note: "",
      error: "",
      usage: {},
      model_name: reviewer!.label,
      reviewer_changed: [],
    };
    records.unshift(record);
    prefs.model = body.model;
    setTimeout(() => {
      if (record.status !== "queued") return;
      record.status = "running";
      wake(session);
    }, 150);
    setTimeout(() => {
      if (record.status !== "running") return;
      record.status = "completed";
      record.finished_at = Date.now() / 1000;
      record.usage = {
        total_tokens: 3100,
        cost_usd: reviewer!.local ? 0 : null,
        source: reviewer!.local ? "local" : "vendor",
      };
      if (record.kind === "ask")
        record.summary =
          "The fix is right, but nothing tests negative numbers. Add a case for add(-1, 1).";
      else {
        record.summary = "One real gap and one small one.";
        record.findings = findings(source);
      }
      wake(session);
    }, 900);
    wake(session);
    return record;
  }

  async function fix(record: Json, finding: Json, consent: boolean) {
    if (finding.status === "fixing")
      fail("A fix for this finding is already queued");
    const writer = record.task_id
      ? jobOfTask(record.task_id)
      : lastWriter() || null;
    const session = record.session_id || writer?.session_id;
    if (!writer) fail("There is no conversation to queue the fix in");
    const where = finding.file
      ? ` in ${finding.file}${finding.line ? ` at line ${finding.line}` : ""}`
      : "";
    const job = await inner.request("/api/jobs", "POST", {
      task: `A second-opinion review by ${record.reviewer.label} found a problem${where}: ${finding.title}\n\nCheck whether this is right. If it is, fix it and keep the change focused; if it is not, explain why.`,
      session_id: session,
      model: writer.model,
      queue: true,
      handoff_consent: consent || undefined,
    });
    finding.status = "fixing";
    finding.fix_job_id = (job as Json).id;
    finding.fix_session_id = (job as Json).session_id;
    wake(record.session_id);
    return { second_opinion: record, job };
  }

  async function answer(
    method: string,
    path: string,
    body: any,
  ): Promise<unknown> {
    const url = new URL(path, "http://fake.local");
    const q = url.searchParams;
    const p = url.pathname;
    if (!p.startsWith("/api/second-opinions")) return undefined;
    let m: RegExpMatchArray | null;
    if (p === "/api/second-opinions" && method === "GET")
      return {
        workspace,
        second_opinions: records.filter(
          (r) =>
            (!q.get("session_id") || r.session_id === q.get("session_id")) &&
            (!q.get("task_id") || r.task_id === q.get("task_id")) &&
            (!q.get("source") || r.source === q.get("source")),
        ),
      };
    if (p === "/api/second-opinions" && method === "POST") return start(body);
    if (p === "/api/second-opinions/options") {
      const rows = await targets();
      const job = q.get("task_id")
        ? jobOfTask(q.get("task_id")!)
        : lastWriter();
      const writer = job ? route(rows, job.model) : null;
      const session = job?.session_id || q.get("session_id");
      return {
        workspace,
        prefs: { ...prefs },
        offline: state.config.network?.mode === "offline",
        writer,
        local_only:
          Boolean(writer?.local) ||
          Boolean(session && state.lastRoute?.[session] === "local"),
      };
    }
    if (p === "/api/second-opinions/current")
      return q.get("source") === "task"
        ? {
            hash: `task-${q.get("task_id")}`,
            files: ["src/app.ts"],
            omitted: [],
            truncated: false,
          }
        : {
            hash: stagedHash(),
            files: staged() ? ["src/math.js"] : [],
            omitted: [],
            truncated: false,
          };
    if (p === "/api/second-opinions/prefs" && method === "POST") {
      if (typeof body?.model === "string") prefs.model = body.model || null;
      if (typeof body?.before_commit === "boolean")
        prefs.before_commit = body.before_commit;
      return { ...prefs };
    }
    if ((m = p.match(/^\/api\/second-opinions\/([^/]+)(?:\/(.*))?$/))) {
      const record = records.find((r) => r.id === m![1]);
      if (!record) fail("Second opinion not found");
      const rest = m[2] || "";
      if (!rest && method === "GET") return record;
      if (rest === "cancel" && method === "POST") {
        if (["queued", "running"].includes(record!.status)) {
          record!.status = "cancelled";
          record!.error = "The second opinion was stopped.";
          wake(record!.session_id);
        }
        return record;
      }
      const f = rest.match(/^findings\/([^/]+)(\/fix)?$/);
      if (f && method === "POST") {
        const finding = record!.findings.find((x: Json) => x.id === f[1]);
        if (!finding) fail("Finding not found");
        if (f[2]) return fix(record!, finding, Boolean(body?.consent));
        finding.status = body?.status === "dismissed" ? "dismissed" : "open";
        return record;
      }
    }
    return fail(`Application command is not available: ${method} ${p}`);
  }

  const bridge = {
    ...inner,
    async request(path: string, method: string, body: unknown) {
      const result = await answer(method, path, body);
      if (result === undefined) return inner.request(path, method, body);
      log.push({ method, path, body });
      return JSON.parse(JSON.stringify(result));
    },
    async listen(event: string, handler: (payload: unknown) => void) {
      const stop = await inner.listen(event, handler);
      if (event !== "shadowcode:events") return stop;
      listeners.push(handler);
      return () => {
        stop();
        const at = listeners.indexOf(handler);
        if (at >= 0) listeners.splice(at, 1);
      };
    },
  };
  (window as any).__SHADOW_TEST_TRANSPORT__ = bridge;
  fake.opinions = { records, prefs };
}
