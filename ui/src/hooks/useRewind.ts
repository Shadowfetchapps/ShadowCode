import { useCallback, useState } from "react";
import { api } from "../api";
import type { ToastAction, ToastKind } from "./useToasts";

export type RewindAsk = {
  taskId: string;
  /** The files that will go back to how they were before the task. */
  paths: string[];
};

/** Rewind with a confirmation that lists the files, then a notification
 * whose Undo puts the files back as they were just before the rewind (the
 * engine keeps them under an undo id). */
export function useRewind({
  busy,
  refresh,
  toast,
}: {
  busy: boolean;
  refresh: () => Promise<void>;
  toast: (text: string, kind?: ToastKind, action?: ToastAction) => void;
}) {
  const [asking, setAsking] = useState<RewindAsk | null>(null);

  const undo = useCallback(
    async (undoId: string) => {
      try {
        const result = await api.undoRewind(undoId);
        toast(
          `Rewind undone. ${result.restored.length} file${result.restored.length === 1 ? "" : "s"} put back.`,
          "ok",
        );
      } catch (e) {
        toast(String(e), "err");
      }
      await refresh().catch(() => undefined);
    },
    [refresh, toast],
  );

  /** Rewind without asking (the caller already confirmed). Answers the
   * restored paths, or null when it failed (the error was shown). */
  const rewindNow = useCallback(
    async (taskId: string, { quiet = false } = {}) => {
      try {
        const result = await api.rewindTask(taskId);
        const count = result.restored.length;
        if (!quiet)
          toast(
            `Restored ${count} file${count === 1 ? "" : "s"}.`,
            "ok",
            result.undo_id
              ? {
                  label: "Undo",
                  run: () => void undo(result.undo_id as string),
                }
              : undefined,
          );
        await refresh().catch(() => undefined);
        return result.restored;
      } catch (e) {
        toast(String(e), "err");
        return null;
      }
    },
    [refresh, toast, undo],
  );

  /** Show the confirmation for a task's rewind. */
  const ask = useCallback(
    async (taskId: string) => {
      if (busy) {
        toast("Stop the task before rewinding its files.", "info");
        return;
      }
      try {
        const detail = await api.taskCheckpoint(taskId);
        const paths = detail.checkpoint?.paths || [];
        if (!detail.rewindable || !paths.length) {
          toast("This task has no file changes left to rewind.", "info");
          return;
        }
        setAsking({ taskId, paths });
      } catch (e) {
        toast(String(e), "err");
      }
    },
    [busy, toast],
  );

  const confirm = useCallback(async () => {
    if (!asking) return;
    await rewindNow(asking.taskId);
    setAsking(null);
  }, [asking, rewindNow]);

  return {
    asking,
    ask,
    confirm,
    cancel: () => setAsking(null),
    rewindNow,
    undo,
  };
}
