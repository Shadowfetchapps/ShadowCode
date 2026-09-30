import { useCallback, type RefObject } from "react";
import { api, type StartJobRequest } from "../api";
import type { ChatItem } from "../components/cards";
import type { Attachment } from "../lib/attachments";
import type { ToastKind } from "./useToasts";

type UserItem = Extract<ChatItem, { kind: "user" }>;

/** Actions on messages in the conversation: Edit & resend (a new
 * conversation forked just before the message, optionally undoing the file
 * changes made from that message on), Retry and Copy. */
export function useMessageActions({
  items,
  sessionId,
  workspace,
  model,
  roles,
  busy,
  queueing,
  submittingRef,
  openSession,
  startTask,
  rewindNow,
  toast,
}: {
  items: ChatItem[];
  sessionId: string;
  workspace: string;
  /** The composer's model (picker id). */
  model: string;
  /** Plan → Implement → Review is on: resent messages run as roles too. */
  roles?: boolean;
  busy: boolean;
  queueing: boolean;
  /** A request is being sent: a second Retry would send it twice. */
  submittingRef: RefObject<boolean>;
  openSession: (id: string) => Promise<void>;
  startTask: (
    body: StartJobRequest,
    original: { task: string; attachments: Attachment[] } | null,
  ) => Promise<void>;
  rewindNow: (
    taskId: string,
    options?: { quiet?: boolean },
  ) => Promise<string[] | null>;
  toast: (text: string, kind?: ToastKind) => void;
}) {
  const request = useCallback(
    (task: string, session: string, queue: boolean): StartJobRequest => ({
      task,
      workspace: workspace || undefined,
      session_id: session || undefined,
      model,
      purpose: "coder",
      queue,
      images: [],
      web: false,
      ...(roles ? { roles: true } : {}),
    }),
    [workspace, model, roles],
  );

  const editResend = useCallback(
    async (item: UserItem, text: string, undoFiles: boolean) => {
      if (busy) {
        toast(
          "Stop the running task before editing an earlier message.",
          "info",
        );
        return;
      }
      if (!model) {
        toast("Choose a model to send.", "err");
        return;
      }
      if (!item.eventId || !sessionId) {
        toast("This message can't be edited: it has no saved position.", "err");
        return;
      }
      if (undoFiles) {
        // This task and every later one, newest first, so each rewind finds
        // the files as its task left them.
        const start = items.findIndex(
          (other) => other === item || other.key === item.key,
        );
        const tasks: string[] = [];
        for (const other of items.slice(Math.max(0, start)))
          if (other.taskId && !tasks.includes(other.taskId))
            tasks.push(other.taskId);
        let restored = 0;
        for (const task of tasks.reverse()) {
          const detail = await api.taskCheckpoint(task).catch(() => null);
          if (!detail?.rewindable) continue;
          const paths = await rewindNow(task, { quiet: true });
          if (paths === null) return;
          restored += paths.length;
        }
        if (restored)
          toast(
            `Undid file changes from this message on (${restored} file${restored === 1 ? "" : "s"}).`,
            "ok",
          );
      }
      let fork;
      try {
        fork = await api.forkSession(sessionId, item.eventId, undefined, true);
      } catch (e) {
        toast(String(e), "err");
        return;
      }
      await openSession(fork.fork.id);
      await startTask(request(text, fork.fork.id, false), null);
      toast(
        "Edited message sent in a new conversation. The original conversation is kept.",
        "ok",
      );
    },
    [
      busy,
      model,
      sessionId,
      items,
      rewindNow,
      openSession,
      startTask,
      request,
      toast,
    ],
  );

  const retry = useCallback(
    async (text: string) => {
      if (submittingRef.current) return;
      if (!model) {
        toast("Choose a model to send.", "err");
        return;
      }
      await startTask(request(text, sessionId, queueing), null);
    },
    [model, sessionId, queueing, submittingRef, startTask, request, toast],
  );

  const copy = useCallback(
    async (text: string) => {
      try {
        await navigator.clipboard.writeText(text);
        toast("Copied", "ok");
      } catch {
        toast("Could not copy to the clipboard.", "err");
      }
    },
    [toast],
  );

  return { editResend, retry, copy };
}
