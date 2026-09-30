import { expect, it } from "vitest";
import { resumeRequest } from "./useTaskActions";
import type { ChatItem } from "../components/cards";

const ready: Extract<ChatItem, { kind: "resume" }> = {
  kind: "resume",
  resumeId: "r1",
  state: "needs_consent",
  at: 1_790_010_000,
  label: "Codex",
  target: "cli:codex",
  text: "Codex's limit has reset.",
  task: "Continue where Codex stopped when its plan limit was reached.",
};
const where = { workspace: "/p", sessionId: "s1", queueing: false };

it("resumes a Plan or Ask task read-only and keeps its web access", () => {
  expect(resumeRequest({ ...ready, mode: "plan" }, where)).toMatchObject({
    task: ready.task,
    model: "cli:codex",
    session_id: "s1",
    purpose: "planner",
    web: false,
  });
  expect(
    resumeRequest({ ...ready, mode: "ask", web: true }, where),
  ).toMatchObject({ purpose: "reviewer", web: true });
  // A Code task, or a card from before the mode was recorded.
  expect(resumeRequest(ready, where)).toMatchObject({
    purpose: "coder",
    web: false,
  });
});

it("resumes with the limited task's files and Only change these, as Try on does", () => {
  const mentions = [{ path: "src/a.ts", kind: "file" as const }];
  expect(
    resumeRequest({ ...ready, mentions, onlyChange: true }, where),
  ).toMatchObject({ mentions, only_change: true });
  const loose = resumeRequest({ ...ready, mentions }, where);
  expect(loose.mentions).toEqual(mentions);
  expect(loose.only_change).toBeUndefined();
  // A card without files (or from before they were kept) sends none.
  const plain = resumeRequest(ready, where);
  expect(plain.mentions).toBeUndefined();
  expect(plain.only_change).toBeUndefined();
});
