import { act, cleanup, renderHook } from "@testing-library/react";
import { StrictMode, useEffect } from "react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { PickerResponse } from "../api";
import { usePickerTargets } from "./useCatalog";

const mocks = vi.hoisted(() => ({ picker: vi.fn(), pickerCached: vi.fn() }));
vi.mock("../api", () => ({ api: mocks }));

function deferred<T = PickerResponse>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}
function rows(
  id: string,
  billing?: "subscription" | "api_key" | "unknown",
): PickerResponse {
  return {
    targets: [
      {
        id,
        provider: "fixture",
        name: id,
        group: "subscriptions",
        inference: "cloud",
        availability: "ready",
        billing,
      },
    ],
    generated_at: 1,
  };
}
const flush = async () => {
  await act(async () => {
    await Promise.resolve();
  });
};
const tick = async (ms: number) => {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
};

beforeEach(() => {
  vi.useFakeTimers();
  vi.setSystemTime(new Date("2026-01-01T00:00:00Z"));
  mocks.picker.mockReset();
  mocks.pickerCached.mockReset();
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

it("publishes immediate and progressive cached rows while the full request remains pending", async () => {
  const full = deferred();
  mocks.picker.mockReturnValue(full.promise);
  mocks.pickerCached
    .mockResolvedValueOnce(rows("first", "unknown"))
    .mockResolvedValue(rows("fast-provider", "api_key"));
  const toast = vi.fn();
  const { result } = renderHook(() => usePickerTargets(toast));
  let completion!: Promise<void>;
  act(() => {
    completion = result.current.reload(true);
  });
  await flush();
  expect(mocks.picker).toHaveBeenCalledExactlyOnceWith(true);
  expect(mocks.pickerCached).toHaveBeenCalledTimes(1);
  expect(result.current.loaded).toBe(true);
  expect(result.current.targets).toEqual(rows("first", "unknown").targets);
  await tick(1000);
  expect(result.current.targets).toEqual(
    rows("fast-provider", "api_key").targets,
  );
  full.resolve(rows("final", "subscription"));
  await act(async () => {
    await completion;
  });
  expect(result.current.targets).toEqual(rows("final", "subscription").targets);
  const count = mocks.pickerCached.mock.calls.length;
  await tick(10000);
  expect(mocks.pickerCached).toHaveBeenCalledTimes(count);
  expect(toast).not.toHaveBeenCalled();
});

it("serializes cached requests and ignores an older snapshot after the full result", async () => {
  const full = deferred();
  const first = deferred();
  const second = deferred();
  mocks.picker.mockReturnValue(full.promise);
  mocks.pickerCached
    .mockReturnValueOnce(first.promise)
    .mockReturnValue(second.promise);
  const { result } = renderHook(() => usePickerTargets(vi.fn()));
  let completion!: Promise<void>;
  act(() => {
    completion = result.current.reload();
  });
  await tick(10000);
  expect(mocks.pickerCached).toHaveBeenCalledTimes(1);
  first.resolve(rows("partial"));
  await flush();
  await tick(1000);
  expect(mocks.pickerCached).toHaveBeenCalledTimes(2);
  full.resolve(rows("authoritative"));
  await act(async () => {
    await completion;
  });
  second.resolve(rows("late-cache"));
  await flush();
  expect(result.current.targets).toEqual(rows("authoritative").targets);
  expect(vi.getTimerCount()).toBe(0);
});

it("retires older reload responses and errors without losing current rows", async () => {
  const first = deferred();
  const second = deferred();
  const oldCache = deferred();
  const newCache = deferred();
  mocks.picker
    .mockReturnValueOnce(first.promise)
    .mockReturnValue(second.promise);
  mocks.pickerCached
    .mockReturnValueOnce(oldCache.promise)
    .mockReturnValue(newCache.promise);
  const toast = vi.fn();
  const { result } = renderHook(() => usePickerTargets(toast));
  let oldCompletion!: Promise<void>;
  let newCompletion!: Promise<void>;
  act(() => {
    oldCompletion = result.current.reload();
  });
  act(() => {
    newCompletion = result.current.reload(true);
  });
  newCache.resolve(rows("current"));
  await flush();
  oldCache.resolve(rows("old"));
  first.reject(new Error("obsolete failure"));
  await act(async () => {
    await oldCompletion;
  });
  expect(result.current.targets).toEqual(rows("current").targets);
  expect(toast).not.toHaveBeenCalled();
  second.resolve(rows("current-final"));
  await act(async () => {
    await newCompletion;
  });
  expect(result.current.targets).toEqual(rows("current-final").targets);
  expect(mocks.picker.mock.calls).toEqual([[false], [true]]);
});

it("retains the last snapshot but stops polling when the full refresh fails", async () => {
  const full = deferred();
  const late = deferred();
  mocks.picker.mockReturnValue(full.promise);
  mocks.pickerCached
    .mockResolvedValueOnce(rows("available"))
    .mockReturnValue(late.promise);
  const toast = vi.fn();
  const { result } = renderHook(() => usePickerTargets(toast));
  let completion!: Promise<void>;
  act(() => {
    completion = result.current.reload();
  });
  await flush();
  await tick(1000);
  full.reject(new Error("provider unavailable"));
  await act(async () => {
    await completion;
  });
  late.resolve(rows("after-error"));
  await flush();
  expect(result.current.targets).toEqual(rows("available").targets);
  expect(result.current.loaded).toBe(true);
  expect(toast).toHaveBeenCalledExactlyOnceWith(
    "Could not load models: Error: provider unavailable",
    "err",
  );
  expect(vi.getTimerCount()).toBe(0);
});

it("retries a failed snapshot while the full refresh still owns error reporting", async () => {
  const full = deferred();
  mocks.picker.mockReturnValue(full.promise);
  mocks.pickerCached
    .mockRejectedValueOnce(new Error("snapshot unavailable"))
    .mockResolvedValue(rows("recovered"));
  const toast = vi.fn();
  const { result } = renderHook(() => usePickerTargets(toast));
  let completion!: Promise<void>;
  act(() => {
    completion = result.current.reload();
  });
  await flush();
  expect(toast).not.toHaveBeenCalled();
  await tick(1000);
  expect(result.current.targets).toEqual(rows("recovered").targets);
  full.resolve(rows("final"));
  await act(async () => {
    await completion;
  });
});

it("stops polling and ignores pending responses and errors after unmount", async () => {
  const full = deferred();
  const cached = deferred();
  mocks.picker.mockReturnValue(full.promise);
  mocks.pickerCached.mockReturnValue(cached.promise);
  const toast = vi.fn();
  const { result, unmount } = renderHook(() => usePickerTargets(toast));
  let completion!: Promise<void>;
  act(() => {
    completion = result.current.reload();
  });
  unmount();
  cached.resolve(rows("late"));
  full.reject(new Error("late failure"));
  await act(async () => {
    await completion;
  });
  await tick(10000);
  expect(mocks.pickerCached).toHaveBeenCalledTimes(1);
  expect(toast).not.toHaveBeenCalled();
  expect(vi.getTimerCount()).toBe(0);
});

it("bounds snapshot polling lifetime while allowing the original full request to finish", async () => {
  const full = deferred();
  const late = deferred();
  mocks.picker.mockReturnValue(full.promise);
  mocks.pickerCached
    .mockResolvedValueOnce(rows("initial"))
    .mockReturnValue(late.promise);
  const { result } = renderHook(() => usePickerTargets(vi.fn()));
  let completion!: Promise<void>;
  act(() => {
    completion = result.current.reload();
  });
  await flush();
  await tick(1000);
  expect(mocks.pickerCached).toHaveBeenCalledTimes(2);
  await tick(60000);
  expect(vi.getTimerCount()).toBe(0);
  late.resolve(rows("expired-snapshot"));
  await flush();
  await tick(10000);
  expect(mocks.pickerCached).toHaveBeenCalledTimes(2);
  expect(result.current.targets).toEqual(rows("initial").targets);
  full.resolve(rows("eventual-full"));
  await act(async () => {
    await completion;
  });
  expect(result.current.targets).toEqual(rows("eventual-full").targets);
});

it("restarts safely through StrictMode setup and cleanup without stale publication", async () => {
  const first = deferred();
  const second = deferred();
  const firstCache = deferred();
  const secondCache = deferred();
  mocks.picker
    .mockReturnValueOnce(first.promise)
    .mockReturnValue(second.promise);
  mocks.pickerCached
    .mockReturnValueOnce(firstCache.promise)
    .mockReturnValue(secondCache.promise);
  const toast = vi.fn();
  const { result } = renderHook(
    () => {
      const picker = usePickerTargets(toast);
      useEffect(() => {
        void picker.reload();
      }, [picker.reload]);
      return picker;
    },
    { wrapper: StrictMode },
  );
  expect(mocks.picker).toHaveBeenCalledTimes(2);
  firstCache.resolve(rows("retired"));
  first.reject(new Error("retired failure"));
  secondCache.resolve(rows("current"));
  await flush();
  expect(result.current.targets).toEqual(rows("current").targets);
  expect(toast).not.toHaveBeenCalled();
  second.resolve(rows("final"));
  await flush();
  expect(result.current.targets).toEqual(rows("final").targets);
  expect(vi.getTimerCount()).toBe(0);
});
