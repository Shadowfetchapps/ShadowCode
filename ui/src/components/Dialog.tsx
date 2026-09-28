import { useEffect, useRef, useState, type ReactNode } from "react";

/** Modal focus containment and restoration shared by settings and pickers.
 * Escape closes the dialog (after any control inside it, such as an open
 * menu, has handled it), whether or not the app tracks it as an overlay. */
export function Dialog({
  children,
  onClose,
  label,
  className = "modal modal-sm",
}: {
  children: ReactNode;
  onClose: () => void;
  label: string;
  className?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  // Child autoFocus runs during commit, before this effect. Retain the
  // opener during the initial render so cleanup never targets a removed
  // input inside the dialog instead of the control that opened it.
  const [previous] = useState<HTMLElement | null>(() =>
    typeof document === "undefined"
      ? null
      : (document.activeElement as HTMLElement | null),
  );
  useEffect(() => {
    const dialog = ref.current;
    if (!dialog) return;
    const focusables = () =>
      Array.from(
        dialog.querySelectorAll<HTMLElement>(
          'button:not(:disabled),input:not(:disabled),select:not(:disabled),textarea:not(:disabled),a[href],[tabindex="0"]',
        ),
      ).filter((el) => el.offsetParent !== null);
    if (!dialog.contains(document.activeElement))
      (focusables()[0] || dialog).focus();
    // Task activation can finish while a dialog is already open. Keep its
    // delayed composer focus from stealing subsequent keyboard commands.
    const containFocus = (event: FocusEvent) => {
      if (dialog.isConnected && !dialog.contains(event.target as Node))
        (focusables()[0] || dialog).focus();
    };
    const trap = (event: KeyboardEvent) => {
      if (event.key !== "Tab") return;
      const targets = focusables();
      const first = targets[0];
      const last = targets.at(-1);
      if (!first) {
        event.preventDefault();
        dialog.focus();
      } else if (
        event.shiftKey &&
        (document.activeElement === first || document.activeElement === dialog)
      ) {
        event.preventDefault();
        last?.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    dialog.addEventListener("keydown", trap);
    document.addEventListener("focusin", containFocus);
    return () => {
      dialog.removeEventListener("keydown", trap);
      document.removeEventListener("focusin", containFocus);
      if (previous?.isConnected) previous.focus();
    };
  }, []);
  return (
    <div
      className="modal-back"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose();
      }}
    >
      <div
        ref={ref}
        className={className}
        role="dialog"
        aria-modal="true"
        aria-label={label}
        tabIndex={-1}
        onKeyDown={(event) => {
          if (event.key !== "Escape" || event.defaultPrevented) return;
          event.preventDefault();
          event.stopPropagation();
          onClose();
        }}
      >
        {children}
      </div>
    </div>
  );
}
