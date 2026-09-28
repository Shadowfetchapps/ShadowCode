import { useEffect, useRef } from "react";

/** What is open, in the order Escape closes it. */
export type ShortcutContext = {
  consent: boolean;
  overlay: boolean;
  trust: boolean;
  picker: boolean;
  panel: boolean;
  /** First-run onboarding owns the keyboard. */
  onboarding: boolean;
};

export type ShortcutAction =
  | "close-overlay"
  | "close-trust"
  | "close-picker"
  | "close-panel"
  | "palette"
  | "sidebar"
  | "changes"
  | "terminal"
  | "settings"
  | "project"
  | "new"
  | "model"
  | "focus"
  | "stop"
  | "export"
  | "help"
  | "previous-conversation"
  | "next-conversation"
  | "recent-conversation";

/** The action a key press asks for, or null. Pure, for tests. */
export function shortcutFor(
  e: Pick<
    KeyboardEvent,
    "key" | "ctrlKey" | "metaKey" | "shiftKey" | "target"
  > & { altKey?: boolean },
  open: ShortcutContext,
): ShortcutAction | null {
  const key = e.key.toLowerCase();
  const mod = e.ctrlKey || e.metaKey;
  const target = e.target instanceof Element ? e.target : null;
  const editable = target?.closest("[contenteditable]");
  const inField =
    /^(INPUT|TEXTAREA|SELECT)$/.test(target?.tagName || "") ||
    Boolean((target as HTMLElement | null)?.isContentEditable) ||
    ["", "true", "plaintext-only"].includes(
      editable?.getAttribute("contenteditable")?.toLowerCase() ?? "false",
    );
  // A terminal owns the keyboard (Escape, Ctrl+L, Ctrl+P, …) except the
  // key that toggles it.
  const inTerminal = Boolean(
    (e.target as HTMLElement | null)?.closest?.("[data-terminal]"),
  );
  const toggleTerminal = mod && (e.key === "`" || e.key === "Dead");
  if (inTerminal) return toggleTerminal && !open.overlay ? "terminal" : null;
  if (e.key === "Escape") {
    if (open.consent) return null;
    if (open.overlay) return "close-overlay";
    if (open.trust) return "close-trust";
    if (open.picker) return "close-picker";
    if (open.panel) return "close-panel";
    return null;
  }
  // Dialogs own the keyboard while they are open.
  if (open.overlay || open.trust || open.consent || open.onboarding)
    return null;
  if (mod && key === "k") return "palette";
  if (mod && key === "b" && !e.shiftKey) return "sidebar";
  if (mod && e.shiftKey && key === "b") return "changes";
  if (toggleTerminal) return "terminal";
  if (mod && key === ",") return "settings";
  if (mod && key === "p") return "project";
  if (mod && key === "n") return "new";
  if (mod && key === "m") return "model";
  if (mod && key === "l") return "focus";
  if (mod && key === ".") return "stop";
  if (mod && e.shiftKey && key === "e") return "export";
  // Conversations: Alt+↑/↓ previous/next in the sidebar, Ctrl+Tab the one
  // opened before this one.
  if (e.altKey && !mod && key === "arrowup") return "previous-conversation";
  if (e.altKey && !mod && key === "arrowdown") return "next-conversation";
  if (e.ctrlKey && !e.shiftKey && e.key === "Tab") return "recent-conversation";
  if (key === "?" && !inField) return "help";
  return null;
}

/** One window keydown listener for the app's lifetime. The latest open state
 * and handler are read through refs, so re-renders never re-register it. */
export function useShortcuts(
  open: ShortcutContext,
  run: (action: ShortcutAction) => void,
) {
  const state = useRef(open);
  state.current = open;
  const handler = useRef(run);
  handler.current = run;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      // Editors and other focused controls get first ownership of handled keys.
      if (e.defaultPrevented) return;
      const action = shortcutFor(e, state.current);
      if (!action) return;
      if (!action.startsWith("close-")) e.preventDefault();
      handler.current(action);
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
}
