import { describe, expect, it, vi } from "vitest";
import { createSharedNativeEvents } from "./nativeEvents";

function setup() {
  const dispatch = new Map<string, (payload: unknown) => unknown>();
  const nativeStop = vi.fn();
  const register = vi.fn(
    async (event: string, handler: (payload: unknown) => unknown) => {
      dispatch.set(event, handler);
      return nativeStop;
    },
  );
  const errors = vi.fn();
  return {
    dispatch,
    nativeStop,
    register,
    errors,
    listen: createSharedNativeEvents(register, errors),
  };
}

describe("shared native event registrations", () => {
  it("keeps one registration and releases local handlers through 100 task cycles", async () => {
    const { listen, register, dispatch, nativeStop } = setup();
    for (let index = 0; index < 100; index++) {
      const handler = vi.fn();
      const stop = await listen("shadowcode:events", handler);
      dispatch.get("shadowcode:events")!(index);
      expect(handler).toHaveBeenCalledExactlyOnceWith(index);
      stop();
      stop();
      dispatch.get("shadowcode:events")!("after-stop");
      expect(handler).toHaveBeenCalledTimes(1);
    }
    expect(register).toHaveBeenCalledTimes(1);
    expect(nativeStop).not.toHaveBeenCalled();
  });

  it("shares an in-flight registration and allows immediate late cleanup", async () => {
    let accept!: (stop: () => void) => void;
    let dispatch!: (payload: unknown) => unknown;
    const register = vi.fn(
      (_event: string, handler: (payload: unknown) => unknown) => {
        dispatch = handler;
        return new Promise<() => void>((resolve) => {
          accept = resolve;
        });
      },
    );
    const listen = createSharedNativeEvents(register);
    const first = vi.fn();
    const second = vi.fn();
    const one = listen("shadowcode:events", first);
    const two = listen("shadowcode:events", second);
    await Promise.resolve();
    expect(register).toHaveBeenCalledTimes(1);
    accept(vi.fn());
    (await one)(); // Component closed while registration was in flight.
    const stopTwo = await two;
    dispatch("wake");
    expect(first).not.toHaveBeenCalled();
    expect(second).toHaveBeenCalledExactlyOnceWith("wake");
    stopTwo();
  });

  it("rejects all subscribers on registration failure and permits a clean retry", async () => {
    const { listen, register, dispatch } = setup();
    register.mockRejectedValueOnce(new Error("event transport unavailable"));
    const first = vi.fn();
    const second = vi.fn();
    const outcomes = await Promise.allSettled([
      listen("shadowcode:events", first),
      listen("shadowcode:events", second),
    ]);
    expect(outcomes.map((item) => item.status)).toEqual([
      "rejected",
      "rejected",
    ]);
    expect(register).toHaveBeenCalledTimes(1);
    const retry = vi.fn();
    const stop = await listen("shadowcode:events", retry);
    dispatch.get("shadowcode:events")!("recovered");
    expect(retry).toHaveBeenCalledExactlyOnceWith("recovered");
    expect(first).not.toHaveBeenCalled();
    expect(second).not.toHaveBeenCalled();
    expect(register).toHaveBeenCalledTimes(2);
    stop();
  });

  it("preserves independent subscriptions of the same handler", async () => {
    const { listen, dispatch } = setup();
    const handler = vi.fn();
    const one = await listen("shadowcode:events", handler);
    const two = await listen("shadowcode:events", handler);
    dispatch.get("shadowcode:events")!(1);
    expect(handler).toHaveBeenCalledTimes(2);
    one();
    dispatch.get("shadowcode:events")!(2);
    expect(handler).toHaveBeenCalledTimes(3);
    two();
  });

  it("does not dispatch to a subscriber removed by an earlier callback", async () => {
    const { listen, dispatch } = setup();
    let stopLater = () => {};
    const stopFirst = await listen("shadowcode:events", () => stopLater());
    const later = vi.fn();
    stopLater = await listen("shadowcode:events", later);
    dispatch.get("shadowcode:events")!("wake");
    expect(later).not.toHaveBeenCalled();
    stopFirst();
  });

  it("isolates synchronous and asynchronous handler failures while reporting them", async () => {
    const { listen, dispatch, errors } = setup();
    const syncError = new Error("sync handler");
    const asyncError = new Error("async handler");
    await listen("shadowcode:events", () => {
      throw syncError;
    });
    await listen("shadowcode:events", async () => {
      throw asyncError;
    });
    const healthy = vi.fn();
    await listen("shadowcode:events", healthy);
    dispatch.get("shadowcode:events")!("wake");
    await Promise.resolve();
    expect(healthy).toHaveBeenCalledExactlyOnceWith("wake");
    expect(errors.mock.calls).toEqual([[syncError], [asyncError]]);
  });

  it("keeps the four supported channels independent", async () => {
    const { listen, register, dispatch } = setup();
    const handlers = [vi.fn(), vi.fn(), vi.fn(), vi.fn()];
    const channels = [
      "shadowcode:events",
      "shadowcode:terminal",
      "shadowcode:open-session",
      "shadowcode:shutdown",
    ];
    for (const [index, channel] of channels.entries())
      await listen(channel, handlers[index]);
    dispatch.get(channels[1])!("terminal bytes");
    expect(handlers.map((handler) => handler.mock.calls.length)).toEqual([
      0, 1, 0, 0,
    ]);
    expect(register).toHaveBeenCalledTimes(4);
  });

  it("retains ordinary registration and unlisten semantics for unknown channels", async () => {
    const { listen, register, nativeStop } = setup();
    const stop = await listen("custom:event", vi.fn());
    stop();
    await listen("custom:event", vi.fn());
    expect(nativeStop).toHaveBeenCalledTimes(1);
    expect(register).toHaveBeenCalledTimes(2);
  });
});
