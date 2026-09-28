import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { Markdown, MARKDOWN_PREVIEW_LIMIT, safeMarkdownHref } from "./Markdown";

const parses = vi.hoisted(() => vi.fn());
vi.mock("react-markdown", async (original) => {
  const actual = await original<typeof import("react-markdown")>();
  return {
    ...actual,
    default: (props: Parameters<typeof actual.default>[0]) => {
      parses(props.children);
      return actual.default(props);
    },
  };
});
afterEach(() => {
  cleanup();
  parses.mockClear();
  vi.unstubAllGlobals();
});

it("parses only the changing response while earlier Markdown stays interactive", async () => {
  const writeText = vi.fn().mockResolvedValue(undefined);
  vi.stubGlobal("navigator", { clipboard: { writeText } });
  function Conversation({ response }: { response: string }) {
    return (
      <>
        <Markdown>
          {
            "## Earlier answer\n\n[Reference](https://example.com)\n\n```js\nconst answer = 42;\n```"
          }
        </Markdown>
        <Markdown>{response}</Markdown>
      </>
    );
  }
  const view = render(<Conversation response="Starting" />);
  expect(parses).toHaveBeenCalledTimes(2);
  for (let index = 0; index < 100; index++) {
    view.rerender(<Conversation response={`Streaming **${index}**`} />);
  }
  expect(parses).toHaveBeenCalledTimes(102);
  expect(screen.getByRole("heading", { name: "Earlier answer" })).toBeTruthy();
  expect(
    screen.getByRole("link", { name: "Reference" }).getAttribute("href"),
  ).toBe("https://example.com");
  expect(screen.getByRole("button", { name: "Copy code" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Copy code" }));
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Code copied" })).toBeTruthy(),
  );
  expect(writeText).toHaveBeenCalledWith("const answer = 42;\n");
  expect(screen.getByText("99").tagName).toBe("STRONG");
  view.rerender(<Conversation response="Streaming **99**" />);
  expect(parses).toHaveBeenCalledTimes(102);
});

it("bounds a very large response until the reader explicitly expands it", () => {
  const large = `# Large response\n\n${"x".repeat(MARKDOWN_PREVIEW_LIMIT + 1024)}`;
  render(<Markdown>{large}</Markdown>);
  expect(parses).toHaveBeenCalledTimes(1);
  expect(String(parses.mock.calls[0][0])).toHaveLength(MARKDOWN_PREVIEW_LIMIT);
  expect(screen.getByRole("status").textContent).toContain("512 KiB");
  expect(
    screen.getByRole("button", { name: "Show full response" }),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Show full response" }));
  expect(parses).toHaveBeenCalledTimes(2);
  expect(parses.mock.calls[1][0]).toBe(large);
  expect(
    screen.queryByRole("button", { name: "Show full response" }),
  ).toBeNull();
});

it("keeps only http(s) and fragment Markdown links clickable", () => {
  expect(safeMarkdownHref("https://example.com/docs")).toBe(
    "https://example.com/docs",
  );
  expect(safeMarkdownHref("#section")).toBe("#section");
  expect(safeMarkdownHref("javascript:alert(1)")).toBeUndefined();
  expect(safeMarkdownHref("data:text/html,hi")).toBeUndefined();
  expect(safeMarkdownHref("file:///etc/passwd")).toBeUndefined();
  expect(safeMarkdownHref("README.md")).toBeUndefined();
  expect(safeMarkdownHref("https://user:secret@example.com")).toBeUndefined();
  render(
    <Markdown>
      {
        "[Safe](https://example.com)\n[Script](javascript:alert(1))\n[Local](README.md)"
      }
    </Markdown>,
  );
  expect(screen.getByRole("link", { name: "Safe" }).getAttribute("href")).toBe(
    "https://example.com",
  );
  expect(screen.queryByRole("link", { name: "Script" })).toBeNull();
  expect(screen.queryByRole("link", { name: "Local" })).toBeNull();
  expect(screen.getByText("Script")).toBeTruthy();
  expect(screen.getByText("Local")).toBeTruthy();
});
