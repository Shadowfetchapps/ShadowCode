import { describe, expect, it } from "vitest";
import type { EventRow } from "../api";
import { replay } from "./transcript";
import {
  classifyTool,
  deriveSteps,
  isVerificationCommand,
  parseVerification,
  verificationLine,
} from "./activity";

let seq = 0;
const ev = (
  type: string,
  payload: Record<string, unknown>,
  task_id = "t1",
): EventRow => ({ id: ++seq, ts: 1000 + seq, type, payload, task_id });

const steps = (events: EventRow[], pending = 0) =>
  deriveSteps(replay(events).activity.t1, pending).map((s) => [
    s.label,
    s.state,
  ]);

describe("activity timeline from real events", () => {
  it("shows task-scoped local preparation before agent.started and clears it when ready", () => {
    const events = [ev("local.runtime_progress", { phase: "preparing" })];
    const preparing = replay(events);
    expect(preparing.activeTaskId).toBe("t1");
    expect(preparing.items).toEqual([]);
    expect(steps(events)).toEqual([["Preparing local model", "active"]]);
    events.push(ev("local.runtime_progress", { phase: "waiting" }));
    expect(steps(events)).toEqual([["Waiting for local runtime", "active"]]);
    events.push(ev("local.runtime_progress", { phase: "loading" }));
    expect(steps(events)).toEqual([["Loading local model", "active"]]);
    const background = replay([
      ...events,
      ev("local.runtime_progress", { phase: "waiting" }, "t2"),
    ]);
    expect(background.activeTaskId).toBe("t1");
    expect(background.activity.t2.localPhase).toBe("waiting");
    events.push(ev("local.runtime_ready", {}));
    expect(steps(events)).toEqual([]);
    events.push(ev("agent.completed", { success: false, cancelled: true }));
    events.push(ev("local.runtime_progress", { phase: "loading" }));
    expect(steps(events)).toEqual([["Stopped", "failed"]]);
    expect(replay(events).activeTaskId).toBeUndefined();
  });

  it("native tools: read, write, pytest, completion", () => {
    const events = [
      ev("agent.started", { task: "Fix it" }),
      ev("tool.started", {
        tool: "read_file",
        call_id: "a",
        arguments: { path: "app.py" },
      }),
      ev("tool.completed", {
        tool: "read_file",
        call_id: "a",
        success: true,
        output_preview: "def add",
      }),
      ev("tool.started", {
        tool: "write_file",
        call_id: "b",
        arguments: { path: "app.py" },
      }),
      ev("tool.completed", {
        tool: "write_file",
        call_id: "b",
        success: true,
        arguments: { path: "app.py" },
      }),
      ev("checkpoint.updated", { paths: ["app.py"], changes: 1 }),
      ev("tool.started", {
        tool: "exec",
        call_id: "c",
        arguments: { command: "python -m pytest -q" },
      }),
    ];
    expect(steps(events)).toEqual([
      ["Reading project", "done"],
      ["Editing files", "done"],
      ["Running checks", "active"],
    ]);
    const done = [
      ...events,
      ev("tool.completed", {
        tool: "exec",
        call_id: "c",
        success: true,
        output: { exit_code: 0 },
      }),
      ev("verification.summary", {
        status: "passed",
        commands: [
          {
            command: "python -m pytest -q",
            kind: "configured_check",
            state: "passed",
            success: true,
            exit_code: 0,
          },
        ],
      }),
      ev("agent.completed", { summary: "Fixed", success: true }),
    ];
    const state = replay(done);
    expect(
      deriveSteps(state.activity.t1).map((s) => [s.label, s.state]),
    ).toEqual([
      ["Reading project", "done"],
      ["Editing files", "done"],
      ["Checks passed", "done"],
      ["Finished", "done"],
    ]);
    const activity = state.activity.t1;
    expect(activity.changed).toEqual(["app.py"]);
    expect(activity.verification?.commands[0]).toMatchObject({
      command: "python -m pytest -q",
      exit_code: 0,
      success: true,
    });
    expect(activity.finishedAt! - activity.startedAt!).toBeGreaterThan(0);
    expect(state.items.at(-1)).toMatchObject({ kind: "summary", taskId: "t1" });
    // Each step keeps the real calls and their output.
    const reading = deriveSteps(activity).find((s) => s.id === "reading")!;
    expect(reading.calls[0]).toMatchObject({
      tool: "read_file",
      output: "def add",
      ok: true,
    });
  });

  it("vendor tools: Codex, Cursor ACP and Claude names", () => {
    const events = [
      ev("agent.started", { task: "Vendor" }),
      ev("tool.started", {
        tool: "cursor.read",
        call_id: "r",
        arguments: { title: "Read app.ts" },
      }),
      ev("tool.completed", {
        tool: "cursor.read",
        call_id: "r",
        success: true,
      }),
      ev("tool.started", {
        tool: "codex.file_change",
        call_id: "f",
        arguments: { paths: ["src/a.rs"] },
      }),
      ev("tool.completed", {
        tool: "codex.file_change",
        call_id: "f",
        success: true,
      }),
      ev("files.changed", { paths: ["src/a.rs"], vendor: "cli-codex" }),
      ev("tool.started", {
        tool: "codex.command_execution",
        call_id: "x",
        arguments: { command: "cargo test --lib" },
      }),
      ev("tool.completed", {
        tool: "codex.command_execution",
        call_id: "x",
        success: false,
      }),
      ev("tool.started", {
        tool: "Bash",
        call_id: "b",
        arguments: { input: { command: "ls -la" } },
      }),
      ev("tool.completed", { tool: "Bash", call_id: "b", success: true }),
      ev("verification.summary", {
        status: "vendor_owned",
        commands: [],
        verified: false,
        vendor_agent: "cli-codex",
        note: "The vendor CLI owns verification.",
      }),
      ev("agent.completed", { summary: "Done", success: true }),
    ];
    const state = replay(events);
    const derived = deriveSteps(state.activity.t1);
    expect(derived.map((s) => s.label)).toEqual([
      "Reading project",
      "Editing files",
      "Running commands",
      "Checks failed",
      "Finished",
    ]);
    expect(derived.find((s) => s.id === "testing")?.detail).toBe(
      "Checks are run and judged by the vendor agent",
    );
    expect(state.activity.t1.changed).toEqual(["src/a.rs"]);
    expect(state.activity.t1.verification?.status).toBe("vendor_owned");
  });

  it("a vendor turn with a project checkpoint can be rewound", () => {
    const reported = replay([
      ev("agent.started", { task: "Edit" }),
      ev("files.changed", { paths: ["notes.txt"] }),
      ev("agent.completed", { summary: "Done", success: true }),
    ]);
    expect(reported.activity.t1.checkpointed).toBeFalsy();
    const recorded = replay([
      ev("agent.started", { task: "Edit" }),
      ev("checkpoint.updated", {
        source: "vendor",
        paths: ["notes.txt", "old.txt"],
        changed: ["notes.txt", "old.txt"],
      }),
      ev("agent.completed", { summary: "Done", success: true }),
    ]);
    expect(recorded.activity.t1.checkpointed).toBe(true);
    expect(recorded.activity.t1.changed).toEqual(["notes.txt", "old.txt"]);
  });

  it("approvals, web sources, handoff and limit events", () => {
    const events = [
      ev("agent.started", { task: "Look it up" }),
      ev("tool.started", {
        tool: "web_fetch",
        call_id: "w",
        arguments: { url: "https://docs.rs" },
      }),
      ev("web.source", {
        url: "https://docs.rs/x",
        final_url: "https://docs.rs/x/1",
        title: "x docs",
        status: 200,
      }),
      ev("tool.completed", {
        tool: "web_fetch",
        call_id: "w",
        success: true,
        sources: [{ url: "https://docs.rs/y", title: "y docs" }],
      }),
      ev("approval.requested", {
        id: "ap1",
        tool: "exec",
        command: "rm -rf build",
      }),
    ];
    expect(steps(events)).toEqual([
      ["Looking up the web", "done"],
      ["Waiting for approval", "active"],
    ]);
    const state = replay([
      ...events,
      ev("approval.resolved", { tool: "exec", approved: true }),
      ev("agent.handoff", {
        from: "local:llamacpp",
        to: "cli:cursor",
        excerpt_chars: 1200,
      }),
      ev("limit.reached", {
        vendor: "cli-codex",
        usage: { state: "limit_reached", label: "Limit" },
      }),
    ]);
    expect(state.activity.t1.sources.map((s) => s.url)).toEqual([
      "https://docs.rs/x",
      "https://docs.rs/y",
    ]);
    // Once answered, the waiting state leaves the timeline.
    expect(
      deriveSteps(state.activity.t1).find((s) => s.id === "waiting"),
    ).toBeUndefined();
    expect(state.items.find((i) => i.kind === "divider")?.text).toBe(
      "Continued on Cursor · previous context summarized (1,200 characters)",
    );
    expect(state.limit?.vendor).toBe("Codex");
    // A new task clears the limit banner.
    expect(
      replay([
        ...events,
        ev("limit.reached", { vendor: "codex" }),
        ev("agent.started", { task: "again" }, "t2"),
      ]).limit,
    ).toBeUndefined();
  });

  it("does not upgrade legacy command success to a configured check", () => {
    const events = [
      ev("agent.started", { task: "x" }),
      ev("tool.started", {
        tool: "exec",
        call_id: "c",
        arguments: { command: "cat hello.txt" },
      }),
      ev("tool.completed", { tool: "exec", call_id: "c", success: true }),
      ev("verification.summary", {
        status: "last_command_succeeded",
        commands: [{ command: "cat hello.txt", success: true, exit_code: 0 }],
      }),
      ev("agent.completed", { summary: "Done", success: true }),
    ];
    const derived = deriveSteps(replay(events).activity.t1);
    expect(derived.map((s) => s.label)).toEqual([
      "Running commands",
      "Finished",
    ]);
    expect(derived[0].calls.map((c) => c.command)).toEqual(["cat hello.txt"]);
  });

  it("shows no invented steps without evidence", () => {
    expect(deriveSteps(undefined)).toEqual([]);
    expect(steps([ev("agent.started", { task: "x" })])).toEqual([]);
  });

  it("classifies test, build and lint commands", () => {
    for (const cmd of [
      "pytest -q",
      "cargo test",
      "npm run build",
      "pnpm test",
      "npx vitest run",
      "go vet ./...",
      "make check",
      "ruff check .",
      "npm --prefix ui run typecheck",
    ])
      expect([cmd, isVerificationCommand(cmd)]).toEqual([cmd, true]);
    for (const cmd of ["ls", "git status", "cat test.txt", "npm install"])
      expect([cmd, isVerificationCommand(cmd)]).toEqual([cmd, false]);
    expect(classifyTool("apply_patch")).toBe("editing");
    expect(classifyTool("search_code")).toBe("reading");
    expect(classifyTool("exec", { command: "echo hi" })).toBe("commands");
  });
});

it("labels typed verification states without promoting ordinary commands", () => {
  const ordinary = parseVerification({
    status: "not_run",
    commands: [
      { command: "printf test", success: true, exit_code: 0, kind: "command" },
    ],
  });
  expect(verificationLine(ordinary)).toBe("Verification not run");
  for (const [status, label] of [
    ["passed", "Configured checks passed"],
    ["stale", "Checks are stale — files changed"],
    ["cancelled", "Verification cancelled"],
    ["skipped", "Verification incomplete"],
    ["incomplete", "Verification incomplete"],
  ]) {
    expect(
      verificationLine(
        parseVerification({
          status,
          commands: [
            {
              command: "npm test",
              kind: "configured_check",
              state: status,
              tool_call_id: "c",
              attempt_id: "a",
              cwd: "/p",
            },
          ],
        }),
      ),
    ).toBe(label);
  }
});

it("retains passing command evidence when final verification was interrupted", () => {
  for (const [status, final_assessment] of [
    ["failed", "not_completed"],
    ["cancelled", "not_completed"],
    ["incomplete", "interrupted"],
  ]) {
    const verification = parseVerification({
      status,
      final_assessment,
      verified: false,
      commands: [
        {
          command: "npm test",
          kind: "configured_check",
          state: "passed",
          success: true,
          exit_code: 0,
          tool_call_id: "observed-check",
        },
      ],
    });
    expect(verification?.finalAssessment).toBe(final_assessment);
    expect(verification?.commands[0]).toMatchObject({
      state: "passed",
      success: true,
      exit_code: 0,
      callId: "observed-check",
    });
    expect(verificationLine(verification)).toBe(
      "Final verification did not finish",
    );
  }
});

it("keeps not-run and freshness-refresh labels ahead of an incomplete assessment", () => {
  const verification = parseVerification({
    status: "incomplete",
    final_assessment: "not_completed",
    commands: [],
  })!;
  expect(verificationLine(verification)).toBe("Verification not run");
  expect(verificationLine({ ...verification, status: "checking" })).toBe(
    "Assessing current files…",
  );
  expect(verificationLine({ ...verification, status: "unavailable" })).toBe(
    "Current verification unavailable",
  );
});

describe("timeline verdicts do not promote unfinished or unsuccessful evidence", () => {
  const check = {
    command: "npm test",
    kind: "configured_check",
    state: "passed",
    success: true,
    exit_code: 0,
  };
  const completedRead = [
    ev("tool.started", {
      tool: "read_file",
      call_id: "read",
      arguments: { path: "app.ts" },
    }),
    ev("tool.completed", { tool: "read_file", call_id: "read", success: true }),
  ];
  it.each([
    ["cancelled", "Checks cancelled"],
    ["stale", "Checks stale"],
    ["skipped", "Checks incomplete"],
    ["incomplete", "Checks incomplete"],
    ["unavailable", "Checks unavailable"],
  ])(
    "%s check receipts stay neutral while completed reading stays done",
    (status, label) => {
      const activity = replay([
        ...completedRead,
        ev("verification.summary", {
          status,
          commands: [{ ...check, state: status, success: false }],
        }),
        ev("agent.completed", {
          success: false,
          cancelled: status === "cancelled",
        }),
      ]).activity.t1;
      expect(deriveSteps(activity).find((s) => s.id === "reading")?.state).toBe(
        "done",
      );
      expect(
        deriveSteps(activity).find((s) => s.id === "testing"),
      ).toMatchObject({ state: "incomplete", label });
    },
  );
  it("keeps failed commands and failed checks visibly failed", () => {
    const activity = replay([
      ev("tool.started", {
        tool: "exec",
        call_id: "shell",
        arguments: { command: "git status" },
      }),
      ev("tool.completed", { tool: "exec", call_id: "shell", success: false }),
      ev("verification.summary", {
        status: "failed",
        commands: [{ ...check, state: "failed", success: false, exit_code: 1 }],
      }),
      ev("agent.completed", { success: false }),
    ]).activity.t1;
    expect(
      deriveSteps(activity)
        .filter((s) => ["commands", "testing"].includes(s.id))
        .map((s) => s.state),
    ).toEqual(["failed", "failed"]);
  });
  it("does not turn an interrupted tool into a completed phase", () => {
    const activity = replay([
      ...completedRead,
      ev("tool.started", {
        tool: "write_file",
        call_id: "pending",
        arguments: { path: "app.ts" },
      }),
      ev("agent.completed", { success: false, cancelled: true }),
    ]).activity.t1;
    expect(deriveSteps(activity).find((s) => s.id === "reading")?.state).toBe(
      "done",
    );
    expect(deriveSteps(activity).find((s) => s.id === "editing")?.state).toBe(
      "incomplete",
    );
  });
  it.each(["not_run", "vendor_owned"])(
    "keeps %s distinct from failed even when a classified check command completed",
    (status) => {
      const activity = replay([
        ev("tool.started", {
          tool: "exec",
          call_id: "check",
          arguments: { command: "npm test" },
        }),
        ev("tool.completed", { tool: "exec", call_id: "check", success: true }),
        ev("verification.summary", { status, commands: [] }),
        ev("agent.completed", { success: true }),
      ]).activity.t1;
      expect(deriveSteps(activity).find((s) => s.id === "testing")?.state).toBe(
        "incomplete",
      );
    },
  );
  it("retains passed command facts without promoting an interrupted final assessment", () => {
    const activity = replay([
      ev("verification.summary", {
        status: "failed",
        final_assessment: "not_completed",
        commands: [check],
      }),
      ev("agent.completed", { success: false }),
    ]).activity.t1;
    expect(deriveSteps(activity).find((s) => s.id === "testing")).toMatchObject(
      { state: "incomplete", detail: "Final verification did not finish" },
    );
    expect(activity.verification?.commands[0]).toMatchObject({
      state: "passed",
      success: true,
    });
  });
  it("preserves an authoritative passing verdict after the same check was rerun", () => {
    const activity = replay([
      ev("tool.started", {
        tool: "exec",
        call_id: "first",
        arguments: { command: "npm test" },
      }),
      ev("tool.completed", { tool: "exec", call_id: "first", success: false }),
      ev("tool.started", {
        tool: "exec",
        call_id: "second",
        arguments: { command: "npm test" },
      }),
      ev("tool.completed", { tool: "exec", call_id: "second", success: true }),
      ev("verification.summary", {
        status: "passed",
        commands: [
          { ...check, state: "failed", success: false, exit_code: 1 },
          check,
        ],
      }),
      ev("agent.completed", { success: true }),
    ]).activity.t1;
    expect(deriveSteps(activity).find((s) => s.id === "testing")).toMatchObject(
      { state: "done", label: "Checks passed" },
    );
  });
});

it("requires configured evidence before a passed status can create a green check", () => {
  const activity = replay([
    ev("tool.started", {
      tool: "exec",
      call_id: "test",
      arguments: { command: "npm test" },
    }),
    ev("tool.completed", { tool: "exec", call_id: "test", success: true }),
    ev("verification.summary", {
      status: "passed",
      commands: [
        { command: "npm test", kind: "command", success: true, exit_code: 0 },
      ],
    }),
    ev("agent.completed", { success: true }),
  ]).activity.t1;
  expect(
    deriveSteps(activity).find((step) => step.id === "testing"),
  ).toMatchObject({
    state: "incomplete",
    label: "Checks not verified",
    detail: "Verification not run",
  });
});

it("shows freshness assessment as active only while the task is still running", () => {
  const activity = replay([
    ev("verification.summary", {
      status: "checking",
      commands: [
        {
          command: "npm test",
          kind: "configured_check",
          state: "passed",
          success: true,
          exit_code: 0,
        },
      ],
    }),
  ]).activity.t1;
  expect(
    deriveSteps(activity).find((step) => step.id === "testing")?.state,
  ).toBe("active");
  expect(
    deriveSteps({
      ...activity,
      finished: { success: false, cancelled: true, summary: "Stopped" },
    }).find((step) => step.id === "testing")?.state,
  ).toBe("incomplete");
});
