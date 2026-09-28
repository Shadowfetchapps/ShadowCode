import { afterEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, renderHook } from "@testing-library/react";
import {
  shortcutFor,
  useShortcuts,
  type ShortcutContext,
} from "./useShortcuts";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const closed: ShortcutContext = {
  consent: false,
  overlay: false,
  trust: false,
  picker: false,
  panel: false,
  onboarding: false,
};
const key = (
  k: string,
  extra: Partial<KeyboardEvent> = {},
): Pick<KeyboardEvent, "key" | "ctrlKey" | "metaKey" | "shiftKey" | "target"> =>
  ({
    key: k,
    ctrlKey: false,
    metaKey: false,
    shiftKey: false,
    target: document.body,
    ...extra,
  }) as KeyboardEvent;

it("maps the documented shortcuts", () => {
  const ctrl = { ctrlKey: true };
  expect(shortcutFor(key("k", ctrl), closed)).toBe("palette");
  expect(shortcutFor(key("K", { metaKey: true }), closed)).toBe("palette");
  expect(shortcutFor(key("b", ctrl), closed)).toBe("sidebar");
  expect(shortcutFor(key("B", { ...ctrl, shiftKey: true }), closed)).toBe(
    "changes",
  );
  expect(shortcutFor(key(",", ctrl), closed)).toBe("settings");
  expect(shortcutFor(key("p", ctrl), closed)).toBe("project");
  expect(shortcutFor(key("n", ctrl), closed)).toBe("new");
  expect(shortcutFor(key("m", ctrl), closed)).toBe("model");
  expect(shortcutFor(key("l", ctrl), closed)).toBe("focus");
  expect(shortcutFor(key(".", ctrl), closed)).toBe("stop");
  expect(shortcutFor(key("E", { ...ctrl, shiftKey: true }), closed)).toBe(
    "export",
  );
  expect(shortcutFor(key("?"), closed)).toBe("help");
  expect(shortcutFor(key("k"), closed)).toBeNull();
});

it("does not open help while typing", () => {
  const input = document.createElement("textarea");
  expect(shortcutFor(key("?", { target: input }), closed)).toBeNull();
});

it.each(["", "true", "plaintext-only"])(
  "keeps help typing inside contenteditable=%s and its descendants",
  (value) => {
    const editor = document.createElement("div");
    editor.setAttribute("contenteditable", value);
    const token = document.createElement("span");
    editor.append(token);
    expect(shortcutFor(key("?", { target: editor }), closed)).toBeNull();
    expect(shortcutFor(key("?", { target: token }), closed)).toBeNull();
    // This only changes typing detection, not documented global shortcuts.
    expect(
      shortcutFor(key("k", { target: token, ctrlKey: true }), closed),
    ).toBe("palette");
    token.setAttribute("contenteditable", "false");
    expect(shortcutFor(key("?", { target: token }), closed)).toBe("help");
  },
);

it("lets question marks bubble from editing fields without opening global help", () => {
  const run = vi.fn();
  renderHook(() => useShortcuts(closed, run));
  const fields = document.createElement("section");
  fields.innerHTML =
    '<textarea></textarea><div contenteditable="true"><span><em>code</em></span></div>';
  document.body.append(fields);
  try {
    fireEvent.keyDown(fields.querySelector("textarea")!, { key: "?" });
    fireEvent.keyDown(fields.querySelector("div")!, { key: "?" });
    fireEvent.keyDown(fields.querySelector("em")!, { key: "?" });
    expect(run).not.toHaveBeenCalled();
    fireEvent.keyDown(document.body, { key: "?" });
    expect(run).toHaveBeenCalledOnce();
    expect(run).toHaveBeenCalledWith("help");
  } finally {
    fields.remove();
  }
});

it("does not invoke app actions for a key already consumed by its focused owner", () => {
  const run = vi.fn();
  renderHook(() => useShortcuts({ ...closed, panel: true }, run));
  const editor = document.createElement("div");
  editor.setAttribute("contenteditable", "true");
  document.body.append(editor);
  const consume = (event: Event) => event.preventDefault();
  editor.addEventListener("keydown", consume);
  try {
    for (const init of [{ key: "k", ctrlKey: true }, { key: "Escape" }]) {
      const event = new KeyboardEvent("keydown", {
        ...init,
        bubbles: true,
        cancelable: true,
      });
      editor.dispatchEvent(event);
      expect(event.defaultPrevented).toBe(true);
    }
    expect(run).not.toHaveBeenCalled();
    editor.removeEventListener("keydown", consume);
    fireEvent.keyDown(editor, { key: "k", ctrlKey: true });
    expect(run).toHaveBeenCalledOnce();
    expect(run).toHaveBeenCalledWith("palette");
  } finally {
    editor.removeEventListener("keydown", consume);
    editor.remove();
  }
});

it("Escape closes the topmost layer; dialogs own the keyboard", () => {
  const esc = key("Escape");
  expect(shortcutFor(esc, { ...closed, overlay: true, panel: true })).toBe(
    "close-overlay",
  );
  expect(shortcutFor(esc, { ...closed, trust: true, picker: true })).toBe(
    "close-trust",
  );
  expect(shortcutFor(esc, { ...closed, picker: true, panel: true })).toBe(
    "close-picker",
  );
  expect(shortcutFor(esc, { ...closed, panel: true })).toBe("close-panel");
  expect(shortcutFor(esc, { ...closed, consent: true, overlay: true })).toBe(
    null,
  );
  expect(shortcutFor(esc, closed)).toBeNull();
  for (const open of ["overlay", "trust", "consent", "onboarding"] as const)
    expect(
      shortcutFor(key("k", { ctrlKey: true }), { ...closed, [open]: true }),
    ).toBeNull();
});

it("registers one listener for the app's lifetime and reads the latest state", () => {
  const add = vi.spyOn(window, "addEventListener");
  const run = vi.fn();
  const { rerender, unmount } = renderHook(
    ({ open, handler }) => useShortcuts(open, handler),
    { initialProps: { open: closed, handler: run } },
  );
  const next = vi.fn();
  for (let i = 0; i < 5; i++)
    rerender({ open: { ...closed, panel: i % 2 === 0 }, handler: next });
  rerender({ open: { ...closed, overlay: true }, handler: next });
  expect(add.mock.calls.filter(([type]) => type === "keydown")).toHaveLength(1);
  fireEvent.keyDown(window, { key: "Escape" });
  expect(run).not.toHaveBeenCalled();
  expect(next).toHaveBeenCalledWith("close-overlay");
  rerender({ open: closed, handler: next });
  const event = new KeyboardEvent("keydown", {
    key: "k",
    ctrlKey: true,
    cancelable: true,
  });
  window.dispatchEvent(event);
  expect(next).toHaveBeenLastCalledWith("palette");
  expect(event.defaultPrevented).toBe(true);
  const remove = vi.spyOn(window, "removeEventListener");
  unmount();
  expect(remove.mock.calls.filter(([type]) => type === "keydown")).toHaveLength(
    1,
  );
});

it("moves between conversations with Alt+↑/↓ and Ctrl+Tab", () => {
  const alt = { altKey: true } as Partial<KeyboardEvent>;
  expect(shortcutFor(key("ArrowUp", alt), closed)).toBe(
    "previous-conversation",
  );
  expect(shortcutFor(key("ArrowDown", alt), closed)).toBe("next-conversation");
  // Plain arrows stay with the page and the composer.
  expect(shortcutFor(key("ArrowDown"), closed)).toBeNull();
  expect(shortcutFor(key("Tab", { ctrlKey: true }), closed)).toBe(
    "recent-conversation",
  );
  expect(shortcutFor(key("Tab"), closed)).toBeNull();
  // Dialogs keep the keyboard.
  expect(
    shortcutFor(key("ArrowUp", alt), { ...closed, overlay: true }),
  ).toBeNull();
});

it("keys inside any modal dialog are the dialog's, even one the app does not track", () => {
  const modal = document.createElement("div");
  modal.setAttribute("role", "dialog");
  modal.setAttribute("aria-modal", "true");
  const button = document.createElement("button");
  modal.append(button);
  document.body.append(modal);
  try {
    for (const k of [
      key("n", { ctrlKey: true, target: button }),
      key("k", { ctrlKey: true, target: button }),
      key("Escape", { target: button }),
    ])
      expect(shortcutFor(k, { ...closed, panel: true })).toBeNull();
    // Outside the dialog the same keys still work.
    expect(shortcutFor(key("n", { ctrlKey: true }), closed)).toBe("new");
  } finally {
    modal.remove();
  }
});
