import { expect, it } from "vitest";
import { replay } from "./transcript";
import type { EventRow } from "../api";

const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
): EventRow => ({ id, ts: id, type, payload, task_id: "one" });

const notes = (rows: EventRow[]) =>
  replay(rows)
    .items.filter((i) => i.kind === "note")
    .map((i) => ("text" in i ? i.text : ""));

it("says when a command ran because it is always allowed in the project", () => {
  expect(
    notes([
      event(1, "agent.started", { task: "Run the tests" }),
      event(2, "approval.granted", {
        tool: "exec",
        scope: "project",
        command: "cargo test",
        grant: "always allowed in this project",
      }),
      // "Allow for this task" grants stay quiet, as before.
      event(3, "approval.granted", { tool: "exec", grant: "`cargo` commands" }),
    ]),
  ).toEqual([
    "Ran without asking: `cargo test` is always allowed in this project",
  ]);
});

it("names ignored files saved before a step so Rewind can bring them back", () => {
  expect(
    notes([
      event(1, "agent.started", { task: "Reset the database" }),
      event(2, "checkpoint.updated", {
        source: "shell",
        changed: [".env", "data/dev.sqlite3"],
        ignored_saved: [".env", "data/dev.sqlite3"],
      }),
      event(3, "checkpoint.updated", { source: "shell", changed: ["a.rs"] }),
    ]),
  ).toEqual([
    "Saved .env, data/dev.sqlite3 before this step, so Rewind can bring them back",
  ]);
});
