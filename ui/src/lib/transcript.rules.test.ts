import { expect, it } from "vitest";
import { replay, rulesDeliveredNote } from "./transcript";
import type { EventRow } from "../api";

const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
): EventRow => ({ id, ts: id, type, payload, task_id: "one" });

it("says what of the rulebook reached a vendor, in counts only", () => {
  const rows = [
    event(1, "agent.started", { task: "Fix it", vendor_agent: "claude" }),
    event(2, "rules.delivered", {
      vendor: "claude",
      profile_files: 1,
      project_files: 2,
      skills: 3,
      plugin_skills: 1,
      bytes: 3600,
      estimated_tokens: 1200,
      truncated: false,
    }),
  ];
  const note = replay(rows).items.find((i) => i.kind === "note");
  expect(note).toMatchObject({
    kind: "note",
    text: "Rules sent to Claude Code · 1 profile rules file · 2 project files · 3 skills · about 1,200 tokens",
  });
  expect(note && "warning" in note && note.warning).toBeFalsy();
});

it("mentions what was left out and skips empty counts", () => {
  expect(
    rulesDeliveredNote({
      vendor: "codex",
      profile_files: 0,
      project_files: 1,
      skills: 0,
      estimated_tokens: 40,
      truncated: true,
    }),
  ).toBe(
    "Rules sent to Codex · 1 project file · about 40 tokens · some left out to stay within limits",
  );
});
