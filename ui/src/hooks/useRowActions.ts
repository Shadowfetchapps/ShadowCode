import { useMemo, useRef, type SetStateAction } from "react";
import { api, type CheckJobRequest } from "../api";
import type { ChatItem } from "../components/cards";
import type { RowActions } from "../components/shell/TranscriptRows";
import type { Fallback } from "../lib/allowance";
import { batchDiffStats } from "../lib/diffStats";
import type { Transcript } from "../lib/transcript";
import type { RunCheckAction } from "../components/RunCheck";

type LimitItem = Extract<ChatItem, { kind: "limit" }>;
export type RowHandlers = {
  runCheck?: RunCheckAction;
  setTranscript: (update: SetStateAction<Transcript>) => void;
  reviewChanges: (path?: string, taskId?: string) => void;
  rewind: (taskId: string) => Promise<void>;
  editResend: (
    item: Extract<ChatItem, { kind: "user" }>,
    text: string,
    undoFiles: boolean,
  ) => Promise<void>;
  retry: (text: string) => Promise<void>;
  copy: (text: string) => Promise<void>;
  continueOnFallback: (item: LimitItem, choice: Fallback) => Promise<void>;
  chooseModel: () => void;
  openLocal: () => void;
  fork: (eventId: number) => Promise<void>;
  openSession: (sessionId: string) => Promise<void>;
};

/** Transcript row callbacks with stable identities (they call the latest
 * handlers), so memoized rows skip the window's re-renders. Changed-file
 * line counts from every task summary on screen go out in one request. */
export function useRowActions(handlers: RowHandlers): RowActions {
  const latest = useRef(handlers);
  latest.current = handlers;
  return useMemo(
    () => ({
      runCheck: handlers.runCheck
        ? {
            workspace: handlers.runCheck.workspace,
            sessionId: handlers.runCheck.sessionId,
            disabled: handlers.runCheck.disabled,
            onRun: (request: CheckJobRequest) => {
              const action = latest.current.runCheck;
              if (!action)
                return Promise.reject(
                  new Error("Check execution is unavailable."),
                );
              return action.onRun(request);
            },
          }
        : undefined,
      onToggleTool: (key: string) =>
        latest.current.setTranscript((s) => ({
          ...s,
          items: s.items.map((it) =>
            it.key === key && it.kind === "tool"
              ? { ...it, collapsed: it.collapsed === false }
              : it,
          ),
        })),
      diffStats: batchDiffStats((paths) =>
        api.diffStats(paths).then((r) => r.stats),
      ),
      onReview: (path?: string, taskId?: string) =>
        latest.current.reviewChanges(path, taskId),
      onRewind: (taskId: string) => void latest.current.rewind(taskId),
      onContinue: (item: LimitItem, choice: Fallback) =>
        void latest.current.continueOnFallback(item, choice),
      onChooseModel: () => latest.current.chooseModel(),
      onOpenLocal: () => latest.current.openLocal(),
      onFork: (eventId: number) => void latest.current.fork(eventId),
      onOpenSession: (sessionId: string) =>
        void latest.current.openSession(sessionId),
      onEditResend: (
        item: Extract<ChatItem, { kind: "user" }>,
        text: string,
        undoFiles: boolean,
      ) => void latest.current.editResend(item, text, undoFiles),
      onRetry: (text: string) => void latest.current.retry(text),
      onCopy: (text: string) => void latest.current.copy(text),
    }),
    [
      handlers.runCheck?.workspace,
      handlers.runCheck?.sessionId,
      handlers.runCheck?.disabled,
    ],
  );
}
