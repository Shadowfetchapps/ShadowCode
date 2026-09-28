import { act, renderHook } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useSidebar } from "./useWindow";

let listeners: ((event: MediaQueryListEvent) => void)[] = [];
const resize = (narrow: boolean) =>
  act(() =>
    listeners.forEach((fn) => fn({ matches: narrow } as MediaQueryListEvent)),
  );

beforeEach(() => {
  localStorage.clear();
  listeners = [];
  vi.stubGlobal("innerWidth", 1440);
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({
      matches: false,
      addEventListener: (_: string, fn: (e: MediaQueryListEvent) => void) =>
        listeners.push(fn),
      removeEventListener: vi.fn(),
    })),
  );
});
afterEach(() => vi.unstubAllGlobals());

it("a sidebar the narrow window closed comes back when it is wide again", () => {
  const { result, unmount } = renderHook(() => useSidebar());
  expect(result.current[0]).toBe(true);
  resize(true);
  expect(result.current[0]).toBe(false);
  // Narrowing is not the user's choice, so it is not remembered.
  expect(localStorage.getItem("shadow:sidebar")).not.toBe("closed");
  resize(false);
  expect(result.current[0]).toBe(true);
  unmount();
});

it("a sidebar the user closed stays closed", () => {
  const { result } = renderHook(() => useSidebar());
  act(() => result.current[1]((open) => !open));
  expect(result.current[0]).toBe(false);
  expect(localStorage.getItem("shadow:sidebar")).toBe("closed");
  resize(true);
  resize(false);
  expect(result.current[0]).toBe(false);
  act(() => result.current[1](true));
  expect(localStorage.getItem("shadow:sidebar")).toBe("open");
});
