import { describe, expect, it } from "vitest";
import { replay, tryOnFor } from "./transcript";
import type { EventRow } from "../api";
import type { ChatItem } from "../components/cards";

const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
  task_id = "one",
): EventRow => ({ id, ts: 1_790_000_000 + id, type, payload, task_id });

const of = <K extends ChatItem["kind"]>(items: ChatItem[], kind: K) =>
  items.filter(
    (item): item is Extract<ChatItem, { kind: K }> => item.kind === kind,
  );

describe("spending limits in the transcript", () => {
  const card = {
    id: "p1",
    job_id: "job-1",
    kind: "task",
    limit: 1,
    spent: 1.02,
    raise_to: 2,
    title: "This task reached its spending limit",
    text: "It has spent $1.02 on paid models, and the limit for one task is $1.00. It is paused between steps; nothing is running.",
    continue_label: "Continue (limit raised to $2.00)",
  };

  it("shows a calm notice, then a card that the answer closes", () => {
    const start = [
      event(1, "user.message", { text: "Fix it" }),
      event(2, "agent.started", { job_id: "job-1", task: "Fix it" }),
      event(3, "spend.notice", {
        kind: "task",
        text: "This task has spent $0.80 of its $1.00 limit on paid models.",
      }),
      event(4, "spend.limit_reached", card),
    ];
    const waiting = replay(start);
    const note = of(waiting.items, "note").find((n) =>
      n.text.startsWith("This task has spent"),
    );
    expect(note?.warning).toBeFalsy();
    const [open] = of(waiting.items, "spend");
    expect(open).toMatchObject({
      jobId: "job-1",
      promptId: "p1",
      limitKind: "task",
      continueLabel: "Continue (limit raised to $2.00)",
    });
    expect(open.resolved).toBeUndefined();
    // Replaying the same card twice keeps one.
    expect(
      of(
        replay([...start, event(5, "spend.limit_reached", card)]).items,
        "spend",
      ),
    ).toHaveLength(1);
    const answered = replay([
      ...start,
      event(6, "spend.limit_resolved", {
        prompt_id: "p1",
        action: "continue",
        limit: 2,
        text: "Continuing. This task's limit is now $2.00.",
      }),
    ]);
    const [closed] = of(answered.items, "spend");
    expect(closed.resolved).toBe("continue");
    expect(closed.outcome).toBe("Continuing. This task's limit is now $2.00.");
    const lifted = replay([
      ...start,
      event(6, "spend.limit_resolved", {
        prompt_id: "p1",
        action: "continue",
        reason: "limit_changed",
      }),
    ]);
    expect(of(lifted.items, "spend")[0].resolved).toBe("lifted");
    // A task that ends while the card waits closes it.
    const ended = replay([
      ...start,
      event(7, "agent.completed", {
        success: false,
        cancelled: true,
        summary: "Stopped at your per-task spending limit.",
      }),
    ]);
    expect(of(ended.items, "spend")[0].resolved).toBe("ended");
  });

  it("says once when a price is unknown", () => {
    const state = replay([
      event(1, "spend.unknown", {
        text: "The price of acme/coder isn't known, so what this task spends on it can't be counted toward your spending limits.",
      }),
    ]);
    expect(of(state.items, "note")[0].text).toContain("isn't known");
  });
});

describe("provider retries", () => {
  it("updates one line while retrying and says when the provider answered", () => {
    const retrying = replay([
      event(1, "agent.started", { job_id: "j" }),
      event(2, "model.retry", {
        attempt: 1,
        max_attempts: 5,
        reason: "rate_limited",
        delay_ms: 2000,
      }),
      event(3, "model.retry", {
        attempt: 2,
        max_attempts: 5,
        reason: "overloaded",
        delay_ms: 4000,
      }),
    ]);
    const notes = of(retrying.items, "note").filter((n) => n.retry);
    expect(notes).toHaveLength(1);
    expect(notes[0].text).toBe("Provider busy, retrying (2 of 5) in 4 s…");
    const answered = replay([
      event(1, "agent.started", { job_id: "j" }),
      event(2, "model.retry", {
        attempt: 1,
        max_attempts: 3,
        reason: "disconnected",
        delay_ms: 500,
      }),
      event(3, "model.stream", { text: "Hello", message_id: "m" }),
    ]);
    const done = of(answered.items, "note").filter((n) => n.retry);
    expect(done[0].text).toBe(
      "The provider was busy; it answered after 1 retry.",
    );
    // A later retry in the same task starts a new line.
    const again = replay([
      event(1, "agent.started", { job_id: "j" }),
      event(2, "model.retry", { attempt: 1, max_attempts: 3, delay_ms: 1000 }),
      event(3, "model.delta", { text: "Hi", message_id: "m" }),
      event(4, "model.retry", { attempt: 1, max_attempts: 3, delay_ms: 1000 }),
    ]);
    expect(of(again.items, "note").filter((n) => n.retry)).toHaveLength(2);
  });

  it("throws away a reply cut off by a dropped connection", () => {
    const state = replay([
      event(1, "agent.started", { job_id: "j" }),
      event(2, "model.stream", { text: "Half an ans", message_id: "m1" }),
      event(3, "model.stream_end", { message_id: "m1", complete: false }),
      event(4, "model.retry", {
        attempt: 1,
        max_attempts: 3,
        reason: "disconnected",
        delay_ms: 500,
        discard_message_id: "m1",
      }),
      event(5, "model.stream", { text: "The whole answer.", message_id: "m2" }),
      event(6, "model.delta", {
        text: "The whole answer.",
        message_id: "m2",
        complete: true,
      }),
    ]);
    const replies = of(state.items, "agent");
    expect(replies.map((r) => r.text)).toEqual(["The whole answer."]);
  });

  it("closes the retry line when the retries run out or the answer has no text", () => {
    const retrying = [
      event(1, "agent.started", { job_id: "j" }),
      event(2, "model.retry", {
        attempt: 3,
        max_attempts: 3,
        reason: "overloaded",
        delay_ms: 4000,
      }),
    ];
    const failed = replay([
      ...retrying,
      event(3, "model.request_timing", { success: false }),
      event(4, "agent.completed", {
        success: false,
        summary: "Model provider returned HTTP 503",
      }),
    ]);
    const [gaveUp] = of(failed.items, "note").filter((n) => n.retry);
    expect(gaveUp.text).toBe(
      "The provider still didn't answer after 3 retries.",
    );
    expect(gaveUp.retry?.done).toBe(true);
    const stopped = replay([
      ...retrying,
      event(3, "agent.completed", { success: false, cancelled: true }),
    ]);
    expect(of(stopped.items, "note").filter((n) => n.retry)[0].text).toBe(
      "Stopped before the provider answered.",
    );
    // A retried request that answered with tool calls only streams no text.
    const toolsOnly = replay([
      ...retrying,
      event(3, "model.request_timing", { success: true }),
    ]);
    expect(of(toolsOnly.items, "note").filter((n) => n.retry)[0].text).toBe(
      "The provider was busy; it answered after 3 retries.",
    );
  });
});

describe("plan limits, resumes and Try on", () => {
  const limited = [
    event(1, "user.message", { text: "Fix the add function" }),
    event(2, "agent.started", {
      job_id: "job-1",
      task: "Fix the add function",
    }),
    event(3, "limit.reached", {
      vendor: "codex",
      job_id: "job-1",
      resets_at: 1_790_010_000,
      usage: { windows: [] },
    }),
    event(4, "agent.completed", {
      success: false,
      cancelled: false,
      summary: "Codex plan limit reached",
      limit_reached: { vendor: "codex" },
      run: {
        model_id: "cli:codex",
        model: "gpt-5",
        provider: "cli:codex",
        route: "vendor_cli",
        vendor: "Codex",
        vendor_version: "codex-cli 0.158.0",
        effort: "high",
        app_version: "1.0.0",
        settings_hash: "abc123abc123",
        rules_hash: null,
      },
    }),
    event(5, "limit.fallback", { ok: false, ask: true, from: "Codex" }),
  ];

  it("keeps the limited job and reset time on the card and the run record", () => {
    const state = replay(limited);
    const [card] = of(state.items, "limit");
    expect(card).toMatchObject({
      mode: "ask",
      jobId: "job-1",
      resetsAt: 1_790_010_000,
      request: "Fix the add function",
    });
    expect(state.activity.one.run?.vendor_version).toBe("codex-cli 0.158.0");
    expect(tryOnFor(state, "one")).toEqual({
      taskId: "one",
      from: "Codex",
      request: "Fix the add function",
    });
  });

  it("follows a scheduled resume from waiting to started or cancelled", () => {
    const scheduled = [
      ...limited,
      event(6, "resume.scheduled", {
        resume_id: "r1",
        at: 1_790_010_000,
        target: "cli:codex",
        label: "Codex",
      }),
    ];
    const waiting = replay(scheduled);
    const [resume] = of(waiting.items, "resume");
    expect(resume.state).toBe("scheduled");
    expect(resume.text).toMatch(
      /^Will resume on Codex at .+, when its plan limit resets\.$/,
    );
    expect(of(waiting.items, "limit")[0].resumeScheduled).toBe(true);
    const started = replay([
      ...scheduled,
      event(7, "resume.started", {
        resume_id: "r1",
        at: 1_790_010_000,
        label: "Codex",
        job_id: "job-2",
      }),
    ]);
    const rows = of(started.items, "resume");
    expect(rows).toHaveLength(1);
    expect(rows[0].text).toBe("Resumed on Codex as scheduled.");
    expect(of(started.items, "limit")[0].resumeScheduled).toBe(false);
    const cancelled = replay([
      ...scheduled,
      event(7, "resume.cancelled", { resume_id: "r1", label: "Codex" }),
    ]);
    expect(of(cancelled.items, "resume")[0].state).toBe("cancelled");
    const consent = replay([
      ...scheduled,
      event(7, "resume.needs_consent", {
        resume_id: "r1",
        label: "Codex",
        target: "cli:codex",
        task: "Continue where Codex stopped when its plan limit was reached. The request was:\n\nFix the add function",
      }),
    ]);
    expect(of(consent.items, "resume")[0]).toMatchObject({
      state: "needs_consent",
      target: "cli:codex",
    });
  });

  it("keeps the limited task's mode and web access on the resume card", () => {
    const [ask] = of(
      replay([
        ...limited,
        event(6, "resume.needs_consent", {
          resume_id: "r1",
          label: "Codex",
          target: "cli:codex",
          mode: "review",
          web: true,
          task: "Continue where Codex stopped.",
        }),
      ]).items,
      "resume",
    );
    expect(ask).toMatchObject({ mode: "ask", web: true });
    const [plan] = of(
      replay([
        ...limited,
        event(6, "resume.scheduled", {
          resume_id: "r1",
          at: 1_790_010_000,
          label: "Codex",
          target: "cli:codex",
          mode: "plan",
          web: false,
        }),
      ]).items,
      "resume",
    );
    expect(plan).toMatchObject({ mode: "plan", web: false });
    expect(plan.text).not.toContain("at tomorrow");
  });
});
