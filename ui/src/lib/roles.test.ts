import { describe, expect, it } from "vitest";
import type { EventRow, RolesView, RoleTarget } from "../api";
import {
  parseRolesFinished,
  pipelineLine,
  roleCost,
  rolesAsking,
  rolesBlocked,
  stageStatus,
  verdictLabel,
} from "./roles";
import { replay } from "./transcript";

const target = (over: Partial<RoleTarget>): RoleTarget => ({
  role: "plan",
  label: "Plan",
  setting: "",
  name: "Qwen3 14B",
  local: true,
  runner: "shadowcode",
  cost: "local",
  ...over,
});

const view = (roles: Partial<RolesView["roles"]>): RolesView => ({
  workspace: "/p",
  setup: {
    pipeline: true,
    plan: "",
    implement: "",
    review: "",
    explore: "",
    preset: "",
  },
  roles: {
    plan: target({ role: "plan" }),
    implement: target({ role: "implement", label: "Implement" }),
    review: target({ role: "review", label: "Review" }),
    explore: target({ role: "explore", label: "Explore" }),
    ...roles,
  },
  presets: [],
  conversation: { id: "local:gguf:q", name: "Qwen3 14B", local: true },
  offline: false,
  consented: [],
});

describe("roles", () => {
  it("names each role's cost plainly", () => {
    expect(roleCost("local")).toBe("$0 · local");
    expect(roleCost("subscription")).toBe("Subscription");
    expect(roleCost("api", { cost_usd: 0.1234 })).toBe("$0.12");
    expect(roleCost("api", { cost_usd: 0.004, cost_estimated: true })).toBe(
      "$0.004 est.",
    );
    expect(roleCost("api")).toBe("API key · billed per token");
    expect(roleCost(undefined)).toBe("");
  });

  it("summarises the pipeline and what stops or asks", () => {
    const roles = view({
      plan: target({
        role: "plan",
        name: "Claude Code",
        runner: "vendor",
        cost: "subscription",
        local: false,
        needs_consent: true,
      }),
      implement: target({
        role: "implement",
        name: "Codex",
        runner: "vendor",
        local: false,
        needs_consent: true,
      }),
      review: target({ role: "review", skipped: true }),
    });
    expect(pipelineLine(roles)).toBe("Plan: Claude Code · Implement: Codex");
    expect(rolesAsking(roles)).toEqual(["Claude Code", "Codex"]);
    expect(rolesBlocked(roles)).toBeNull();
    const offline = view({
      plan: target({ role: "plan", blocked: "Offline mode: the plan role…" }),
      // Explore is not a step of the pipeline.
      explore: target({ role: "explore", blocked: "ignored" }),
    });
    expect(rolesBlocked(offline)).toBe("Offline mode: the plan role…");
    expect(pipelineLine(null)).toBe("");
  });

  it("reads a finished task's roles", () => {
    const summary = parseRolesFinished({
      stages: [
        {
          role: "plan",
          name: "Claude Code",
          status: "completed",
          runner: "vendor",
          cost: "subscription",
          usage: { total_tokens: 40 },
        },
        {
          role: "implement",
          name: "Codex",
          status: "completed",
          files: 2,
          additions: 5,
          deletions: 1,
        },
        {
          role: "review",
          name: "Qwen",
          status: "skipped",
          skipped: "the implement role made no changes",
        },
        { role: "bogus" },
      ],
      applied: true,
      apply_note: "The changes were applied to the project.",
    });
    expect(summary.stages.map((s) => s.role)).toEqual([
      "plan",
      "implement",
      "review",
    ]);
    expect(summary.stages[1].files).toBe(2);
    expect(summary.applied).toBe(true);
    expect(stageStatus(summary.stages[0])).toBe("Done");
    expect(stageStatus(summary.stages[2])).toBe("Skipped");
    expect(stageStatus({ status: "limit_reached" })).toBe("Plan limit reached");
    expect(verdictLabel("ready")).toBe("Ready to apply");
    expect(verdictLabel("needs_changes")).toBe("Needs changes");
    expect(verdictLabel(undefined)).toBe("");
  });

  it("puts role cards, the roles note and the summary in the transcript", () => {
    const at = (
      id: number,
      type: string,
      payload: Record<string, unknown>,
    ): EventRow => ({ id, ts: id, type, payload, task_id: "t1" });
    const state = replay([
      at(1, "user.message", { text: "Fix it" }),
      at(2, "routing.selected", {
        provider: "shadowcode:roles",
        model_id: "roles:plan=cli:claude",
        model_name: "Roles: Claude Code → Codex → Qwen",
        inference: "cloud",
      }),
      at(3, "subagent.started", {
        run_id: "r1",
        agent: "plan",
        role: "plan",
        runner: "vendor",
        route: "cloud",
        cost: "subscription",
        model: "Claude Code",
        mode: "read-only",
      }),
      at(4, "subagent.finished", {
        run_id: "r1",
        status: "completed",
        summary: "1. Do it",
        usage: { total_tokens: 30, cost_usd: null },
      }),
      at(5, "subagent.started", {
        run_id: "r2",
        agent: "review",
        role: "review",
        runner: "shadowcode",
        route: "local",
        cost: "local",
        model: "Qwen",
        mode: "read-only",
      }),
      at(6, "subagent.finished", {
        run_id: "r2",
        status: "completed",
        summary: "Fine.\nVerdict: ready",
        verdict: "ready",
      }),
      at(7, "roles.finished", {
        stages: [{ role: "plan", name: "Claude Code", status: "completed" }],
        applied: null,
      }),
    ]);
    const note = state.items.find((item) => item.kind === "note");
    expect(note?.text).toBe(
      "Plan → Implement → Review · Claude Code → Codex → Qwen",
    );
    const runs = state.items.flatMap((item) =>
      item.kind === "subagent" ? [item.run] : [],
    );
    expect(runs.map((r) => [r.role, r.runner, r.cost])).toEqual([
      ["plan", "vendor", "subscription"],
      ["review", "shadowcode", "local"],
    ]);
    expect(runs[1].verdict).toBe("ready");
    expect(state.activity.t1.roles?.stages).toHaveLength(1);
  });
});
