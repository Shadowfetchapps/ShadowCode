import { describe, expect, it } from "vitest";
import type { EventRow } from "../api";
import { replay } from "./transcript";
import { statusLabel } from "./subagents";

const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
): EventRow => ({ id, ts: id, type, payload, task_id: "parent" });

const runs = (items: ReturnType<typeof replay>["items"]) =>
  items.flatMap((item) => (item.kind === "subagent" ? [item.run] : []));

describe("subagent runs in the parent transcript", () => {
  it("adds one card per run and updates it in place when the run finishes", () => {
    const state = replay([
      event(1, "user.message", { text: "Look around" }),
      event(2, "subagent.started", {
        run_id: "r1",
        agent: "explore",
        description: "alpha",
        prompt: "Find alpha",
        mode: "read-only",
        model: "fixture",
        job_id: "j1",
        session_id: "s1",
      }),
      event(3, "subagent.started", {
        run_id: "r2",
        agent: "general",
        prompt: "Edit beta",
        mode: "write",
        job_id: "j2",
        session_id: "s2",
      }),
      event(4, "subagent.finished", {
        run_id: "r1",
        agent: "explore",
        status: "completed",
        summary: "Alpha is in src/a.rs",
        usage: { total_tokens: 120 },
        steps: 2,
        duration_s: 1.5,
      }),
      event(5, "subagent.finished", {
        run_id: "r2",
        agent: "general",
        mode: "write",
        status: "completed",
        summary: "Changed beta",
        files: [
          { path: "b.txt", status: "modified", additions: 1, deletions: 1 },
        ],
        patch: true,
      }),
      event(6, "subagent.applied", { run_id: "r2", paths: ["b.txt"] }),
    ]);
    const [alpha, beta] = runs(state.items);
    expect(runs(state.items)).toHaveLength(2);
    expect(alpha).toMatchObject({
      agent: "explore",
      description: "alpha",
      status: "completed",
      summary: "Alpha is in src/a.rs",
      sessionId: "s1",
      tokens: 120,
      steps: 2,
      durationS: 1.5,
    });
    expect(beta.files).toEqual([
      {
        path: "b.txt",
        status: "modified",
        additions: 1,
        deletions: 1,
        binary: false,
      },
    ]);
    expect(beta.patch && beta.applied).toBe(true);
    // Updated cards keep their row key (so an open card stays open).
    const keys = state.items
      .filter((i) => i.kind === "subagent")
      .map((i) => i.key);
    expect(keys).toEqual(["s:r1", "s:r2"]);
    // The cards keep their place in the conversation.
    expect(state.items.map((i) => i.kind)).toEqual([
      "user",
      "subagent",
      "subagent",
    ]);
  });

  it("labels running, stopped and failed runs", () => {
    const base = runs(
      replay([event(1, "subagent.started", { run_id: "r", agent: "plan" })])
        .items,
    )[0];
    expect(statusLabel(base)).toBe("Working…");
    expect(statusLabel({ ...base, status: "cancelled" })).toBe("Stopped");
    expect(statusLabel({ ...base, status: "interrupted" })).toBe("Interrupted");
    expect(statusLabel({ ...base, status: "failed" })).toBe("Failed");
    expect(statusLabel({ ...base, status: "completed" })).toBe("Done");
  });
});
