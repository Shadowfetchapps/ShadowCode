import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Approval } from "../api";
import { ApprovalCard, PREVIEW_LINES } from "./ApprovalCard";

afterEach(cleanup);

const longDiff = [
  "@@ -1,0 +1,30 @@",
  ...Array.from({ length: 30 }, (_, i) => `+line ${i + 1}`),
].join("\n");

const edit = (over: Partial<Approval> = {}): Approval => ({
  id: "a1",
  session_id: "s1",
  tool: "write_file",
  command: "write_file",
  reason: "Write notes.txt",
  preview: {
    kind: "files",
    files: [
      {
        path: "notes.txt",
        status: "added",
        diff: longDiff,
        added: 30,
        removed: 0,
        truncated: false,
        binary: false,
      },
    ],
  },
  grant: "file edits",
  note: true,
  ...over,
});

describe("ApprovalCard", () => {
  it("previews a new file, truncated with Show all", () => {
    render(<ApprovalCard approval={edit()} onDecide={vi.fn()} />);
    expect(screen.getByText("New file")).toBeTruthy();
    expect(screen.getByText("notes.txt")).toBeTruthy();
    expect(screen.getByText("+30")).toBeTruthy();
    expect(screen.getByText("line 1")).toBeTruthy();
    expect(screen.queryByText(`line ${PREVIEW_LINES + 1}`)).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Show all 30 lines" }));
    expect(screen.getByText("line 30")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Show less" }));
    expect(screen.queryByText("line 30")).toBeNull();
  });

  it("shows an edit as a diff against the current file", () => {
    render(
      <ApprovalCard
        approval={edit({
          preview: {
            kind: "files",
            files: [
              {
                path: "src/app.ts",
                status: "modified",
                diff: "@@ -1,2 +1,2 @@\n-const a = 1;\n+const a = 2;\n keep\n",
                added: 1,
                removed: 1,
                truncated: false,
                binary: false,
              },
            ],
          },
        })}
        onDecide={vi.fn()}
      />,
    );
    expect(screen.getByText("Edited")).toBeTruthy();
    expect(screen.getByLabelText("Removed")).toBeTruthy();
    expect(screen.getByLabelText("Added")).toBeTruthy();
    expect(document.querySelector(".diff-del")?.textContent).toContain(
      "const a = 1;",
    );
  });

  it("shows the full command and its folder", () => {
    render(
      <ApprovalCard
        approval={{
          id: "a2",
          tool: "exec",
          command: "cargo test --workspace -- --nocapture",
          reason: "Run a shell command",
          preview: {
            kind: "command",
            command: "cargo test --workspace -- --nocapture",
            cwd: "/work/demo/crates/core",
          },
          grant: "`cargo test` commands",
          note: true,
        }}
        onDecide={vi.fn()}
      />,
    );
    expect(
      screen.getByText("cargo test --workspace -- --nocapture"),
    ).toBeTruthy();
    expect(screen.getByText("/work/demo/crates/core")).toBeTruthy();
    expect(screen.getByText(/covers cargo test commands/)).toBeTruthy();
  });

  it("answers Allow, Allow for this task, Deny and Deny with note", () => {
    const onDecide = vi.fn();
    render(<ApprovalCard approval={edit()} onDecide={onDecide} />);
    fireEvent.click(screen.getByRole("button", { name: "Allow" }));
    expect(onDecide).toHaveBeenLastCalledWith("a1", { decision: "approve" });
    fireEvent.click(
      screen.getByRole("button", { name: "Allow for this task" }),
    );
    expect(onDecide).toHaveBeenLastCalledWith("a1", {
      decision: "approve",
      scope: "task",
    });
    fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(onDecide).toHaveBeenLastCalledWith("a1", { decision: "deny" });
    fireEvent.click(screen.getByRole("button", { name: "Deny with note…" }));
    fireEvent.change(
      screen.getByLabelText("Tell the agent why, or what to do instead"),
      { target: { value: "Put notes in docs/ instead" } },
    );
    fireEvent.click(screen.getByRole("button", { name: "Deny and send note" }));
    expect(onDecide).toHaveBeenLastCalledWith("a1", {
      decision: "deny",
      note: "Put notes in docs/ instead",
    });
  });

  it("offers only what the prompt allows", () => {
    render(
      <ApprovalCard
        approval={edit({ grant: "", note: false })}
        onDecide={vi.fn()}
      />,
    );
    expect(
      screen.queryByRole("button", { name: "Allow for this task" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Deny with note…" }),
    ).toBeNull();
  });
  it("explains the action with its risk and undo, and offers Always allow only when given", () => {
    const onDecide = vi.fn();
    const approval = edit({
      tool: "exec",
      command: "cargo test",
      reason: "Run a shell command as your user",
      preview: { kind: "command", command: "cargo test", cwd: "/p" },
      assessment: {
        risk: "changes_files",
        risk_label: "Changes files",
        explanation: "Runs the project's Rust tests.",
        undo: "yes",
        undo_label: "Rewind can undo this",
        notes: ["Some steps repeat"],
        read: true,
        checks: [],
      },
      always: "Always allow `cargo test` in this project",
      grant: "",
    });
    const { rerender } = render(
      <ApprovalCard approval={approval} onDecide={onDecide} />,
    );
    expect(screen.getByText("Runs the project's Rust tests.")).toBeTruthy();
    expect(screen.getByText("Changes files")).toBeTruthy();
    expect(screen.getByText("Rewind can undo this")).toBeTruthy();
    expect(screen.getByText("Some steps repeat")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Always allow here" }));
    expect(onDecide).toHaveBeenCalledWith("a1", {
      decision: "approve",
      scope: "project",
    });
    rerender(
      <ApprovalCard
        approval={{ ...approval, id: "a2", always: "" }}
        onDecide={onDecide}
      />,
    );
    expect(
      screen.queryByRole("button", { name: "Always allow here" }),
    ).toBeNull();
  });
});
