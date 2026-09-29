import { useMemo, useRef } from "react";
import type { ConversationOpinions } from "../components/TranscriptOpinions";
import type { PickerTarget } from "../lib/picker";
import { continuePrompt, type SecondOpinion } from "../lib/secondOpinion";
import { useSecondOpinions } from "./useSecondOpinions";
import type { ToastKind } from "./useToasts";

/** The open conversation's second opinions as the transcript shows them
 * (`OpinionContext`), and the consent request a cloud reviewer of local
 * work needs. */
export function useConversationOpinions({
  workspace,
  sessionId,
  targets,
  toast,
  selectTarget,
  draft,
  reviewTask,
  openSession,
}: {
  workspace: string;
  sessionId: string;
  targets: PickerTarget[];
  toast: (text: string, kind?: ToastKind) => void;
  /** Choose the composer's model for this conversation. */
  selectTarget: (id: string) => Promise<void>;
  /** Put text in the composer (after any draft) and focus it. */
  draft: (text: string) => void;
  reviewTask: (taskId: string) => void;
  openSession: (id: string) => Promise<void>;
}) {
  const scope = useMemo(
    () =>
      workspace && sessionId ? { workspace, session_id: sessionId } : null,
    [workspace, sessionId],
  );
  const opinions = useSecondOpinions(scope, toast);
  const handlers = useRef({ selectTarget, draft, reviewTask, openSession });
  handlers.current = { selectTarget, draft, reviewTask, openSession };
  const { items, working, consent } = opinions;
  const context = useMemo<ConversationOpinions>(
    () => ({
      targets,
      opinions,
      ask: async (taskId, model, question) => {
        await opinions.start({
          kind: "ask",
          source: "task",
          task_id: taskId,
          model,
          question: question || undefined,
        });
      },
      actions: {
        working,
        fix: (opinion, finding) =>
          void opinions.fix(opinion.id, finding.id).then((job) => {
            if (job) toast("Fix queued in this conversation.", "ok");
          }),
        setFinding: (opinion, finding, status) =>
          void opinions.setFinding(opinion.id, finding.id, status),
      },
      onContinue: (opinion: SecondOpinion) => {
        void handlers.current.selectTarget(opinion.reviewer.model);
        handlers.current.draft(continuePrompt(opinion));
        toast(
          `Your next message goes to ${opinion.reviewer.label}. Earlier turns are handed over as a summary.`,
          "info",
        );
      },
      onShowInReview: (opinion: SecondOpinion) => {
        if (opinion.task_id) handlers.current.reviewTask(opinion.task_id);
      },
      onOpenWork: (opinion: SecondOpinion) =>
        void handlers.current.openSession(opinion.review_session),
    }),
    // `opinions`' callbacks are stable; its items, working and consent
    // are what change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [targets, items, working, consent, toast],
  );
  return { context, consent };
}
