import { afterEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { Markdown, MARKDOWN_WORKER_THRESHOLD } from "./Markdown";
import { parseMarkdownTree } from "../lib/markdownTree";
import { markdownWorkerQueue } from "../lib/markdownWorker";

vi.mock("../lib/markdownWorker", () => ({
  markdownWorkerQueue: { subscribe: vi.fn() },
}));
afterEach(() => {
  cleanup();
  vi.resetAllMocks();
  vi.unstubAllGlobals();
});
function setup() {
  vi.stubGlobal("Worker", class {});
  const update = vi.fn();
  const dispose = vi.fn();
  let result!: Parameters<typeof markdownWorkerQueue.subscribe>[0];
  let failure!: Parameters<typeof markdownWorkerQueue.subscribe>[1];
  vi.mocked(markdownWorkerQueue.subscribe).mockImplementation(
    (onResult, onFailure) => {
      result = onResult;
      failure = onFailure;
      return { update, dispose };
    },
  );
  return {
    update,
    dispose,
    publish: (text: string, generation: number, id: number) =>
      act(() => result(parseMarkdownTree(text), generation, id)),
    fail: () => act(() => failure({ kind: "unavailable" })),
    parseFailure: (generation: number, id: number) =>
      act(() => failure({ kind: "document", generation, id })),
  };
}
const long = (heading: string) =>
  `# ${heading}\n\n${"Some prose. ".repeat(MARKDOWN_WORKER_THRESHOLD / 10)}\n\n`;
it("shows completed stream prefixes without starving and rejects superseded/reordered results", () => {
  const worker = setup();
  const first = long("First");
  const view = render(<Markdown>{first}</Markdown>);
  view.rerender(<Markdown>{first + "Appended."}</Markdown>);
  worker.publish(first, 0, 1);
  expect(screen.getByRole("heading", { name: "First" })).toBeTruthy();
  worker.publish(first + "Appended.", 0, 3);
  worker.publish(first, 0, 2);
  expect(screen.getByText("Appended.")).toBeTruthy();
  const replacement = long("Final replacement");
  view.rerender(<Markdown>{replacement}</Markdown>);
  worker.publish(first, 0, 4);
  expect(screen.queryByRole("heading", { name: "First" })).toBeNull();
  worker.publish(replacement, 1, 5);
  expect(
    screen.getByRole("heading", { name: "Final replacement" }),
  ).toBeTruthy();
  view.unmount();
  expect(worker.dispose).toHaveBeenCalledOnce();
});
it("preserves code copy and safe-link/image behavior after a worker result", async () => {
  const worker = setup();
  const writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  const text =
    long("Worker result") +
    "```ts\nconst n = 1;\n\nconsole.log(n);\n```\n\n[Reference][later]\n\n[later]: https://example.com\n\n[bad](javascript:alert(1))\n\n![remote](https://example.com/image.png)";
  render(<Markdown>{text}</Markdown>);
  worker.publish(text, 0, 1);
  expect(
    screen.getByRole("link", { name: "Reference" }).getAttribute("href"),
  ).toBe("https://example.com");
  expect(screen.queryByRole("link", { name: "bad" })).toBeNull();
  expect(document.querySelector("img")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Copy code" }));
  await waitFor(() =>
    expect(writeText).toHaveBeenCalledWith("const n = 1;\n\nconsole.log(n);\n"),
  );
});
it("keeps fully formatted content when worker startup is unavailable", () => {
  const worker = setup();
  render(<Markdown>{long("Fallback answer")}</Markdown>);
  worker.fail();
  expect(screen.getByRole("heading", { name: "Fallback answer" })).toBeTruthy();
});
it("keeps a parse failure as readable source and recovers on a newer snapshot", () => {
  const worker = setup();
  const first = long("Readable failure");
  const view = render(<Markdown>{first}</Markdown>);
  worker.parseFailure(0, 1);
  expect(screen.queryByRole("heading")).toBeNull();
  expect(view.container.textContent).toContain(first);
  const next = first + "New text.";
  view.rerender(<Markdown>{next}</Markdown>);
  expect(worker.update).toHaveBeenLastCalledWith(next, 0);
  worker.publish(next, 0, 2);
  worker.parseFailure(0, 1);
  expect(
    screen.getByRole("heading", { name: "Readable failure" }),
  ).toBeTruthy();
});
it("ignores a superseded document's parse failure after final replacement", () => {
  const worker = setup();
  const view = render(<Markdown>{long("Old answer")}</Markdown>);
  const final = long("Final answer");
  view.rerender(<Markdown>{final}</Markdown>);
  worker.parseFailure(0, 1);
  expect(screen.queryByRole("heading")).toBeNull();
  worker.publish(final, 1, 2);
  expect(screen.getByRole("heading", { name: "Final answer" })).toBeTruthy();
});
