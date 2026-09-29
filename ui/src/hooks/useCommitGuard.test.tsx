import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../api";
import { useCommitGuard } from "./useCommitGuard";

vi.mock("../api", () => ({
  api: { gitCommit: vi.fn(), gitUnstage: vi.fn(), gitIgnore: vi.fn() },
}));

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

function Commit({ done }: { done: () => void }) {
  const guard = useCommitGuard(vi.fn());
  return (
    <>
      <button
        type="button"
        onClick={() => void guard.commit("Add config", done)}
      >
        Commit
      </button>
      {guard.dialog}
    </>
  );
}

it("shows possible secrets; removing them lets the commit go ahead", async () => {
  const done = vi.fn();
  vi.mocked(api.gitCommit)
    .mockResolvedValueOnce({
      ok: false,
      status: 409,
      secrets: [
        {
          path: "config.py",
          line: 3,
          kind: "a GitHub token",
          preview: "ghp_aB… (40 characters)",
        },
      ],
    })
    .mockResolvedValueOnce({ ok: true });
  vi.mocked(api.gitUnstage).mockResolvedValue({ ok: true });
  render(<Commit done={done} />);
  fireEvent.click(screen.getByRole("button", { name: "Commit" }));
  expect(
    await screen.findByText("This commit may contain a secret"),
  ).toBeTruthy();
  expect(screen.getByText(/Line 3: looks like a GitHub token/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Commit anyway" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Remove from commit" }));
  await waitFor(() =>
    expect(api.gitUnstage).toHaveBeenCalledWith(["config.py"]),
  );
  expect(await screen.findByText("Ready to commit")).toBeTruthy();
  fireEvent.click(screen.getAllByRole("button", { name: "Commit" }).at(-1)!);
  await waitFor(() => expect(done).toHaveBeenCalled());
  expect(api.gitCommit).toHaveBeenLastCalledWith("Add config", {});
});

it("asks once about the project's hooks and sends the answer", async () => {
  const done = vi.fn();
  vi.mocked(api.gitCommit)
    .mockResolvedValueOnce({
      ok: false,
      status: 409,
      needs_hooks_choice: true,
      hooks: [
        {
          name: "pre-commit",
          path: "/p/.git/hooks/pre-commit",
          preview: "npx lint-staged",
        },
      ],
    })
    .mockResolvedValueOnce({ ok: true, hooks_ran: true });
  render(<Commit done={done} />);
  fireEvent.click(screen.getByRole("button", { name: "Commit" }));
  expect(await screen.findByText("npx lint-staged")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Run hooks and commit" }));
  await waitFor(() => expect(done).toHaveBeenCalled());
  expect(api.gitCommit).toHaveBeenLastCalledWith("Add config", {
    hooks: "run",
  });
});
