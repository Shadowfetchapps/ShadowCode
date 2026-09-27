import { describe, expect, it } from "vitest";
import { applyEvent, emptyTranscript, replay } from "./transcript";
import type { EventRow } from "../api";
const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
  task_id = "one",
): EventRow => ({ id, ts: id, type, payload, task_id });
// Completed tasks also get a summary card; these checks cover the messages.
const messages = (items: ReturnType<typeof replay>["items"]) =>
  items.filter((item) => item.kind !== "summary");
describe("durable transcript", () => {
  it("separates Cursor's earlier permission reason from command output and keeps failures red", () => {
    const output = {
      status: "completed",
      input: { command: "python3 -m unittest -q" },
      content: ["Not in allowlist: python3"],
      cursor_execution: { exit_code: 0 },
      raw_output: { exitCode: 0, stdout: "", stderr: "Ran 5 tests\nOK" },
      raw_output_format: "json",
      raw_output_truncated: false,
      acp_provenance: {
        schema_version: 1,
        history_complete: true,
        content: { source: "permission_request", phase: "pending" },
        raw_output: {
          source: "tool_update",
          phase: "completed",
          explicit_terminal: true,
        },
        permission: {
          state: "resolved",
          decision: "allow_once",
          current_operation_matches: true,
        },
      },
    };
    const rows = [
      event(1, "agent.started", { task: "Test" }),
      event(2, "tool.started", {
        tool: "cursor.execute",
        call_id: "cursor-call",
      }),
      event(3, "tool.completed", {
        tool: "cursor.execute",
        call_id: "cursor-call",
        success: true,
        output,
      }),
    ];
    const result = replay(rows);
    const card = result.items.find((i) => i.kind === "tool");
    expect(card).toMatchObject({ text: "Command result · exit 0", ok: true });
    if (card?.kind !== "tool") throw new Error("missing tool card");
    expect(card.fullOutput).toContain(
      "Earlier approval reason\nNot in allowlist: python3",
    );
    expect(card.fullOutput).toContain(
      "Client permission decision\nApproved once",
    );
    expect(card.fullOutput).toContain("Standard error\nRan 5 tests\nOK");
    const failed = replay([
      ...rows.slice(0, 2),
      event(3, "tool.completed", {
        tool: "cursor.execute",
        call_id: "cursor-call",
        success: true,
        output: {
          ...output,
          cursor_execution: { exit_code: 7 },
          raw_output: "clipped output",
          raw_output_format: "json_text_preview",
          raw_output_truncated: true,
        },
      }),
    ]);
    expect(failed.items.find((i) => i.kind === "tool")).toMatchObject({
      ok: false,
      text: "Command result · exit 7",
    });
    expect(failed.activity.one.calls[0].ok).toBe(false);
    expect(failed.activity.one.calls[0].output).toContain(
      "Raw output preview (truncated)",
    );
    const historical = replay([
      ...rows.slice(0, 2),
      event(3, "tool.completed", {
        tool: "cursor.execute",
        call_id: "cursor-call",
        success: true,
        output: {
          ...output,
          acp_provenance: undefined,
          cursor_execution: undefined,
        },
      }),
    ]);
    const oldCard = historical.items.find((i) => i.kind === "tool");
    if (oldCard?.kind !== "tool") throw new Error("missing old tool card");
    expect(oldCard.fullOutput).not.toContain("Earlier approval reason");
    expect(oldCard.fullOutput).toContain("Not in allowlist: python3");
  });
  it("preserves running state during queue changes and moves each prompt to its execution turn", () => {
    const started = replay([
      event(1, "user.message", { text: "First" }),
      event(2, "agent.started", { task: "First" }),
      event(3, "plan.updated", {
        plan: { steps: [{ id: "one", title: "Work", status: "running" }] },
      }),
      event(4, "user.message", { text: "Second" }, "two"),
      event(5, "user.message", { text: "Cancel me" }, "three"),
      event(
        6,
        "agent.completed",
        {
          summary: "Queued task cancelled",
          cancelled: true,
          plan: { steps: [] },
          usage: { total_tokens: 0 },
        },
        "three",
      ),
    ]);
    expect(started.stage).toBe("UNDERSTAND");
    expect(started.activeTaskId).toBe("one");
    expect(started.plan).toHaveLength(1);
    const completed = applyEvent(
      started,
      event(7, "agent.completed", {
        summary: "First result",
        success: true,
        usage: { total_tokens: 30 },
      }),
    );
    expect(completed.usage.total_tokens).toBe(30);
    const next = applyEvent(
      completed,
      event(8, "agent.started", { task: "Second" }, "two"),
    );
    expect(next.items.at(-1)).toMatchObject({
      kind: "user",
      text: "Second",
      taskId: "two",
    });
    expect(
      next.items.filter(
        (item) => item.kind === "user" && item.taskId === "two",
      ),
    ).toHaveLength(1);
    expect(next.activeTaskId).toBe("two");
    expect(next.usage).toEqual({});
    expect(next.plan).toEqual([]);
  });
  it("keeps a final answer once when completion hooks follow it, without hiding a later task or failure", () => {
    const rows = [
      event(1, "model.delta", { text: "Done", message_id: "final" }),
      event(2, "hook.completed", {
        id: "completion",
        name: "verify",
        event: "on_complete",
        status: "passed",
        success: true,
      }),
      event(3, "agent.completed", { summary: "Done", success: true }),
    ];
    const first = replay(rows);
    expect(first.items.filter((item) => item.kind === "agent")).toHaveLength(1);
    expect(rows.reduce(applyEvent, first)).toEqual(first);
    const second = applyEvent(
      first,
      event(4, "agent.completed", { summary: "Done", success: true }, "two"),
    );
    expect(second.items.filter((item) => item.kind === "agent")).toHaveLength(
      2,
    );
    const failed = replay([
      rows[0],
      rows[1],
      event(3, "agent.completed", { summary: "Done", success: false }),
    ]);
    expect(messages(failed.items).at(-1)).toMatchObject({
      kind: "agent",
      who: "Needs attention",
      taskId: "one",
    });
  });
  it("keeps interleaved hook checks separate from the tool they gate and replays their result once", () => {
    const rows = [
      event(1, "tool.started", { tool: "exec", call_id: "exec-one" }),
      event(2, "hook.started", {
        id: "check-one",
        name: "lint",
        event: "before_command",
        command: "lint",
        path: ".shadowcode/hooks/lint.yaml",
      }),
      event(3, "hook.completed", {
        id: "check-one",
        name: "lint",
        event: "before_command",
        command: "lint",
        path: ".shadowcode/hooks/lint.yaml",
        status: "failed",
        success: false,
        detail: "Exited with status 2",
        process: { stderr: "Fix this error", truncated: true },
      }),
      event(4, "tool.completed", {
        tool: "exec",
        call_id: "exec-one",
        success: false,
        error: "Action blocked by lifecycle command",
      }),
    ];
    const state = replay(rows);
    expect(state.items).toHaveLength(2);
    expect(state.items[0]).toMatchObject({
      kind: "tool",
      tool: "exec",
      ok: false,
      live: false,
    });
    expect(state.items[1]).toMatchObject({
      kind: "tool",
      tool: "hook",
      ok: false,
      live: false,
      headline: "Hook · lint",
      path: ".shadowcode/hooks/lint.yaml",
    });
    expect(state.items[1]).toHaveProperty(
      "fullOutput",
      "before_command\nlint\nExited with status 2\nFix this error\n[Output truncated]",
    );
    expect(rows.reduce(applyEvent, state)).toEqual(state);
  });
  it("replays selected workflow provenance and command cards without duplicates", () => {
    const rows = [
      event(1, "workflow.selected", {
        name: "audit",
        path: ".agents/skills/audit/SKILL.md",
        mode: "code",
        effective_mode: "review",
      }),
      event(2, "command.completed", {
        name: "run",
        result: { kind: "error", headline: "Command failed", body: "Exit: 7" },
      }),
    ];
    const state = replay(rows);
    expect(state.items[0]).toMatchObject({
      kind: "note",
      text: "Workflow /audit · .agents/skills/audit/SKILL.md · review",
    });
    expect(state.items[1]).toMatchObject({
      kind: "command",
      card: { kind: "error", body: "Exit: 7" },
    });
    expect(state.items.filter((item) => item.kind === "agent")).toHaveLength(0);
    expect(rows.reduce(applyEvent, state)).toEqual(state);
  });

  it("retains readable fallback notices from legacy conversations", () => {
    const state = replay([
      event(1, "routing.fallback", {
        purpose: "coder",
        requested: "missing",
        fallback: "local-model",
      }),
    ]);
    expect(state.items[0].text).toBe(
      "Using default: local-model · coder. The saved model is unavailable.",
    );
    expect(state.routing).toBeUndefined();
  });
  it("replays routing and fallback notices separately from model answers", () => {
    const rows = [
      event(1, "agent.started", { task: "Review the changes" }),
      event(2, "routing.fallback", {
        purpose: "reviewer",
        source: "fallback",
        model_id: "default",
        model_name: "local-coder",
        provider: "ollama",
        context_limit: 4096,
        fallback_reason: "Saved model is not registered",
      }),
      event(3, "model.delta", { text: "Review complete" }),
    ];
    const state = replay(rows);
    expect(state.items[1]).toMatchObject({ kind: "note", warning: true });
    expect(state.items[1].text).toContain(
      "Using default: local-coder · ollama · reviewer",
    );
    expect(state.items[1].text).toContain("not registered");
    expect(state.items.filter((item) => item.kind === "agent")).toHaveLength(1);
    expect(state.routing?.context_limit).toBe(4096);
    expect(rows.reduce(applyEvent, state)).toEqual(state);
    expect(
      applyEvent(state, event(4, "agent.started", { task: "Next task" }, "two"))
        .routing,
    ).toBeUndefined();
  });
  it("joins native stream chunks and replaces them with one complete response", () => {
    const state = replay([
      event(1, "user.message", { text: "Read the project" }),
      event(2, "agent.started", { task: "Read the project" }),
      event(3, "model.stream", { text: "Hello ", message_id: "reply" }),
      event(4, "model.stream", { text: "there", message_id: "reply" }),
      event(5, "model.delta", { text: "Hello there.", message_id: "reply" }),
      event(6, "agent.completed", { summary: "Hello there.", success: true }),
    ]);
    expect(messages(state.items).map((i) => i.text)).toEqual([
      "Read the project",
      "Hello there.",
    ]);
    expect(state.items[1]).toMatchObject({ messageId: "reply", live: false });
  });
  it("keeps a stopped partial response without an alarming label", () => {
    const state = replay([
      event(1, "model.stream", { text: "Partial", message_id: "reply" }),
      event(2, "model.stream_end", { message_id: "reply", complete: false }),
      event(3, "agent.completed", {
        summary: "Task cancelled",
        cancelled: true,
        success: false,
      }),
    ]);
    expect(state.items[0]).toMatchObject({
      text: "Partial",
      live: false,
    });
    expect(state.items[0]).not.toHaveProperty("who");
    expect(state.stage).toBe("CANCELLED");
  });
  it("retains native tool paths and structured output for review cards", () => {
    const state = replay([
      event(1, "tool.started", {
        tool: "write_file",
        call_id: "write",
        arguments: { path: "src/main.rs" },
      }),
      event(2, "tool.completed", {
        tool: "write_file",
        call_id: "write",
        success: true,
        output: { path: "src/main.rs", bytes: 42 },
      }),
    ]);
    expect(state.items[0]).toMatchObject({
      path: "src/main.rs",
      ok: true,
      fullOutput: JSON.stringify({ path: "src/main.rs", bytes: 42 }, null, 2),
    });
  });
  it("finalizes generic vendor calls with the completed typed tool input in the same task", () => {
    const started = replay([
      event(1, "tool.started", { tool: "grok.tool", call_id: "shared" }),
      event(
        2,
        "tool.started",
        {
          tool: "grok.tool",
          call_id: "shared",
          arguments: { command: "other task" },
        },
        "two",
      ),
    ]);
    const originalKey = started.items[0].key;
    const completed = applyEvent(
      started,
      event(3, "tool.completed", {
        tool: "grok.execute",
        call_id: "shared",
        success: true,
        output: {
          title: "A display label is not the executed command",
          tool_kind: "execute",
          input: { command: ["python3", "-m", "unittest", "-q"] },
        },
      }),
    );
    expect(completed.items).toHaveLength(2);
    expect(completed.items[0]).toMatchObject({
      key: originalKey,
      kind: "tool",
      taskId: "one",
      callId: "shared",
      tool: "grok.execute",
      headline: "grok.execute",
      live: false,
      ok: true,
    });
    expect(completed.activity.one.calls).toHaveLength(1);
    expect(completed.activity.one.calls[0]).toMatchObject({
      callId: "shared",
      tool: "grok.execute",
      label: "grok.execute",
      step: "testing",
      command: "python3 -m unittest -q",
      live: false,
    });
    expect(completed.activity.two).toEqual(started.activity.two);
    expect(completed.items[1]).toEqual(started.items[1]);
  });
  it("replaces an initial vendor command and category when typed completion input changes", () => {
    const state = replay([
      event(1, "tool.started", {
        tool: "grok.execute",
        call_id: "check",
        arguments: { command: "python3 -m unittest -q" },
      }),
      event(2, "tool.completed", {
        tool: "grok.execute",
        call_id: "check",
        success: true,
        output: { input: { command: "pwd" }, title: "Run tests" },
      }),
    ]);
    expect(state.items).toHaveLength(1);
    expect(state.activity.one.calls).toHaveLength(1);
    expect(state.activity.one.calls[0]).toMatchObject({
      step: "commands",
      command: "pwd",
      live: false,
    });
  });
  it("does not retain an earlier command when completed vendor input explicitly clears it", () => {
    const state = replay([
      event(1, "tool.started", {
        tool: "grok.execute",
        call_id: "check",
        arguments: { command: "npm test" },
      }),
      event(2, "tool.completed", {
        tool: "grok.execute",
        call_id: "check",
        success: true,
        output: { input: {}, title: "npm test" },
      }),
    ]);
    expect(state.activity.one.calls).toHaveLength(1);
    expect(state.activity.one.calls[0].command).toBeUndefined();
    expect(state.activity.one.calls[0].step).toBe("commands");
  });
  it("keeps the observed category when a completion has no typed input", () => {
    const state = replay([
      event(1, "tool.started", {
        tool: "claude.Bash",
        call_id: "check",
        arguments: { input: { command: "python3 -m unittest -q" } },
      }),
      event(2, "tool.completed", {
        tool: "claude.Bash",
        call_id: "check",
        success: true,
        output: {
          output: "Output mentions pwd; this is not new command input",
        },
      }),
    ]);
    expect(state.activity.one.calls).toHaveLength(1);
    expect(state.activity.one.calls[0]).toMatchObject({
      step: "testing",
      label: "claude.Bash",
      live: false,
    });
  });
  it.each(["CommandLine", "command_line"])(
    "uses Antigravity's typed %s only for Antigravity tools",
    (field) => {
      for (const vendor of ["antigravity", "grok"]) {
        const state = replay([
          event(1, "tool.started", {
            tool: `${vendor}.tool`,
            call_id: "check",
          }),
          event(2, "tool.completed", {
            tool: `${vendor}.execute`,
            call_id: "check",
            success: true,
            output: {
              input: { [field]: "python3 -m unittest -q" },
              title: "A descriptive title",
            },
          }),
        ]);
        expect(state.activity.one.calls).toHaveLength(1);
        expect(state.activity.one.calls[0]).toMatchObject({
          tool: `${vendor}.execute`,
          command:
            vendor === "antigravity" ? "python3 -m unittest -q" : undefined,
          step: vendor === "antigravity" ? "testing" : "commands",
          live: false,
        });
      }
    },
  );
  it("replays every task once when the event stream reconnects", () => {
    const rows = [
      event(1, "agent.started", { task: "Make it work" }),
      event(2, "model.delta", { text: "Inspecting files" }),
      event(3, "agent.completed", { summary: "Finished", success: true }),
    ];
    const state = replay(rows);
    expect(rows.reduce(applyEvent, state)).toEqual(state);
    expect(messages(state.items).map((i) => i.text)).toEqual([
      "Make it work",
      "Inspecting files",
      "Finished",
    ]);
  });
  it("refreshes the picker for vendor usage pushes but not per-task usage", () => {
    const state = replay([
      event(1, "usage.updated", {
        purpose: "turn",
        turn: { prompt_tokens: 20 },
        job: { prompt_tokens: 20 },
        session: { prompt_tokens: 20 },
      }),
    ]);
    expect(state.usageVersion).toBe(0);
    const pushed = applyEvent(
      state,
      event(2, "usage.updated", { vendor: "codex", usage: {} }),
    );
    expect(pushed.usageVersion).toBe(1);
  });
  it("does not duplicate a final model response", () => {
    const items = replay([
      event(1, "model.delta", { text: "Done" }),
      event(2, "agent.completed", { summary: "Done", success: true }),
    ]).items;
    expect(messages(items)).toHaveLength(1);
    expect(items.filter((item) => item.kind === "summary")).toHaveLength(1);
  });
  it("matches simultaneous calls of the same tool by call ID", () => {
    const state = replay([
      event(1, "tool.started", { tool: "read_file", call_id: "a" }),
      event(2, "tool.started", { tool: "read_file", call_id: "b" }),
      event(3, "tool.completed", {
        tool: "read_file",
        call_id: "b",
        success: true,
        output_full: "second",
      }),
      event(4, "tool.completed", {
        tool: "read_file",
        call_id: "a",
        success: true,
        output_full: "first",
      }),
    ]);
    expect(state.items.map((i) => i.kind === "tool" && i.fullOutput)).toEqual([
      "first",
      "second",
    ]);
  });
  it("surfaces local context and autonomy notes without claiming verification", () => {
    const state = replay([
      event(1, "context.budget", {
        used_estimated_tokens: 1200,
        limit: 8000,
      }),
      event(2, "context.budget", {
        used_estimated_tokens: 7000,
        limit: 8000,
      }),
      event(3, "autonomy.budget", { ratio: 0.8, max_steps: 64 }),
      event(4, "runaway.warning", {
        action: "replan",
        tool: "read_file",
        repeats: 4,
      }),
      event(5, "runaway.warning", {
        kind: "assistant_text",
        action: "pause",
        repeats: 5,
      }),
      event(6, "runaway.warning", {
        kind: "prose_command",
        action: "pause",
        repeats: 5,
      }),
    ]);
    const text = state.items.map((item) => item.text).join("\n");
    // A routine budget report stays out of the conversation; a nearly full
    // context is surfaced.
    expect(text).not.toMatch(/1200\/8000/);
    expect(text).toMatch(/Context nearly full: 7000\/8000/);
    expect(state.items.some((item) => /Autonomy budget/.test(item.text))).toBe(
      true,
    );
    expect(state.items.some((item) => /Loop replan/.test(item.text))).toBe(
      true,
    );
    expect(
      state.items.some((item) => /assistant text repeated/.test(item.text)),
    ).toBe(true);
    expect(
      state.items.some((item) =>
        /described a command without calling a tool/.test(item.text),
      ),
    ).toBe(true);
  });
  it("shows unfinished-action retries without inventing completion or tool evidence", () => {
    const state = replay([
      event(1, "completion.retry", {
        reason: "unperformed_action",
        attempt: 1,
      }),
    ]);
    expect(state.items).toHaveLength(1);
    expect(state.items[0]).toMatchObject({
      kind: "note",
      text: "Continuing: the previous response described work that still needs to be done.",
    });
    expect(state.items.some((item) => item.kind === "summary")).toBe(false);
    expect(state.items.some((item) => item.kind === "tool")).toBe(false);
  });
  it("replays 10000 stream events without dropping the last cursor", () => {
    const started = performance.now();
    let state = emptyTranscript();
    for (let i = 1; i <= 10000; i++)
      state = applyEvent(state, event(i, "model.delta", { text: String(i) }));
    expect(state.cursor).toBe(10000);
    expect(state.items).toHaveLength(10000);
    expect(state.items[9999].text).toBe("10000");
    expect(performance.now() - started).toBeLessThan(4000);
  });
  it("continues well past the former 800-event boundary", () => {
    let state = emptyTranscript();
    for (let i = 1; i <= 1600; i++)
      state = applyEvent(state, event(i, "model.delta", { text: String(i) }));
    expect(state.cursor).toBe(1600);
    expect(state.items).toHaveLength(1600);
  });
  it("records stopped and failed runs without claiming completion", () => {
    expect(
      replay([event(1, "agent.completed", { cancelled: true, success: false })])
        .stage,
    ).toBe("CANCELLED");
    expect(
      replay([event(1, "agent.completed", { success: false })]).stage,
    ).toBe("FAILED");
  });
});

describe("subscription turns", () => {
  it("also shows older vendor answers once, without a stream end", () => {
    const text = "Today is Thursday.";
    const state = replay([
      event(1, "model.stream", { text, message_id: "m1" }),
      event(2, "agent.completed", { summary: text, success: true }),
    ]);
    expect(
      state.items.filter((i) => i.kind === "agent" && i.text === text),
    ).toHaveLength(1);
  });
  it("shows a vendor's answer once, not again as the result", () => {
    const text = "Today is **Thursday, September 24, 2026**.";
    const state = replay([
      event(1, "routing.selected", {
        inference: "cloud",
        model_id: "cli:cursor:auto",
        model_name: "auto",
        provider: "cli:cursor",
      }),
      event(2, "model.stream", { text, message_id: "m1" }),
      event(3, "model.stream_end", { message_id: "m1", complete: true }),
      event(4, "agent.completed", { summary: text, success: true }),
    ]);
    const answers = state.items.filter(
      (item) => item.kind === "agent" && item.text === text,
    );
    expect(answers).toHaveLength(1);
    expect(state.items.some((i) => "who" in i && i.who === "Result")).toBe(
      false,
    );
    // The note names the product the picker showed.
    expect(
      state.items.some(
        (i) => i.kind === "note" && i.text === "Using Cursor · Auto · Cloud",
      ),
    ).toBe(true);
  });
});

describe("plan limit fallback (limit.fallback)", () => {
  const limited = [
    event(1, "user.message", { text: "Fix the add function" }, "a"),
    event(2, "agent.started", { task: "Fix the add function" }, "a"),
    event(3, "limit.reached", { vendor: "codex" }, "a"),
    event(
      4,
      "agent.completed",
      {
        success: false,
        cancelled: false,
        summary: "Codex plan limit reached: weekly limit.",
        limit_reached: { vendor: "codex", detail: "weekly limit" },
      },
      "a",
    ),
  ];
  const followUp =
    "Continue where Codex stopped when its plan limit was reached. The request was:\n\nFix the add function";

  it("continued automatically: a note above the follow-up, which is labelled", () => {
    const state = replay([
      ...limited,
      // The engine records the follow-up's prompt before the fallback note.
      event(5, "user.message", { text: followUp }, "b"),
      event(
        6,
        "limit.fallback",
        {
          ok: true,
          from: "Codex",
          to: "qwen3:14b",
          target: "local:gguf:qwen",
          job_id: "j2",
        },
        "a",
      ),
      event(7, "agent.started", { task: followUp, job_id: "j2" }, "b"),
    ]);
    const kinds = state.items.map((item) => item.kind);
    const note = state.items.findIndex((item) => item.kind === "limit");
    const user = state.items.findIndex(
      (item) => item.kind === "user" && item.taskId === "b",
    );
    expect(note).toBeGreaterThan(-1);
    expect(note).toBe(user - 1);
    expect(state.items[note]).toMatchObject({
      mode: "continued",
      text: "Codex reached its plan limit. Continuing on qwen3:14b on this computer.",
    });
    expect(state.items[user]).toMatchObject({
      text: followUp,
      continued: "auto",
    });
    // The plain limit note is replaced; the original prompt is not labelled.
    expect(
      state.items.some((i) => i.kind === "note" && /Plan limit/.test(i.text)),
    ).toBe(false);
    expect(
      state.items.find((i) => i.kind === "user" && i.taskId === "a"),
    ).not.toHaveProperty("continued");
    expect(kinds.filter((k) => k === "summary")).toHaveLength(1);
    expect(state.fallback).toEqual({
      jobId: "j2",
      target: "local:gguf:qwen",
      to: "qwen3:14b",
    });
    expect(state.limit).toBeUndefined();
    expect(state.activity.a.finished?.limitReached).toBe("Codex");
    expect(
      state.items.find((i) => i.kind === "agent" && i.taskId === "a"),
    ).toMatchObject({ who: "Plan limit reached" });
  });

  it("ask: a card that a manual continuation resolves", () => {
    const asked = replay([
      ...limited,
      event(5, "limit.fallback", { ok: false, ask: true }, "a"),
    ]);
    const card = asked.items.find((i) => i.kind === "limit");
    expect(card).toMatchObject({
      mode: "ask",
      from: "Codex",
      text: "Codex reached its plan limit.",
      request: "Fix the add function",
    });
    expect(card).not.toHaveProperty("resolved");
    // The card carries the actions; the banner is not repeated.
    expect(asked.limit).toBeUndefined();
    const continued = [
      ...limited,
      event(5, "limit.fallback", { ok: false, ask: true }, "a"),
      event(6, "user.message", { text: followUp }, "b"),
      event(7, "agent.started", { task: followUp }, "b"),
    ];
    const state = replay(continued);
    expect(state.items.find((i) => i.kind === "limit")).toMatchObject({
      resolved: true,
    });
    expect(
      state.items.find((i) => i.kind === "user" && i.taskId === "b"),
    ).toMatchObject({ continued: "manual" });
    // Any other task started later also closes the offer.
    const other = replay([
      ...limited,
      event(5, "limit.fallback", { ok: false, ask: true }, "a"),
      event(6, "user.message", { text: "Something else" }, "c"),
      event(7, "agent.started", { task: "Something else" }, "c"),
    ]);
    expect(other.items.find((i) => i.kind === "limit")).toMatchObject({
      resolved: true,
    });
    expect(
      other.items.find((i) => i.kind === "user" && i.taskId === "c"),
    ).not.toHaveProperty("continued");
  });

  it("no local model: a warning with the reason, and the banner stays", () => {
    const state = replay([
      ...limited,
      event(
        5,
        "limit.fallback",
        {
          ok: false,
          from: "Codex",
          reason:
            "No local model is ready. Add one in Settings › Local models.",
        },
        "a",
      ),
    ]);
    expect(state.items.find((i) => i.kind === "limit")).toMatchObject({
      mode: "unavailable",
      text: "Codex reached its plan limit. No local model is ready. Add one in Settings › Local models.",
    });
    expect(state.limit?.vendor).toBe("Codex");
  });
});
