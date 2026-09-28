type Handler = (payload: unknown) => unknown;
type Register = (event: string, handler: Handler) => Promise<() => void>;
type Subscription = { handler: Handler };
type Channel = {
  subscribers: Set<Subscription>;
  ready?: Promise<void>;
};

const sharedChannels = new Set([
  "shadowcode:events",
  "shadowcode:terminal",
  "shadowcode:open-session",
  "shadowcode:shutdown",
]);

function reportHandlerError(error: unknown) {
  if (typeof globalThis.reportError === "function")
    globalThis.reportError(error);
  else
    setTimeout(() => {
      throw error;
    }, 0);
}

/** Keep one native registration per known channel for this renderer document.
 * Tauri unregisters callbacks but retains listener metadata in its JS registry;
 * creating a native listener for each task therefore accumulates dead records.
 * Local subscriber tokens disappear on stop; the at-most-four native callbacks
 * are released with the document. Unknown channels retain ordinary semantics. */
export function createSharedNativeEvents(
  register: Register,
  onHandlerError: (error: unknown) => void = reportHandlerError,
) {
  const channels = new Map<string, Channel>();
  return async (event: string, handler: Handler): Promise<() => void> => {
    if (!sharedChannels.has(event)) return register(event, handler);
    let channel = channels.get(event);
    if (!channel) {
      channel = { subscribers: new Set() };
      channels.set(event, channel);
    }
    const current = channel;
    // A token, rather than the handler itself, preserves independent duplicate
    // subscriptions and makes each returned cleanup idempotent.
    const subscription = { handler };
    current.subscribers.add(subscription);
    current.ready ??= Promise.resolve()
      .then(() =>
        register(event, (payload) => {
          for (const token of [...current.subscribers]) {
            if (!current.subscribers.has(token)) continue;
            try {
              const result = token.handler(payload);
              if (
                result &&
                typeof (result as PromiseLike<unknown>).then === "function"
              )
                void Promise.resolve(result).catch(onHandlerError);
            } catch (error) {
              onHandlerError(error);
            }
          }
        }),
      )
      .then(() => undefined)
      .catch((error: unknown) => {
        // Only evict this failed registration; a later retry gets a fresh set.
        if (channels.get(event) === current) channels.delete(event);
        throw error;
      });
    try {
      await current.ready;
    } catch (error) {
      current.subscribers.delete(subscription);
      throw error;
    }
    return () => {
      current.subscribers.delete(subscription);
    };
  };
}
