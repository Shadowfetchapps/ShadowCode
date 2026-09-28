import { afterEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { EditorView } from "@codemirror/view";
import { StrictMode } from "react";
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
  const { memory, update, discardFileDraft, resolveFileDraftConflict } =
    useDrawerMemory(workspace);
  return (
    <FileEditor
      workspace={workspace}
      memory={memory}
      onMemory={update}
      onDiscardFileDraft={discardFileDraft}
      onResolveFileDraftConflict={resolveFileDraftConflict}
      onShowDiff={() => {}}
      toast={() => {}}
    />
  );
}

function editorView(element: HTMLElement) {
  const view = EditorView.findFromDOM(element);
  if (!view) throw new Error("Expected a mounted code editor");
  return view;
}

function replaceText(element: HTMLElement, text: string) {
  const view = editorView(element);
  act(() =>
    view.dispatch({
      changes: { from: 0, to: view.state.doc.length, insert: text },
    }),
  );
}

it("preserves mixed separators when indenting selected lines and saving", async () => {
  const content = "one\r\ntwo\rthree\nfour\r\n";
  vi.mocked(api.files).mockResolvedValue({
    entries: [{ name: "mixed.txt", path: "mixed.txt", type: "file" }],
    workspace: "/mixed",
    path: ".",
    parent: "",
  });
  vi.mocked(api.file).mockResolvedValue({
    path: "mixed.txt",
    content,
    hash: "d".repeat(64),
    bytes: content.length,
    truncated: false,
  });
  vi.mocked(api.fileRevision).mockResolvedValue({
    path: "mixed.txt",
    hash: "d".repeat(64),
    bytes: content.length,
  });
  vi.mocked(api.saveFile).mockResolvedValue({
    path: "mixed.txt",
    hash: "e".repeat(64),
    bytes: content.length + 4,
  });
  render(
    <StrictMode>
      <Harness workspace="/mixed" />
    </StrictMode>,
  );
  fireEvent.click(await screen.findByRole("button", { name: "mixed.txt" }));
  const editor = (await screen.findByRole("textbox", {
    name: "Edit mixed.txt",
  })) as HTMLTextAreaElement;
  expect(editor.tagName).toBe("TEXTAREA");
  editor.setSelectionRange(1, 8);
  fireEvent.keyDown(editor, { key: "Tab" });
  await waitFor(() => expect(editor.selectionEnd).toBe(11));
  fireEvent.keyDown(editor, { key: "Tab", shiftKey: true });
  await waitFor(() => expect(editor.value).toBe("one\ntwo\nthree\nfour\n"));
  editor.setSelectionRange(1, 8);
  fireEvent.keyDown(editor, { key: "Tab" });
  fireEvent.keyDown(editor, { key: "s", ctrlKey: true });
  await waitFor(() =>
    expect(api.saveFile).toHaveBeenCalledWith(
      "mixed.txt",
      "  one\r\n  two\rthree\nfour\r\n",
      "d".repeat(64),
    ),
  );
});

it("indents and outdents selected lines without replacing the code", async () => {
  const content = "one\ntwo\nthree\n";
  vi.mocked(api.files).mockResolvedValue({
    entries: [{ name: "block.ts", path: "block.ts", type: "file" }],
    workspace: "/indent",
    path: ".",
    parent: "",
  });
  vi.mocked(api.file).mockResolvedValue({
    path: "block.ts",
    content,
    hash: "d".repeat(64),
    bytes: content.length,
    truncated: false,
  });
  vi.mocked(api.fileRevision).mockResolvedValue({
    path: "block.ts",
    hash: "d".repeat(64),
    bytes: content.length,
  });
  render(<Harness workspace="/indent" />);
  fireEvent.click(await screen.findByRole("button", { name: "block.ts" }));
  const editor = await screen.findByRole("textbox", {
    name: "Edit block.ts",
  });
  const view = editorView(editor);
  act(() => view.dispatch({ selection: { anchor: 1, head: 8 } }));
  fireEvent.keyDown(editor, { key: "Tab" });
  await waitFor(() =>
    expect(view.state.sliceDoc()).toBe("  one\n  two\nthree\n"),
  );
  fireEvent.keyDown(editor, { key: "Tab", shiftKey: true });
  await waitFor(() => expect(view.state.sliceDoc()).toBe(content));
  expect(view.state.selection.main.from).toBe(1);
  expect(view.state.selection.main.to).toBe(8);
});

it("keeps the active editor while a read is pending and ignores a superseded open", async () => {
  let finish!: (file: Awaited<ReturnType<typeof api.file>>) => void;
  const pending = new Promise<Awaited<ReturnType<typeof api.file>>>(
    (resolve) => {
      finish = resolve;
    },
  );
  vi.mocked(api.files).mockResolvedValue({
    entries: ["a.ts", "b.ts"].map((path) => ({
      name: path,
      path,
      type: "file" as const,
    })),
    workspace: "/pending",
    path: ".",
    parent: "",
  });
  vi.mocked(api.file).mockImplementation(async (path) =>
    path === "b.ts"
      ? pending
      : {
          path,
          content: "alpha",
          hash: "a".repeat(64),
          bytes: 5,
          truncated: false,
        },
  );
  vi.mocked(api.fileRevision).mockResolvedValue({
    path: "a.ts",
    hash: "a".repeat(64),
    bytes: 5,
  });
  render(<Harness workspace="/pending" />);
  fireEvent.click(await screen.findByRole("button", { name: "a.ts" }));
  const editor = await screen.findByRole("textbox", { name: "Edit a.ts" });
  const originalView = editorView(editor);
  replaceText(editor, "alpha!");
  fireEvent.click(screen.getByRole("button", { name: "b.ts" }));
  expect(screen.getByText("Opening b.ts…")).toBeTruthy();
  expect(editorView(editor)).toBe(originalView);
  fireEvent.click(screen.getByRole("button", { name: "Open a.ts, unsaved" }));
  await act(async () =>
    finish({
      path: "b.ts",
      content: "beta",
      hash: "b".repeat(64),
      bytes: 4,
      truncated: false,
    }),
  );
  expect(screen.queryByRole("textbox", { name: "Edit b.ts" })).toBeNull();
  expect(screen.queryByText("Opening b.ts…")).toBeNull();
  expect(editorView(editor)).toBe(originalView);
  expect(originalView.state.sliceDoc()).toBe("alpha!");
});

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
  replaceText(editor, "const value = 2;\n");
  expect(editorView(editor).state.sliceDoc()).toBe("const value = 2;\n");
  fireEvent.keyDown(editor, { key: "Escape" });
  expect(document.activeElement).toBe(
    screen.getByRole("button", { name: "Diff" }),
  );

  view.rerender(<Harness workspace="/b" />);
  disk = { content: "const value = 3;\n", hash: "b".repeat(64) };
  view.rerender(<Harness workspace="/a" />);
  await screen.findByRole("group", { name: "File conflict" });
  expect(
    editorView(
      screen.getByRole("textbox", {
        name: "Edit source.ts",
      }),
    ).state.sliceDoc(),
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

it("preserves typing made while a save is in flight", async () => {
  let disk = { content: "const value = 1;\n", hash: "a".repeat(64) };
  let finishSave!: (result: Awaited<ReturnType<typeof api.saveFile>>) => void;
  vi.mocked(api.files).mockResolvedValue({
    entries: [{ name: "live.ts", path: "live.ts", type: "file" }],
    workspace: "/save-race",
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
  vi.mocked(api.saveFile).mockImplementation((path, content) => {
    if (vi.mocked(api.saveFile).mock.calls.length > 1) {
      disk = { content, hash: "c".repeat(64) };
      return Promise.resolve({ path, hash: disk.hash, bytes: content.length });
    }
    return new Promise((resolve) => {
      finishSave = resolve;
    });
  });

  const rendered = render(<Harness workspace="/save-race" />);
  fireEvent.click(await screen.findByRole("button", { name: "live.ts" }));
  const editor = await screen.findByRole("textbox", { name: "Edit live.ts" });
  replaceText(editor, "const value = 2;\n");
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(api.saveFile).toHaveBeenCalledTimes(1));

  replaceText(editor, "const value = 3;\n");
  disk = { content: "const value = 2;\n", hash: "b".repeat(64) };
  await act(async () =>
    finishSave({
      path: "live.ts",
      hash: "b".repeat(64),
      bytes: 18,
    }),
  );
  expect(editorView(editor).state.sliceDoc()).toBe("const value = 3;\n");
  await waitFor(() => expect(screen.getByText("Unsaved changes")).toBeTruthy());
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(api.saveFile).toHaveBeenCalledTimes(2));
  expect(api.saveFile).toHaveBeenLastCalledWith(
    "live.ts",
    "const value = 3;\n",
    "b".repeat(64),
  );
  rendered.rerender(<Harness workspace="/another-project" />);
  rendered.rerender(<Harness workspace="/save-race" />);
  expect(
    editorView(
      await screen.findByRole("textbox", { name: "Edit live.ts" }),
    ).state.sliceDoc(),
  ).toBe("const value = 3;\n");
});
