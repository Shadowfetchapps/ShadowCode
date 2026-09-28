import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { RunInWorktreeButton } from "./RunInWorktreeButton";

afterEach(cleanup);

describe("run in worktree", () => {
  it("keeps the action discoverable and explains why it is unavailable", () => {
    const onRun = vi.fn();
    render(
      <RunInWorktreeButton
        reason="Open a trusted Git project first."
        enabled={false}
        queueing={false}
        onRun={onRun}
      />,
    );
    const action = screen.getByRole("button", {
      name: /Run in a new worktree unavailable: Open a trusted Git project first/,
    });
    expect(action.getAttribute("aria-disabled")).toBe("true");
    expect(action.getAttribute("title")).toBe(
      "Open a trusted Git project first.",
    );
    fireEvent.click(action);
    expect(onRun).not.toHaveBeenCalled();
  });
});
