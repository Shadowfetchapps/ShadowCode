import { expect, it, vi } from "vitest";
import {
  createMarkdownWorkerQueue,
  type MarkdownWorker,
} from "./markdownWorker";

function fixture() {
  const worker: MarkdownWorker = {
    postMessage: vi.fn(),
    terminate: vi.fn(),
    onmessage: null,
    onerror: null,
    onmessageerror: null,
  };
  const factory = vi.fn(() => worker);
  const queue = createMarkdownWorkerQueue(factory);
  const reply = (id: number) =>
    worker.onmessage?.({
      data: { id, tree: { type: "root", children: [] } },
    } as MessageEvent);
  return { worker, factory, queue, reply };
}
it("bounds a burst to one active parse and the latest pending snapshot per response", () => {
  const { worker, queue, reply } = fixture();
  const result = vi.fn();
  const client = queue.subscribe(result, vi.fn());
  client.update("first", 0);
  for (let i = 0; i < 10_000; i++) client.update(`latest ${i}`, 0);
  expect(worker.postMessage).toHaveBeenCalledTimes(1);
  reply(vi.mocked(worker.postMessage).mock.calls[0][0].id);
  expect(result).toHaveBeenCalledTimes(1);
  expect(worker.postMessage).toHaveBeenCalledTimes(2);
  expect(vi.mocked(worker.postMessage).mock.calls[1][0].text).toBe(
    "latest 9999",
  );
});
it("shares one worker, fairly advances other responses, and terminates after final disposal", () => {
  const { worker, factory, queue, reply } = fixture();
  const firstResult = vi.fn();
  const first = queue.subscribe(firstResult, vi.fn());
  const second = queue.subscribe(vi.fn(), vi.fn());
  first.update("first active", 0);
  second.update("second pending", 3);
  first.update("first newest", 0);
  first.dispose();
  reply(vi.mocked(worker.postMessage).mock.calls[0][0].id);
  expect(firstResult).not.toHaveBeenCalled();
  expect(vi.mocked(worker.postMessage).mock.calls[1][0].text).toBe(
    "second pending",
  );
  expect(factory).toHaveBeenCalledTimes(1);
  second.dispose();
  expect(worker.terminate).toHaveBeenCalledTimes(1);
});
it("falls back once after a constructor/CSP failure instead of retrying on every token", () => {
  const factory = vi.fn(() => {
    throw new Error("blocked by CSP");
  });
  const queue = createMarkdownWorkerQueue(factory);
  const failed = vi.fn();
  const client = queue.subscribe(vi.fn(), failed);
  client.update("first", 0);
  client.update("second", 0);
  expect(factory).toHaveBeenCalledTimes(1);
  expect(failed).toHaveBeenCalledTimes(2);
});
it("worker failure releases queued state and signals every current response", () => {
  const { worker, queue } = fixture();
  const failed = vi.fn();
  const first = queue.subscribe(vi.fn(), failed);
  const second = queue.subscribe(vi.fn(), failed);
  first.update("first", 0);
  second.update("second", 0);
  worker.onerror?.(new ErrorEvent("error"));
  expect(worker.terminate).toHaveBeenCalledTimes(1);
  expect(failed).toHaveBeenCalledTimes(2);
  expect(worker.postMessage).toHaveBeenCalledTimes(1);
});
it("late errors from a retired worker cannot fail its replacement", () => {
  const old = fixture().worker;
  const current = fixture().worker;
  const queue = createMarkdownWorkerQueue(
    vi.fn().mockReturnValueOnce(old).mockReturnValueOnce(current),
  );
  const first = queue.subscribe(vi.fn(), vi.fn());
  first.update("old", 0);
  const lateError = old.onerror;
  const lateMessageError = old.onmessageerror;
  first.dispose();
  expect(old.onerror).toBeNull();
  const failed = vi.fn();
  const second = queue.subscribe(vi.fn(), failed);
  second.update("current", 0);
  lateError?.(new ErrorEvent("error"));
  lateMessageError?.({} as MessageEvent);
  expect(current.terminate).not.toHaveBeenCalled();
  expect(failed).not.toHaveBeenCalled();
});
it("attaches document failure to its request and continues the latest queued parse", () => {
  const { worker, queue, reply } = fixture();
  const result = vi.fn();
  const failed = vi.fn();
  const client = queue.subscribe(result, failed);
  client.update("old", 0);
  client.update("replacement", 1);
  const firstId = vi.mocked(worker.postMessage).mock.calls[0][0].id;
  worker.onmessage?.({
    data: { id: firstId, error: "parse failure" },
  } as MessageEvent);
  expect(failed).toHaveBeenCalledWith({
    kind: "document",
    generation: 0,
    id: firstId,
  });
  expect(worker.terminate).not.toHaveBeenCalled();
  const secondId = vi.mocked(worker.postMessage).mock.calls[1][0].id;
  reply(secondId);
  expect(result).toHaveBeenCalledWith(
    { type: "root", children: [] },
    1,
    secondId,
  );
});
