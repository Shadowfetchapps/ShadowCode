import { afterEach, expect, it } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { remembered, useDrawerMemory } from "./useDrawerMemory";

afterEach(cleanup);

it("keeps drawer work per project and starts fresh in another", () => {
  const { result: hook, rerender } = renderHook(
    ({ workspace }) => useDrawerMemory(workspace),
    { initialProps: { workspace: "/a" } },
  );
  act(() => {
    hook.current.update("commitMessage", "Fix the add function");
    hook.current.update("terminalActive", "t1");
    hook.current.update("filesActive", "README.md");
    hook.current.update("filesBuffers", {
      "README.md": {
        path: "README.md",
        base: "# A",
        draft: "# Draft",
        hash: "abc",
      },
    });
    hook.current.update("filesDir", "src");
  });
  // Functional updates see the latest value (a draft edited twice quickly).
  act(() =>
    hook.current.update("prDraft", (prev) => ({ ...prev, title: "Add login" })),
  );
  act(() =>
    hook.current.update("prDraft", (prev) => ({ ...prev, draft: true })),
  );
  rerender({ workspace: "/a" });
  expect(hook.current.memory.commitMessage).toBe("Fix the add function");
  expect(hook.current.memory.terminalActive).toBe("t1");
  expect(hook.current.memory.prDraft).toMatchObject({
    title: "Add login",
    draft: true,
  });
  expect(hook.current.memory.filesActive).toBe("README.md");
  expect(hook.current.memory.filesBuffers["README.md"].draft).toBe("# Draft");
  expect(hook.current.memory.filesDir).toBe("src");
  rerender({ workspace: "/b" });
  expect(hook.current.memory.commitMessage).toBe("");
  expect(hook.current.memory.terminalActive).toBe("");
  expect(hook.current.memory.prDraft.title).toBe("");
  expect(hook.current.memory.filesActive).toBeNull();
  expect(hook.current.memory.filesBuffers).toEqual({});
  expect(hook.current.memory.filesDir).toBe(".");
  const close = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(close);
  expect(close.defaultPrevented).toBe(true);
  rerender({ workspace: "/a" });
  expect(hook.current.memory.filesBuffers["README.md"].draft).toBe("# Draft");
});

it("gives each tab a useState-shaped value and setter", () => {
  const { result: hook } = renderHook(() => useDrawerMemory("/a"));
  const [message, setMessage] = remembered(
    hook.current.memory,
    hook.current.update,
    "commitMessage",
  );
  expect(message).toBe("");
  act(() => setMessage("Draft"));
  expect(hook.current.memory.commitMessage).toBe("Draft");
  const before = hook.current.memory;
  act(() => hook.current.update("commitMessage", "Draft"));
  expect(hook.current.memory).toBe(before);
});
