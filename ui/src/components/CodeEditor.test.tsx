import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { EditorState, Facet, type Extension } from "@codemirror/state";
import { EditorView } from "@codemirror/view";
import { insertNewline, undo, undoDepth } from "@codemirror/commands";
import { loadEditorLanguage } from "../lib/editorLanguage";
import { CodeEditor, type CodeEditorProps } from "./CodeEditor";

// Keep real CodeMirror state, transactions, history and DOM. Only the async
// language-chunk boundary is controlled so completion order is deterministic.
vi.mock("../lib/editorLanguage", () => ({ loadEditorLanguage: vi.fn() }));

beforeEach(() => {
  vi.mocked(loadEditorLanguage).mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  vi.resetAllMocks();
});

function props(overrides: Partial<CodeEditorProps> = {}): CodeEditorProps {
  return {
    workspace: "/project-a",
    path: "a.ts",
    value: "alpha",
    openPaths: ["a.ts", "b.py"],
    onChange: vi.fn(),
    onSave: vi.fn(),
    onEscape: vi.fn(),
    ...overrides,
  };
}

function editor(path = "a.ts"): EditorView {
  const content = screen.getByRole("textbox", { name: `Edit ${path}` });
  const view = EditorView.findFromDOM(content);
  if (!view) throw new Error(`Missing real CodeMirror view for ${path}`);
  return view;
}

function save(view: EditorView) {
  fireEvent.keyDown(view.contentDOM, {
    key: "s",
    code: "KeyS",
    ctrlKey: true,
  });
}

function deferred() {
  let resolve!: (extension: Extension) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<Extension>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

it("keeps selection and undo through the parent's ordinary value echo", () => {
  const initial = props();
  const rendered = render(<CodeEditor {...initial} />);
  const view = editor();
  act(() => {
    view.dispatch({
      changes: { from: 5, insert: "!" },
      selection: { anchor: 6 },
      userEvent: "input.type",
    });
  });
  expect(initial.onChange).toHaveBeenCalledExactlyOnceWith("alpha!");
  expect(undoDepth(view.state)).toBe(1);
  rendered.rerender(<CodeEditor {...initial} value="alpha!" />);
  expect(editor()).toBe(view);
  expect(view.state.selection.main.anchor).toBe(6);
  expect(undoDepth(view.state)).toBe(1);
  act(() => {
    expect(undo(view)).toBe(true);
  });
  expect(view.state.sliceDoc()).toBe("alpha");
  expect(initial.onChange).toHaveBeenLastCalledWith("alpha");
});

it("clears stale undo on an authoritative external replacement", () => {
  const initial = props();
  const rendered = render(<CodeEditor {...initial} />);
  const view = editor();
  act(() => view.dispatch({ changes: { from: 5, insert: " draft" } }));
  expect(undoDepth(view.state)).toBe(1);
  vi.mocked(initial.onChange).mockClear();
  rendered.rerender(<CodeEditor {...initial} value="external disk revision" />);
  expect(view.state.sliceDoc()).toBe("external disk revision");
  expect(undoDepth(view.state)).toBe(0);
  act(() => {
    expect(undo(view)).toBe(false);
  });
  expect(view.state.sliceDoc()).toBe("external disk revision");
  expect(initial.onChange).not.toHaveBeenCalled();
});

it("retains separate open-file histories but drops them across workspaces", () => {
  const initial = props();
  const rendered = render(<CodeEditor {...initial} />);
  const view = editor();
  act(() =>
    view.dispatch({
      changes: { from: 5, insert: "!" },
      selection: { anchor: 6 },
    }),
  );
  rendered.rerender(<CodeEditor {...initial} path="b.py" value="beta" />);
  expect(editor("b.py")).toBe(view);
  expect(undoDepth(view.state)).toBe(0);
  act(() =>
    view.dispatch({
      changes: { from: 4, insert: "?" },
      selection: { anchor: 5 },
    }),
  );
  rendered.rerender(<CodeEditor {...initial} value="alpha!" />);
  expect(view.state.sliceDoc()).toBe("alpha!");
  expect(view.state.selection.main.anchor).toBe(6);
  act(() => {
    expect(undo(view)).toBe(true);
  });
  expect(view.state.sliceDoc()).toBe("alpha");
  rendered.rerender(<CodeEditor {...initial} path="b.py" value="beta?" />);
  expect(view.state.selection.main.anchor).toBe(5);
  act(() => {
    expect(undo(view)).toBe(true);
  });
  expect(view.state.sliceDoc()).toBe("beta");
  rendered.rerender(
    <CodeEditor {...initial} workspace="/project-b" value="other project" />,
  );
  expect(view.state.sliceDoc()).toBe("other project");
  expect(undoDepth(view.state)).toBe(0);
  rendered.rerender(<CodeEditor {...initial} value="alpha!" />);
  expect(view.state.sliceDoc()).toBe("alpha!");
  expect(undoDepth(view.state)).toBe(0);
});

it("does not resurrect history after a path has been closed", () => {
  const initial = props();
  const rendered = render(<CodeEditor {...initial} />);
  const view = editor();
  act(() => view.dispatch({ changes: { from: 5, insert: "!" } }));
  rendered.rerender(
    <CodeEditor {...initial} path="b.py" value="beta" openPaths={["b.py"]} />,
  );
  rendered.rerender(<CodeEditor {...initial} value="alpha!" />);
  expect(view.state.sliceDoc()).toBe("alpha!");
  expect(undoDepth(view.state)).toBe(0);
});

it("retires the view during suspension but resumes an open file's selection and undo", () => {
  const initial = props({ openPaths: ["a.ts", "mixed.txt"] });
  const rendered = render(<CodeEditor {...initial} />);
  const firstView = editor();
  act(() =>
    firstView.dispatch({
      changes: { from: 5, insert: "!" },
      selection: { anchor: 6 },
      userEvent: "input.type",
    }),
  );
  const destroy = vi.spyOn(firstView, "destroy");
  vi.mocked(initial.onChange).mockClear();
  rendered.rerender(
    <CodeEditor
      {...initial}
      path="mixed.txt"
      value={"first\r\nsecond\nthird\r"}
      suspended
    />,
  );
  expect(destroy).toHaveBeenCalledTimes(1);
  expect(screen.queryByRole("textbox", { hidden: true })).toBeNull();
  expect(loadEditorLanguage).toHaveBeenCalledExactlyOnceWith("a.ts");
  expect(initial.onChange).not.toHaveBeenCalled();
  expect(initial.onSave).not.toHaveBeenCalled();
  rendered.rerender(<CodeEditor {...initial} value="alpha!" />);
  const resumed = editor();
  expect(resumed).not.toBe(firstView);
  expect(resumed.state.sliceDoc()).toBe("alpha!");
  expect(resumed.state.selection.main.anchor).toBe(6);
  expect(undoDepth(resumed.state)).toBe(1);
  act(() => {
    expect(undo(resumed)).toBe(true);
  });
  expect(resumed.state.sliceDoc()).toBe("alpha");
  expect(initial.onChange).toHaveBeenCalledExactlyOnceWith("alpha");
});

it("never creates a CodeMirror document or input for initially suspended mixed bytes", () => {
  const create = vi.spyOn(EditorState, "create");
  try {
    const initial = props({
      path: "mixed.txt",
      value: "first\r\nsecond\nthird\r",
      openPaths: ["mixed.txt"],
      suspended: true,
    });
    const rendered = render(<CodeEditor {...initial} />);
    rendered.rerender(
      <CodeEditor {...initial} value={"first\r\nchanged\nthird\r"} />,
    );
    expect(create).not.toHaveBeenCalled();
    expect(loadEditorLanguage).not.toHaveBeenCalled();
    expect(screen.queryByRole("textbox", { hidden: true })).toBeNull();
    expect(rendered.container.querySelector("[contenteditable]")).toBeNull();
    expect(initial.onChange).not.toHaveBeenCalled();
    expect(initial.onSave).not.toHaveBeenCalled();
  } finally {
    create.mockRestore();
  }
});

it.each(["closed", "workspace"] as const)(
  "prunes retained history when %s changes while suspended",
  (reason) => {
    const initial = props({ openPaths: ["a.ts", "mixed.txt"] });
    const rendered = render(<CodeEditor {...initial} />);
    const firstView = editor();
    act(() => firstView.dispatch({ changes: { from: 5, insert: "!" } }));
    expect(undoDepth(firstView.state)).toBe(1);
    rendered.rerender(
      <CodeEditor
        {...initial}
        path="mixed.txt"
        value={"first\r\nsecond\n"}
        suspended
      />,
    );
    rendered.rerender(
      <CodeEditor
        {...initial}
        path="mixed.txt"
        value={"first\r\nsecond\n"}
        suspended
        workspace={reason === "workspace" ? "/project-b" : initial.workspace}
        openPaths={reason === "closed" ? ["mixed.txt"] : initial.openPaths}
      />,
    );
    expect(screen.queryByRole("textbox", { hidden: true })).toBeNull();
    rendered.rerender(<CodeEditor {...initial} value="alpha!" />);
    const resumed = editor();
    expect(resumed.state.sliceDoc()).toBe("alpha!");
    expect(undoDepth(resumed.state)).toBe(0);
    act(() => {
      expect(undo(resumed)).toBe(false);
    });
  },
);

it("uses authoritative changed bytes after suspension without resurrecting old undo", () => {
  const initial = props({ openPaths: ["a.ts", "mixed.txt"] });
  const rendered = render(<CodeEditor {...initial} />);
  act(() => editor().dispatch({ changes: { from: 5, insert: "!" } }));
  rendered.rerender(
    <CodeEditor
      {...initial}
      path="mixed.txt"
      value={"first\r\nsecond\n"}
      suspended
    />,
  );
  vi.mocked(initial.onChange).mockClear();
  rendered.rerender(<CodeEditor {...initial} value="external disk revision" />);
  const resumed = editor();
  expect(resumed.state.sliceDoc()).toBe("external disk revision");
  expect(undoDepth(resumed.state)).toBe(0);
  expect(initial.onChange).not.toHaveBeenCalled();
});

it.each([
  ["resolve", "suspended"],
  ["reject", "suspended"],
  ["resolve", "resumed"],
  ["reject", "resumed"],
] as const)(
  "ignores old language %s while %s without contaminating the retained file",
  async (outcome, phase) => {
    const first = deferred();
    const second = deferred();
    const marker = Facet.define<string, string>({
      combine: (values) => values[0] ?? "none",
    });
    vi.mocked(loadEditorLanguage)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const initial = props({ openPaths: ["a.ts", "mixed.txt"] });
    const rendered = render(<CodeEditor {...initial} />);
    rendered.rerender(
      <CodeEditor
        {...initial}
        path="mixed.txt"
        value={"first\r\nsecond\n"}
        suspended
      />,
    );
    expect(loadEditorLanguage).toHaveBeenCalledExactlyOnceWith("a.ts");
    const settleOld = async () => {
      await act(async () => {
        if (outcome === "resolve") first.resolve(marker.of("old request"));
        else first.reject(new Error("old chunk unavailable"));
      });
    };
    if (phase === "suspended") {
      await settleOld();
      expect(screen.queryByRole("textbox", { hidden: true })).toBeNull();
      expect(screen.queryByRole("status", { hidden: true })).toBeNull();
    }
    rendered.rerender(<CodeEditor {...initial} />);
    const resumed = editor();
    expect(resumed.state.facet(marker)).toBe("none");
    await act(async () => second.resolve(marker.of("fresh request")));
    if (phase === "resumed") await settleOld();
    expect(loadEditorLanguage).toHaveBeenCalledTimes(2);
    expect(loadEditorLanguage).toHaveBeenNthCalledWith(2, "a.ts");
    expect(resumed.state.facet(marker)).toBe("fresh request");
    expect(resumed.state.sliceDoc()).toBe("alpha");
    expect(screen.queryByRole("status", { hidden: true })).toBeNull();
    expect(initial.onChange).not.toHaveBeenCalled();
    expect(initial.onSave).not.toHaveBeenCalled();
  },
);

it("saves the live document before an echo and honors latest disabled callbacks", () => {
  const initial = props();
  const rendered = render(<CodeEditor {...initial} />);
  const view = editor();
  act(() => view.dispatch({ changes: { from: 5, insert: "!" } }));
  // Parent value is deliberately still alpha; Save must serialize the view.
  save(view);
  expect(initial.onSave).toHaveBeenCalledExactlyOnceWith("alpha!");
  const latestSave = vi.fn();
  rendered.rerender(
    <CodeEditor {...initial} value="alpha!" disabled onSave={latestSave} />,
  );
  expect(view.state.readOnly).toBe(true);
  expect(view.contentDOM.getAttribute("contenteditable")).toBe("false");
  expect(view.contentDOM.getAttribute("aria-disabled")).toBe("true");
  vi.mocked(initial.onChange).mockClear();
  fireEvent.keyDown(view.contentDOM, { key: "Enter", code: "Enter" });
  fireEvent.keyDown(view.contentDOM, { key: "Tab", code: "Tab" });
  fireEvent.keyDown(view.contentDOM, {
    key: "z",
    code: "KeyZ",
    ctrlKey: true,
  });
  save(view);
  expect(view.state.sliceDoc()).toBe("alpha!");
  expect(initial.onChange).not.toHaveBeenCalled();
  expect(latestSave).not.toHaveBeenCalled();
  expect(initial.onSave).toHaveBeenCalledTimes(1);
  rendered.rerender(
    <CodeEditor
      {...initial}
      value="alpha!"
      disabled={false}
      onSave={latestSave}
    />,
  );
  save(view);
  expect(latestSave).toHaveBeenCalledExactlyOnceWith("alpha!");
});

it.each(["resolve", "reject"] as const)(
  "ignores a stale language %s after switching documents",
  async (outcome) => {
    const first = deferred();
    const second = deferred();
    const marker = Facet.define<string, string>({
      combine: (values) => values[0] ?? "none",
    });
    vi.mocked(loadEditorLanguage)
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const initial = props();
    const rendered = render(<CodeEditor {...initial} />);
    rendered.rerender(<CodeEditor {...initial} path="b.py" value="beta" />);
    const view = editor("b.py");
    await act(async () => second.resolve(marker.of("python")));
    expect(view.state.facet(marker)).toBe("python");
    await act(async () => {
      if (outcome === "resolve") first.resolve(marker.of("typescript"));
      else first.reject(new Error("old chunk unavailable"));
    });
    expect(view.state.facet(marker)).toBe("python");
    expect(view.state.sliceDoc()).toBe("beta");
    expect(screen.queryByRole("status")).toBeNull();
    expect(initial.onChange).not.toHaveBeenCalled();
  },
);

it("destroys the view on unmount and ignores a pending language failure", async () => {
  const language = deferred();
  vi.mocked(loadEditorLanguage).mockReturnValueOnce(language.promise);
  const initial = props();
  const rendered = render(<CodeEditor {...initial} />);
  const view = editor();
  const destroy = vi.spyOn(view, "destroy");
  rendered.unmount();
  expect(destroy).toHaveBeenCalledTimes(1);
  await act(async () => language.reject(new Error("late chunk unavailable")));
  expect(screen.queryByRole("textbox")).toBeNull();
  expect(initial.onChange).not.toHaveBeenCalled();
  expect(initial.onSave).not.toHaveBeenCalled();
});

it.each(["\n", "\r\n", "\r"])(
  "preserves homogeneous %j separators through Enter, Save and Undo",
  (separator) => {
    const value = `const café = "🧪";${separator}  second${separator}`;
    const initial = props({ value });
    render(<CodeEditor {...initial} />);
    const view = editor();
    expect(view.state.lineBreak).toBe(separator);
    expect(view.state.sliceDoc()).toBe(value);
    act(() => {
      view.dispatch({ selection: { anchor: view.state.doc.line(1).to } });
      expect(insertNewline(view)).toBe(true);
    });
    const edited = `const café = "🧪";${separator}${separator}  second${separator}`;
    expect(initial.onChange).toHaveBeenLastCalledWith(edited);
    save(view);
    expect(initial.onSave).toHaveBeenCalledExactlyOnceWith(edited);
    act(() => {
      expect(undo(view)).toBe(true);
    });
    expect(view.state.sliceDoc()).toBe(value);
    expect(initial.onChange).toHaveBeenLastCalledWith(value);
  },
);
