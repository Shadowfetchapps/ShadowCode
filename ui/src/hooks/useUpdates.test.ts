import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { act, cleanup, renderHook } from "@testing-library/react";
import { api, type UpdateStatus } from "../api";
import {
  FIRST_CHECK_DELAY_MS,
  RECHECK_MS,
  announceUpdates,
  noticeVersion,
  useUpdateNotice,
} from "./useUpdates";

function updateStatus(overrides: Partial<UpdateStatus> = {}): UpdateStatus {
  return {
    current: "0.33.1",
    allowed: true,
    automatic: true,
    setting: null,
    default_on: true,
    offline: false,
    policy_message: null,
    policy_source: null,
    install: { kind: "appimage", label: "AppImage" },
    last_checked_at: null,
    last_attempt_at: null,
    error: null,
    latest: null,
    available: false,
    dismissed: false,
    next_step: null,
    releases_url: "https://github.com/Shadowfetchapps/ShadowCode/releases",
    ...overrides,
  };
}

const available = updateStatus({
  available: true,
  latest: {
    version: "0.34.0",
    tag: "v0.34.0",
    url: "https://github.com/Shadowfetchapps/ShadowCode/releases/tag/v0.34.0",
    published_at: "2026-10-02T09:15:00Z",
    signed: true,
  },
});

beforeEach(() => vi.useFakeTimers());
afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

it("shows the last answer at once, then asks for the daily check and hourly", async () => {
  const updates = vi.spyOn(api, "updates").mockResolvedValue(available);
  const { result } = renderHook(() => useUpdateNotice(true));
  // Start-up reads what the last check found, without a check.
  await act(async () => {
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(updates).toHaveBeenCalledTimes(1);
  expect(updates).toHaveBeenLastCalledWith(false);
  expect(noticeVersion(result.current)).toBe("0.34.0");
  await act(async () => {
    await vi.advanceTimersByTimeAsync(FIRST_CHECK_DELAY_MS);
  });
  expect(updates).toHaveBeenCalledTimes(2);
  expect(updates).toHaveBeenLastCalledWith(true);
  await act(async () => {
    await vi.advanceTimersByTimeAsync(RECHECK_MS);
  });
  expect(updates).toHaveBeenCalledTimes(3);
  expect(updates).toHaveBeenLastCalledWith(true);
  // Settings › About changed something: re-read the cache, no check.
  updates.mockResolvedValue({ ...available, dismissed: true });
  await act(async () => {
    announceUpdates();
    await vi.advanceTimersByTimeAsync(0);
  });
  expect(updates).toHaveBeenLastCalledWith(false);
  expect(noticeVersion(result.current)).toBe("");
});

it("never asks when disabled (remote devices, unit tests)", async () => {
  const updates = vi.spyOn(api, "updates").mockResolvedValue(available);
  const { result } = renderHook(() => useUpdateNotice(false));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(RECHECK_MS * 2);
  });
  expect(updates).not.toHaveBeenCalled();
  expect(result.current).toBeNull();
});

it("stays silent when the engine cannot answer", async () => {
  vi.spyOn(api, "updates").mockRejectedValue(new Error("offline"));
  const { result } = renderHook(() => useUpdateNotice(true));
  await act(async () => {
    await vi.advanceTimersByTimeAsync(FIRST_CHECK_DELAY_MS);
  });
  expect(result.current).toBeNull();
});

it("announces only newer, visible releases", () => {
  expect(noticeVersion(null)).toBe("");
  expect(noticeVersion(updateStatus())).toBe("");
  expect(noticeVersion(available)).toBe("0.34.0");
  expect(noticeVersion({ ...available, dismissed: true })).toBe("");
  expect(noticeVersion({ ...available, latest: null })).toBe("");
});
