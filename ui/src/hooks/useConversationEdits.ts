import type { RefObject } from "react";
import { api, type Job } from "../api";
import type { PickerTarget } from "../lib/picker";
import type { useConversation } from "./useConversation";
import { useMessageActions } from "./useMessageActions";
import { useReviewTarget } from "./useReview";
import { useRewind } from "./useRewind";
import { useStableCallback } from "./useStableCallback";
import type { useTaskActions } from "./useTaskActions";
import type { ToastAction, ToastKind } from "./useToasts";

/** Changing what already happened in a conversation: the per-task Review,
 * rewinds (with confirmation and Undo) and Edit & resend / Retry / Copy. */
export function useConversationEdits({
  conversation,
  jobRef,
  selectedRef,
  submittingRef,
  sessionId,
  workspace,
  target,
  roles,
  busy,
  queueing,
  openSession,
  startTask,
  refresh,
  toast,
}: {
  conversation: ReturnType<typeof useConversation>;
  jobRef: RefObject<Job | null>;
  selectedRef: RefObject<string>;
  submittingRef: RefObject<boolean>;
  sessionId: string;
  workspace: string;
  target: PickerTarget | undefined;
  /** Resent messages run as Plan → Implement → Review. */
  roles?: boolean;
  /** A task runs or is being sent. */
  busy: boolean;
  queueing: boolean;
  openSession: (id: string) => Promise<void>;
  startTask: ReturnType<typeof useTaskActions>["startTask"];
  refresh: () => Promise<void>;
  toast: (text: string, kind?: ToastKind, action?: ToastAction) => void;
}) {
  const review = useReviewTarget(sessionId);
  /** After a rewind or a review undo while no task streams: the project
   * state and the conversation's new rows (the divider, the notes). */
  const refreshAfterFiles = useStableCallback(async () => {
    await refresh().catch(() => undefined);
    const sid = selectedRef.current;
    if (!sid || busy) return;
    const detail = await api.session(sid).catch(() => null);
    if (detail && selectedRef.current === sid && !submittingRef.current)
      conversation.load(detail, jobRef.current, true);
  });
  const rewinding = useRewind({ busy, refresh: refreshAfterFiles, toast });
  const messages = useMessageActions({
    items: conversation.transcript.items,
    sessionId,
    workspace,
    model: target?.id || "",
    roles,
    busy,
    queueing,
    openSession,
    startTask,
    rewindNow: rewinding.rewindNow,
    toast,
  });
  return { review, rewinding, messages, refreshAfterFiles };
}
