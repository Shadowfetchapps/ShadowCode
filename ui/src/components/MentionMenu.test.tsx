import { act, cleanup, renderHook } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api } from "../api";
import { fitsMentionQuery } from "../lib/mentions";
import { useFileMentions } from "./MentionMenu";

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("matches mention queries the way the project search does: letters in order", () => {
  expect(fitsMentionQuery("src/app.ts", "app")).toBe(true);
  expect(fitsMentionQuery("src/app.ts", "sapp")).toBe(true);
  expect(fitsMentionQuery("README.md", "app")).toBe(false);
  expect(fitsMentionQuery("README.md", "")).toBe(true);
  expect(fitsMentionQuery("docs/Guide.md", "GUIDE")).toBe(true);
});

it("while a search is on its way, earlier results the new text rules out are hidden", async () => {
  vi.useFakeTimers();
  const answers: Record<string, { path: string; kind: "file" }[]> = {
    a: [
      { path: "README.md", kind: "file" },
      { path: "src/app.ts", kind: "file" },
    ],
    app: [{ path: "src/app.ts", kind: "file" }],
  };
  let release!: () => void;
  const held = new Promise<void>((resolve) => (release = resolve));
  vi.spyOn(api, "mentions").mockImplementation(async (query: string) => {
    if (query === "app") await held;
    return { items: answers[query] ?? [] } as Awaited<
      ReturnType<typeof api.mentions>
    >;
  });
  const { result, rerender } = renderHook(
    ({ query }) => useFileMentions(query),
    { initialProps: { query: "a" as string | null } },
  );
  await act(async () => {
    await vi.advanceTimersByTimeAsync(150);
  });
  expect(result.current.items.map((i) => i.path)).toEqual([
    "README.md",
    "src/app.ts",
  ]);
  rerender({ query: "app" });
  await act(async () => {
    await vi.advanceTimersByTimeAsync(150);
  });
  // "app" is still being searched: README.md cannot be the first choice.
  expect(result.current.loading).toBe(true);
  expect(result.current.items.map((i) => i.path)).toEqual(["src/app.ts"]);
  await act(async () => {
    release();
    await held;
  });
  expect(result.current.loading).toBe(false);
  expect(result.current.items.map((i) => i.path)).toEqual(["src/app.ts"]);
});
