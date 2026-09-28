import { expect, it, vi } from "vitest";
import { createVerificationReader } from "./verificationRefresh";

const rows = (ids: string[]) => ({
  verifications: Object.fromEntries(
    ids.map((id) => [id, { status: "passed" }]),
  ),
});

it("batches and deduplicates the same synchronous burst without caching later reads", async () => {
  const send = vi.fn(async (ids: string[]) => rows(ids));
  const read = createVerificationReader(send);
  await Promise.all([read("a"), read("b"), read("a")]);
  expect(send).toHaveBeenCalledOnce();
  expect(send).toHaveBeenCalledWith(["a", "b"]);
  await read("a");
  expect(send).toHaveBeenCalledTimes(2);
});

it("serializes chunks of at most 32 while a request is unresolved", async () => {
  let resolve!: (value: ReturnType<typeof rows>) => void;
  const send = vi
    .fn()
    .mockImplementationOnce(
      () =>
        new Promise((done) => {
          resolve = done;
        }),
    )
    .mockImplementation(async (ids: string[]) => rows(ids));
  const read = createVerificationReader(send);
  const results = Array.from({ length: 70 }, (_, i) => read(String(i)));
  await Promise.resolve();
  expect(send).toHaveBeenCalledOnce();
  expect(send.mock.calls[0][0]).toHaveLength(32);
  resolve(rows(send.mock.calls[0][0]));
  await Promise.all(results);
  expect(send.mock.calls.map((call) => call[0].length)).toEqual([32, 32, 6]);
});

it("drops cards hidden or removed before dispatch and bounds the pending queue", async () => {
  const send = vi.fn(async (ids: string[]) => rows(ids));
  const read = createVerificationReader(send);
  const controller = new AbortController();
  const cancelled = read("hidden", controller.signal);
  controller.abort();
  await expect(cancelled).rejects.toThrow("cancelled");
  expect(send).not.toHaveBeenCalled();
  const requests = Array.from({ length: 257 }, (_, i) => read(String(i)));
  const results = await Promise.allSettled(requests);
  expect(
    results.filter((result) => result.status === "fulfilled"),
  ).toHaveLength(256);
  expect(results[256].status).toBe("rejected");
});

it("rejects every card on a failed or incomplete batch", async () => {
  const send = vi
    .fn()
    .mockRejectedValueOnce(new Error("unknown job"))
    .mockResolvedValueOnce(rows(["a"]));
  const read = createVerificationReader(send);
  for (let i = 0; i < 2; i++) {
    const results = await Promise.allSettled([read("a"), read("b")]);
    expect(results.every((result) => result.status === "rejected")).toBe(true);
  }
});
