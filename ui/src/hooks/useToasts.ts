import { useCallback, useRef, useState } from "react";
import { readableError } from "../lib/transport";

export type ToastKind = "ok" | "err" | "info";
/** A button on the notification (Undo after a rewind). */
export type ToastAction = { label: string; run: () => void };
export type Toast = {
  id: number;
  text: string;
  kind: ToastKind;
  action?: ToastAction;
};

/** Up to four notifications; errors stay 8 s, ones with an action 12 s, the
 * rest 5 s. Callers often pass `String(error)`; the text is shown without a
 * JavaScript class prefix ("Error: …"). */
export function useToasts() {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const seq = useRef(0);
  const toast = useCallback(
    (text: string, kind: ToastKind = "info", action?: ToastAction) => {
      const id = ++seq.current;
      const shown = readableError(text) || text;
      setToasts((prev) => [
        ...prev.slice(-3),
        { id, text: shown, kind, action },
      ]);
      const duration = action ? 12000 : kind === "err" ? 8000 : 5000;
      setTimeout(
        () => setToasts((prev) => prev.filter((t) => t.id !== id)),
        duration,
      );
    },
    [],
  );
  const dismiss = useCallback(
    (id: number) => setToasts((prev) => prev.filter((t) => t.id !== id)),
    [],
  );
  return { toasts, toast, dismiss };
}
