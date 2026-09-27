import { afterEach, expect, it, vi } from "vitest";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { api } from "../api";
import { useDrawerMemory } from "../hooks/useDrawerMemory";
import { FileEditor } from "./FileEditor";

vi.mock("../api", () => ({
  api: {
    files: vi.fn(),
    file: vi.fn(),
    fileRevision: vi.fn(),
    saveFile: vi.fn(),
  },
}));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

function Harness({ workspace }: { workspace: string }) {
  const { memory, update } = useDrawerMemory(workspace);
  return (
    <FileEditor
      workspace={workspace}
      memory={memory}
      onMemory={update}
      onShowDiff={() => {}}
      toast={() => {}}
    />
  );
}

it("keeps a project draft and requires explicit review before saving over an agent edit", async () => {
  let disk = { content: "const value = 1;\n", hash: "a".repeat(64) };
  vi.mocked(api.files).mockResolvedValue({
    entries: [{ name: "source.ts", path: "source.ts", type: "file" }],
    workspace: "/a",
    path: ".",
    parent: "",
  });
  vi.mocked(api.file).mockImplementation(async (path) => ({
    path,
    ...disk,
    bytes: disk.content.length,
    truncated: false,
  }));
  vi.mocked(api.fileRevision).mockImplementation(async (path) => ({
    path,
    hash: disk.hash,
    bytes: disk.content.length,
  }));
  vi.mocked(api.saveFile).mockImplementation(
    async (path, content, expectedHash) => {
      if (expectedHash !== disk.hash)
        throw new Error("File changed since it was read");
      disk = { content, hash: "c".repeat(64) };
      return { path, hash: disk.hash, bytes: content.length };
    },
  );

  const view = render(<Harness workspace="/a" />);
  fireEvent.click(await screen.findByRole("button", { name: "source.ts" }));
  const editor = await screen.findByRole("textbox", { name: "Edit source.ts" });
  fireEvent.change(editor, { target: { value: "const value = 2;\n" } });
  expect((editor as HTMLTextAreaElement).value).toBe("const value = 2;\n");
  fireEvent.keyDown(editor, { key: "Escape" });
  expect(document.activeElement).toBe(
    screen.getByRole("button", { name: "Diff" }),
  );

  view.rerender(<Harness workspace="/b" />);
  disk = { content: "const value = 3;\n", hash: "b".repeat(64) };
  view.rerender(<Harness workspace="/a" />);
  await screen.findByRole("group", { name: "File conflict" });
  expect(
    (
      screen.getByRole("textbox", {
        name: "Edit source.ts",
      }) as HTMLTextAreaElement
    ).value,
  ).toBe("const value = 2;\n");
  expect(
    (screen.getByRole("button", { name: "Save" }) as HTMLButtonElement)
      .disabled,
  ).toBe(true);
  expect(api.saveFile).not.toHaveBeenCalled();

  fireEvent.click(screen.getByText("Show current disk version"));
  expect(screen.getByText("const value = 3;")).toBeTruthy();
  fireEvent.click(
    screen.getByRole("button", { name: "Use disk revision as save base" }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() =>
    expect(api.saveFile).toHaveBeenCalledWith(
      "source.ts",
      "const value = 2;\n",
      "b".repeat(64),
    ),
  );
  await waitFor(() => expect(screen.getByText("Saved")).toBeTruthy());
  const close = new Event("beforeunload", { cancelable: true });
  window.dispatchEvent(close);
  expect(close.defaultPrevented).toBe(false);
});
