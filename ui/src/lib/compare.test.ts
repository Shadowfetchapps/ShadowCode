import { describe, expect, it } from "vitest";
import type { CompareLane, CompareRecord } from "../api";
import type { PickerTarget } from "./picker";
import {
  badgeFor,
  checkLineup,
  compareBlocked,
  conflictFiles,
  keepBlocked,
  laneState,
  slotTargets,
} from "./compare";

const row = (over: Partial<PickerTarget>): PickerTarget => ({
  id: "x",
  provider: "cli:codex",
  group: "subscriptions",
  name: "X",
  inference: "cloud",
  availability: "ready",
  ...over,
});
const codex = row({ id: "cli:codex:a", name: "Codex · A" });
const router = row({
  id: "api:openrouter:q",
  provider: "openrouter",
  group: "api",
  name: "Qwen: Coder",
});
const qwen = row({
  id: "local:gguf:qwen",
  provider: "llamacpp",
  group: "local",
  inference: "local",
  name: "qwen · This computer",
});
const gemma = row({ ...qwen, id: "local:gguf:gemma", name: "gemma" });
const claude = row({
  id: "cli:claude",
  name: "Claude Code · Default",
  availability: "sign_in",
  availability_label: "Sign in",
  reason: "Not signed in",
});
const targets = [codex, router, qwen, gemma, claude];

describe("checkLineup", () => {
  it("needs 2 or 3 chosen, distinct, ready models", () => {
    expect(checkLineup(["", ""], targets).error).toBe("Choose a model.");
    expect(checkLineup([codex.id, ""], targets).slots).toEqual([
      null,
      "Choose a model.",
    ]);
    expect(checkLineup([codex.id], targets).error).toBe(
      "Compare needs 2 or 3 models.",
    );
    expect(
      checkLineup([codex.id, router.id, qwen.id, gemma.id], targets).error,
    ).toBe("Compare needs 2 or 3 models.");
    expect(checkLineup([codex.id, codex.id], targets).slots[1]).toMatch(
      /different model/,
    );
    expect(checkLineup([codex.id, claude.id], targets).slots[1]).toBe(
      "Claude Code · Default: Sign in · Not signed in",
    );
    expect(checkLineup([codex.id, qwen.id], targets)).toEqual({
      error: null,
      slots: [null, null],
    });
    expect(checkLineup([codex.id, router.id, qwen.id], targets).error).toBe(
      null,
    );
  });
  it("allows distinct local models to run sequentially", () => {
    const check = checkLineup([qwen.id, gemma.id], targets);
    expect(check.slots).toEqual([null, null]);
    expect(check.error).toBeNull();
  });
  it("flags a model that left the picker", () => {
    expect(checkLineup([codex.id, "cli:gone"], targets).slots[1]).toMatch(
      /no longer available/,
    );
  });
});

describe("slotTargets", () => {
  it("keeps rows visible but explains why they cannot join", () => {
    const rows = slotTargets(targets, [qwen.id, ""], 1);
    const local = rows.find((t) => t.id === gemma.id)!;
    expect(local).toBe(gemma);
    const taken = rows.find((t) => t.id === qwen.id)!;
    expect(taken.availability_label).toBe("Already chosen");
    expect(rows.find((t) => t.id === codex.id)).toBe(codex);
    // The slot holding the local model may still switch to another one.
    expect(slotTargets(targets, [qwen.id, ""], 0)).toEqual(targets);
  });
});

describe("compareBlocked", () => {
  const base = { task: "Fix it", repo: true, inLane: false, images: 0 };
  it("explains every reason the button is unavailable", () => {
    expect(compareBlocked(base)).toBeNull();
    expect(compareBlocked({ ...base, task: "  " })).toMatch(/Type a task/);
    expect(compareBlocked({ ...base, repo: false })).toMatch(/Git repository/);
    expect(compareBlocked({ ...base, task: "/diff" })).toMatch(/Commands/);
    expect(compareBlocked({ ...base, images: 1 })).toMatch(/text only/);
    expect(compareBlocked({ ...base, inLane: true })).toMatch(
      /one model's copy/,
    );
  });
});

describe("conflictFiles", () => {
  it("reads the files a Keep conflicts on", () => {
    expect(
      conflictFiles(
        "Codex's changes no longer apply: the project changed since the comparison started in src/a.ts, new.txt. Nothing was changed and every lane is kept; update or revert those files, then keep again.",
      ),
    ).toEqual(["src/a.ts", "new.txt"]);
    expect(
      conflictFiles(
        "Codex's changes no longer apply: the project changed since the comparison started (error: corrupt patch). Nothing was changed",
      ),
    ).toEqual([]);
    expect(conflictFiles("Comparison not found")).toBeNull();
  });
});

const lane = (over: Partial<CompareLane> = {}): CompareLane => ({
  model: "cli:codex:a",
  name: "Codex · A",
  session_id: "s2",
  job_id: "j1",
  worktree: "/w/1",
  worktree_id: "w1",
  branch: "shadowcode/w1",
  base_commit: "b",
  status: "running",
  summary: "",
  changed_files: [],
  changed_files_truncated: false,
  checks: { passed: 0, failed: 0, commands: [] },
  duration_s: 3,
  usage: {},
  error: null,
  removed: false,
  ...over,
});
const record = (over: Partial<CompareRecord> = {}): CompareRecord => ({
  id: "c1",
  workspace: "/w",
  task: "t",
  mode: "code",
  web: false,
  created_at: 0,
  finished_at: null,
  state: "running",
  base: { commit: "b", head: "h", included_uncommitted: false },
  lanes: [],
  winner: null,
  applied_files: [],
  notes: [],
  ...over,
});

describe("lane state", () => {
  it("reads status and pending approvals", () => {
    expect(laneState(lane()).label).toBe("Working…");
    expect(laneState(lane(), [{ id: "a", session_id: "s2" }]).kind).toBe(
      "waiting",
    );
    expect(laneState(lane({ status: "queued" })).kind).toBe("queued");
    expect(laneState(lane({ status: "completed" })).label).toBe("Finished");
    expect(laneState(lane({ status: "failed" })).tone).toBe("bad");
    expect(laneState(lane({ status: "limit_reached" })).label).toBe(
      "Plan limit reached",
    );
    expect(laneState(lane({ status: "cancelled" })).label).toBe("Stopped");
  });
  it("offers Keep only for a finished lane of an open comparison", () => {
    expect(keepBlocked(record(), lane())).toMatch(/finish/);
    expect(keepBlocked(record(), lane({ status: "completed" }))).toBeNull();
    expect(keepBlocked(record(), lane({ status: "failed" }))).toMatch(
      /nothing to keep/,
    );
    expect(
      keepBlocked(
        record(),
        lane({
          status: "failed",
          changed_files: [
            {
              path: "a",
              status: "added",
              additions: 1,
              deletions: 0,
              binary: false,
            },
          ],
        }),
      ),
    ).toBeNull();
    expect(
      keepBlocked(record({ state: "applied" }), lane({ status: "completed" })),
    ).toMatch(/already kept/);
    expect(
      keepBlocked(record(), lane({ status: "completed", removed: true })),
    ).toMatch(/removed/);
  });
  it("badges lanes like the picker", () => {
    expect(badgeFor("local:gguf:q").label).toBe("Local");
    expect(badgeFor("api:openrouter:q").label).toBe("API key");
    expect(badgeFor("cli:codex:a").label).toBe("Cloud");
    expect(badgeFor(router.id, router).label).toBe("API key");
  });
});
