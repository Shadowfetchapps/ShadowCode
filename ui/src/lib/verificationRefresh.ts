/** One cadence for all visible summaries, so their reads share an API batch. */
export const VERIFICATION_REFRESH_MS = 5_000;
const subscribers = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | undefined;
export function onVerificationRefresh(refresh: () => void) {
  subscribers.add(refresh);
  timer ??= setInterval(() => {
    if (document.visibilityState !== "hidden")
      for (const callback of [...subscribers]) callback();
  }, VERIFICATION_REFRESH_MS);
  return () => {
    subscribers.delete(refresh);
    if (!subscribers.size) {
      clearInterval(timer);
      timer = undefined;
    }
  };
}

type Row = Record<string, unknown>;
type Waiter = {
  signal?: AbortSignal;
  resolve: (value: Row) => void;
  reject: (error: unknown) => void;
};
/** Serial batches, with no cache: each read assesses current workspace contents.
 * The finite queue also bounds pressure during rapid conversation switching. */
export function createVerificationReader(
  send: (ids: string[]) => Promise<{ verifications: Record<string, Row> }>,
) {
  const pending = new Map<string, Waiter[]>();
  let queued = 0;
  let scheduled = false;
  let running = false;
  const aborted = () =>
    new DOMException("Verification read cancelled", "AbortError");
  const flush = async () => {
    scheduled = false;
    if (running) return;
    const batch = new Map<string, Waiter[]>();
    for (const [id, waiters] of pending) {
      pending.delete(id);
      queued -= waiters.length;
      const active = waiters.filter((waiter) => {
        if (!waiter.signal?.aborted) return true;
        waiter.reject(aborted());
        return false;
      });
      if (active.length) batch.set(id, active);
      if (batch.size === 32) break;
    }
    if (!batch.size) return;
    running = true;
    try {
      const result = await send([...batch.keys()]);
      // Fail the complete batch if transport returned an incomplete payload.
      for (const id of batch.keys())
        if (!Object.hasOwn(result.verifications ?? {}, id))
          throw new Error("Incomplete verification refresh");
      for (const [id, waiters] of batch)
        for (const waiter of waiters)
          if (waiter.signal?.aborted) waiter.reject(aborted());
          else waiter.resolve(result.verifications[id]);
    } catch (error) {
      for (const waiters of batch.values())
        for (const waiter of waiters) waiter.reject(error);
    } finally {
      running = false;
      if (pending.size) schedule();
    }
  };
  const schedule = () => {
    if (!scheduled && !running) {
      scheduled = true;
      queueMicrotask(() => void flush());
    }
  };
  return (id: string, signal?: AbortSignal): Promise<Row> => {
    if (signal?.aborted) return Promise.reject(aborted());
    if (!id.trim())
      return Promise.reject(new Error("Missing verification job"));
    const existing = pending.get(id);
    if (queued >= 256)
      return Promise.reject(new Error("Verification refresh queue is full"));
    return new Promise((resolve, reject) => {
      const waiters = existing ?? [];
      waiters.push({ resolve, reject, signal });
      queued++;
      pending.set(id, waiters);
      schedule();
    });
  };
}
