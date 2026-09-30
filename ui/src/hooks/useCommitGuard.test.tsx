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

it("asks again when the hooks changed, for exactly the hooks shown", async () => {
  const done = vi.fn();
  vi.mocked(api.gitCommit)
    .mockResolvedValueOnce({
      ok: false,
      status: 409,
      needs_hooks_choice: true,
      hooks_fingerprint: "abc123",
      hooks_changed: true,
      hooks: [
        {
          name: "pre-commit",
          path: "/p/.husky/_/pre-commit",
          preview: "npm test",
        },
      ],
    })
    .mockResolvedValueOnce({ ok: true, hooks_ran: true });
  render(<Commit done={done} />);
  fireEvent.click(screen.getByRole("button", { name: "Commit" }));
  expect(
    await screen.findByText(/The hooks changed since you chose to run them/),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Run hooks and commit" }));
  await waitFor(() => expect(done).toHaveBeenCalled());
  expect(api.gitCommit).toHaveBeenLastCalledWith("Add config", {
    hooks: "run",
    hooks_fingerprint: "abc123",
  });
});

it("a check that could not read everything is not a clean commit", async () => {
  const done = vi.fn();
  const unchecked =
    "Part of the changes is too large to check for secrets (over 32 MB).";
  vi.mocked(api.gitCommit)
    .mockResolvedValueOnce({
      ok: false,
      status: 409,
      secrets: [],
      secrets_unchecked: unchecked,
      error: `${unchecked} Nothing was committed.`,
    })
    .mockResolvedValueOnce({ ok: true });
  render(<Commit done={done} />);
  fireEvent.click(screen.getByRole("button", { name: "Commit" }));
  expect(
    await screen.findByText("These changes weren’t all checked"),
  ).toBeTruthy();
  expect(screen.getByText(/too large to check for secrets/)).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Commit anyway" }));
  await waitFor(() => expect(done).toHaveBeenCalled());
  expect(api.gitCommit).toHaveBeenLastCalledWith("Add config", {
    allow_secrets: true,
  });
});

it("says when a file stays in the repository after Add to .gitignore", async () => {
  const toast = vi.fn();
  function Tracked() {
    const guard = useCommitGuard(toast);
    return (
      <>
        <button
          type="button"
          onClick={() => void guard.commit("Add config", vi.fn())}
        >
          Commit
        </button>
        {guard.dialog}
      </>
    );
  }
  vi.mocked(api.gitCommit).mockResolvedValueOnce({
    ok: false,
    status: 409,
    secrets: [
      {
        path: ".env.development",
        line: 4,
        kind: "a secret value",
        preview: "Zq8vN2… (32 characters)",
      },
    ],
  });
  vi.mocked(api.gitIgnore).mockResolvedValue({
    ok: true,
    path: ".env.development",
    tracked: true,
  });
  render(<Tracked />);
  fireEvent.click(screen.getByRole("button", { name: "Commit" }));
  fireEvent.click(
    await screen.findByRole("button", { name: "Add to .gitignore" }),
  );
  await waitFor(() =>
    expect(toast).toHaveBeenCalledWith(
      expect.stringContaining("already in the repository"),
      "info",
    ),
  );
  expect(await screen.findByText("Ready to commit")).toBeTruthy();
});
