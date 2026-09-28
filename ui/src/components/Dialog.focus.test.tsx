import { StrictMode, useState } from "react";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { ConfirmDialog } from "./ConfirmDialog";
import { Dialog } from "./Dialog";
import { Palette } from "./overlays";
import { useShortcuts } from "../hooks/useShortcuts";

afterEach(cleanup);

function Harness() {
  const [open, setOpen] = useState(false);
  return (
    <div>
      <button type="button" onClick={() => setOpen(true)}>
        Open settings
      </button>
      {open ? (
        <Dialog label="Settings" onClose={() => setOpen(false)}>
          <button type="button" onClick={() => setOpen(false)}>
            Close
          </button>
        </Dialog>
      ) : null}
    </div>
  );
}

it("restores keyboard focus to the opener when the dialog unmounts", () => {
  render(<Harness />);
  const opener = screen.getByRole("button", { name: "Open settings" });
  opener.focus();
  fireEvent.click(opener);
  expect(screen.getByRole("dialog", { name: "Settings" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(screen.queryByRole("dialog", { name: "Settings" })).toBeNull();
  expect(document.activeElement).toBe(opener);
});

function PaletteHarness() {
  const [open, setOpen] = useState(false);
  useShortcuts(
    {
      consent: false,
      overlay: open,
      trust: false,
      picker: false,
      panel: false,
      onboarding: false,
    },
    (action) => {
      if (action === "palette") setOpen(true);
      if (action === "close-overlay") setOpen(false);
    },
  );
  return (
    <>
      <textarea aria-label="Message ShadowCode" />
      <button type="button" onClick={() => setOpen(true)}>
        Open commands
      </button>
      {open && <Palette items={[]} onClose={() => setOpen(false)} />}
    </>
  );
}

it("returns focus to the composer after the real autofocus palette closes with Escape", () => {
  render(<PaletteHarness />);
  const composer = screen.getByRole("textbox", { name: "Message ShadowCode" });
  composer.focus();
  fireEvent.keyDown(composer, { key: "k", ctrlKey: true });
  const search = screen.getByRole("textbox", { name: "Search commands" });
  expect(document.activeElement).toBe(search);
  fireEvent.keyDown(search, { key: "Escape" });
  expect(screen.queryByRole("dialog", { name: "Command palette" })).toBeNull();
  expect(document.activeElement).toBe(composer);
});

it("restores the actual palette opener across repeated StrictMode mounts", () => {
  render(
    <StrictMode>
      <PaletteHarness />
    </StrictMode>,
  );
  const composer = screen.getByRole("textbox", { name: "Message ShadowCode" });
  const opener = screen.getByRole("button", { name: "Open commands" });
  for (const target of [opener, composer, opener]) {
    target.focus();
    if (target === opener) fireEvent.click(opener);
    else fireEvent.keyDown(target, { key: "k", ctrlKey: true });
    const search = screen.getByRole("textbox", { name: "Search commands" });
    expect(document.activeElement).toBe(search);
    fireEvent.keyDown(search, { key: "Escape" });
    expect(
      screen.queryByRole("dialog", { name: "Command palette" }),
    ).toBeNull();
    expect(document.activeElement).toBe(target);
  }
});

function ConfirmHarness({ onShortcut }: { onShortcut: () => void }) {
  const [open, setOpen] = useState(true);
  // The window's shortcuts do not know about this dialog (like Rewind or a
  // drawer confirmation): nothing marks it as an overlay.
  useShortcuts(
    {
      consent: false,
      overlay: false,
      trust: false,
      picker: false,
      panel: true,
      onboarding: false,
    },
    onShortcut,
  );
  return open ? (
    <ConfirmDialog
      title="Rewind this task's changes?"
      confirmLabel="Rewind"
      onConfirm={() => setOpen(false)}
      onCancel={() => setOpen(false)}
    >
      <input
        aria-label="Inner menu"
        onKeyDown={(e) => {
          // An inner control that handles Escape itself keeps the dialog.
          if (e.key === "Escape" && e.currentTarget.value) {
            e.stopPropagation();
            e.currentTarget.value = "";
          }
        }}
      />
    </ConfirmDialog>
  ) : null;
}

it("Escape closes any dialog, after inner controls, and shortcuts stay behind it", () => {
  const shortcut = vi.fn();
  render(<ConfirmHarness onShortcut={shortcut} />);
  const dialog = screen.getByRole("dialog", {
    name: "Rewind this task's changes?",
  });
  const inner = screen.getByRole("textbox", { name: "Inner menu" });
  inner.focus();
  // Window shortcuts do not act on the window behind a modal dialog.
  fireEvent.keyDown(inner, { key: "n", ctrlKey: true });
  fireEvent.keyDown(inner, { key: "k", ctrlKey: true });
  expect(shortcut).not.toHaveBeenCalled();
  (inner as HTMLInputElement).value = "open";
  fireEvent.keyDown(inner, { key: "Escape" });
  expect(dialog.isConnected).toBe(true);
  fireEvent.keyDown(inner, { key: "Escape" });
  expect(
    screen.queryByRole("dialog", { name: "Rewind this task's changes?" }),
  ).toBeNull();
  // Escape did not also close the drawer behind the dialog.
  expect(shortcut).not.toHaveBeenCalled();
});
