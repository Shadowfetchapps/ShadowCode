/** Second opinions inside a conversation: "Ask another model" under each
 * finished answer, and the labelled cards of the conversation's second
 * opinions after the task they are about. Rows reach them through
 * `OpinionContext`, so a memoized row does not re-render when a card does. */
import { createContext, useContext, useMemo, useState } from "react";
import { MessagesSquare } from "lucide-react";
import {
  AskAnotherModel,
  OpinionCard,
  type FindingActions,
} from "./SecondOpinion";
import {
  useOpinionOptions,
  type SecondOpinions,
} from "../hooks/useSecondOpinions";
import type { PickerTarget } from "../lib/picker";
import { suggestReviewer, type SecondOpinion } from "../lib/secondOpinion";

export type ConversationOpinions = {
  targets: PickerTarget[];
  opinions: SecondOpinions;
  /** Ask `model` for a second opinion on a task's answer. */
  ask: (taskId: string, model: string, question: string) => Promise<void>;
  actions: FindingActions;
  /** The next message goes to the reviewer, with its opinion drafted. */
  onContinue: (opinion: SecondOpinion) => void;
  /** Open the task's Review view, where findings sit next to the hunks. */
  onShowInReview: (opinion: SecondOpinion) => void;
  /** Open the reviewer's own (hidden) conversation. */
  onOpenWork: (opinion: SecondOpinion) => void;
};

export const OpinionContext = createContext<ConversationOpinions | null>(null);

/** The inline form under one answer: a reviewer suggested for that task
 * (not the model that answered) and an optional question. */
function AskForm({
  taskId,
  context,
  onClose,
}: {
  taskId: string;
  context: ConversationOpinions;
  onClose: () => void;
}) {
  const scope = useMemo(() => ({ task_id: taskId }), [taskId]);
  const { options } = useOpinionOptions(scope);
  if (!options) return <p className="hint">Loading models…</p>;
  const initial = suggestReviewer(context.targets, {
    writer: options.writer?.model,
    remembered: options.prefs.model,
    offline: options.offline,
    localOnly: options.local_only,
  });
  return (
    <AskAnotherModel
      targets={context.targets}
      initial={initial}
      offline={options.offline}
      writer={options.writer}
      onClose={onClose}
      onAsk={(model, question) => {
        onClose();
        void context.ask(taskId, model, question);
      }}
    />
  );
}

/** "Ask another model" for one answer; renders nothing without a
 * conversation context (history pages, tests). */
export function AskAnotherModelButton({
  taskId,
  open,
  onOpen,
}: {
  taskId: string;
  open: boolean;
  onOpen: () => void;
}) {
  const context = useContext(OpinionContext);
  if (!context || !taskId) return null;
  return (
    <button
      type="button"
      className="icon-btn"
      aria-label="Ask another model"
      aria-expanded={open}
      title="Ask another model for a second opinion (it changes nothing)"
      onClick={onOpen}
    >
      <MessagesSquare size={13} aria-hidden="true" />
    </button>
  );
}

export function AskAnotherModelForm({
  taskId,
  onClose,
}: {
  taskId: string;
  onClose: () => void;
}) {
  const context = useContext(OpinionContext);
  if (!context) return null;
  return <AskForm taskId={taskId} context={context} onClose={onClose} />;
}

/** State for one answer's form, shared by its button and the form. */
export function useAskOpen() {
  const [open, setOpen] = useState(false);
  return {
    open,
    toggle: () => setOpen((v) => !v),
    close: () => setOpen(false),
  };
}

/** The second opinions about one task, oldest first. */
export function TaskOpinions({ taskId }: { taskId: string }) {
  const context = useContext(OpinionContext);
  if (!context) return null;
  const cards = context.opinions.items
    .filter(
      (opinion) => opinion.task_id === taskId && opinion.source === "task",
    )
    .sort((a, b) => a.created_at - b.created_at);
  if (!cards.length) return null;
  return (
    <>
      {cards.map((opinion) => (
        <OpinionCard
          key={opinion.id}
          opinion={opinion}
          actions={context.actions}
          onCancel={() => void context.opinions.cancel(opinion.id)}
          onContinue={
            opinion.kind === "ask"
              ? () => context.onContinue(opinion)
              : undefined
          }
          onShowInReview={() => context.onShowInReview(opinion)}
          onOpenWork={
            opinion.review_session
              ? () => context.onOpenWork(opinion)
              : undefined
          }
        />
      ))}
    </>
  );
}
