import type { MarkdownTree } from "./markdownTree";

type Request = { id: number; owner: number; generation: number; text: string };
type Reply = { id: number; tree?: MarkdownTree; error?: string };
export type MarkdownWorkerFailure =
  | { kind: "unavailable" }
  | { kind: "document"; generation: number; id: number };
export type MarkdownWorker = Pick<Worker, "postMessage" | "terminate"> & {
  onmessage: ((event: MessageEvent<Reply>) => void) | null;
  onerror: ((event: ErrorEvent) => void) | null;
  onmessageerror: ((event: MessageEvent) => void) | null;
};
type Consumer = {
  result: (tree: MarkdownTree, generation: number, id: number) => void;
  failure: (reason: MarkdownWorkerFailure) => void;
};

/** One worker for mounted long responses, one in-flight parse, and at most
 * one pending snapshot per response. Superseded snapshots never build an
 * unbounded queue. Completed prefixes may render while a newer one parses.
 */
export function createMarkdownWorkerQueue(factory: () => MarkdownWorker) {
  let worker: MarkdownWorker | null = null;
  let failed = false;
  let serial = 0;
  let active: Request | null = null;
  const consumers = new Map<number, Consumer>();
  const pending = new Map<number, Request>();
  const retire = () => {
    if (!worker) return;
    worker.onmessage = null;
    worker.onerror = null;
    worker.onmessageerror = null;
    worker.terminate();
    worker = null;
  };
  const fail = () => {
    failed = true;
    retire();
    active = null;
    pending.clear();
    consumers.forEach((consumer) => consumer.failure({ kind: "unavailable" }));
  };
  function pump() {
    if (active || failed || !pending.size) return;
    try {
      if (!worker) {
        worker = factory();
        const instance = worker;
        worker.onmessage = ({ data }) => {
          if (worker !== instance) return;
          if (!active || data.id !== active.id) return;
          const completed = active;
          active = null;
          const consumer = consumers.get(completed.owner);
          if (data.tree)
            consumer?.result(data.tree, completed.generation, completed.id);
          else
            consumer?.failure({
              kind: "document",
              generation: completed.generation,
              id: completed.id,
            });
          pump();
        };
        const workerFailed = () => {
          if (worker === instance) fail();
        };
        worker.onerror = workerFailed;
        worker.onmessageerror = workerFailed;
      }
      const next = pending.values().next().value as Request;
      pending.delete(next.owner);
      active = next;
      worker.postMessage({ id: next.id, text: next.text });
    } catch {
      // Unsupported workers or a restrictive CSP retain the synchronous path.
      fail();
    }
  }
  return {
    subscribe(result: Consumer["result"], failure: Consumer["failure"]) {
      const owner = ++serial;
      consumers.set(owner, { result, failure });
      return {
        update(text: string, generation: number) {
          if (!consumers.has(owner)) return;
          if (failed) return failure({ kind: "unavailable" });
          pending.set(owner, { id: ++serial, owner, generation, text });
          pump();
        },
        dispose() {
          consumers.delete(owner);
          pending.delete(owner);
          if (!consumers.size) {
            retire();
            active = null;
          }
        },
      };
    },
  };
}

export const markdownWorkerQueue = createMarkdownWorkerQueue(
  () =>
    new Worker(new URL("../workers/markdown.worker.ts", import.meta.url), {
      type: "module",
    }),
);
