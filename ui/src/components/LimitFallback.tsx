import type { ChatItem } from "./cards";
import type { Fallback } from "../lib/allowance";
import { clockTime } from "../lib/spending";

type LimitItem = Extract<ChatItem, { kind: "limit" }>;

/** "Resume on Codex at 3:40 PM" when the vendor said when its plan resets
 * and nothing is scheduled yet. */
function ResumeButton({
  item,
  disabled,
  onSchedule,
  now,
}: {
  item: LimitItem;
  disabled?: boolean;
  onSchedule?: (item: LimitItem) => void;
  now?: number;
}) {
  const current = now ?? Date.now() / 1000;
  if (
    !onSchedule ||
    !item.jobId ||
    !item.resetsAt ||
    item.resetsAt <= current ||
    item.resumeScheduled
  )
    return null;
  return (
    <button
      type="button"
      className="mini"
      disabled={disabled}
      title={`Continue this conversation on ${item.from} when its plan resets. You can cancel it until then.`}
      onClick={() => onSchedule(item)}
    >
      Resume on {item.from} {clockTime(item.resetsAt, current)}
    </button>
  );
}

/** What happened after a subscription reported its plan limit
 * (limit.fallback): the conversation continued on this computer, the user is
 * asked, or nothing could continue. Every state offers "Try on…" (another
 * model the user picks) and, when the vendor said when its plan resets,
 * "Resume at …" on the same model. */
export function LimitFallbackItem({
  item,
  fallback,
  disabled,
  onContinue,
  onChoose,
  onOpenLocal,
  onScheduleResume,
  now,
}: {
  item: LimitItem;
  /** The local model "Continue on …" uses. */
  fallback: Fallback | null;
  disabled?: boolean;
  onContinue: (fallback: Fallback) => void;
  /** "Try on…": continue this task on a model the user picks. */
  onChoose: () => void;
  onOpenLocal: () => void;
  onScheduleResume?: (item: LimitItem) => void;
  now?: number;
}) {
  const resume = (
    <ResumeButton
      item={item}
      disabled={disabled}
      onSchedule={onScheduleResume}
      now={now}
    />
  );
  if (item.mode === "continued")
    return (
      <div className="msg-note limit-note">
        <span>{item.text}</span>
        {resume}
      </div>
    );
  if (item.mode === "unavailable")
    return (
      <div className="msg-note warning limit-note">
        <span>{item.text}</span>
        <button type="button" className="mini" onClick={onOpenLocal}>
          Open Local models
        </button>
        <button
          type="button"
          className="mini"
          disabled={disabled}
          onClick={onChoose}
        >
          Try on…
        </button>
        {resume}
      </div>
    );
  return (
    <section
      className={`limit-card${item.resolved ? " is-resolved" : ""}`}
      aria-label="Plan limit reached"
    >
      <p>
        <strong>{item.text}</strong>
      </p>
      {item.resolved ? null : (
        <>
          <p className="dim">
            {fallback
              ? "Keep this conversation going on a model on this computer, or try it on another model."
              : "No local model is ready. Try it on another model, or add one in Local models."}
          </p>
          <div className="row">
            {fallback ? (
              <button
                type="button"
                className="mini primary-mini"
                disabled={disabled}
                onClick={() => onContinue(fallback)}
              >
                Continue on {fallback.name}
              </button>
            ) : (
              <button type="button" className="mini" onClick={onOpenLocal}>
                Open Local models
              </button>
            )}
            <button
              type="button"
              className="mini"
              disabled={disabled}
              onClick={onChoose}
            >
              Try on…
            </button>
            {resume}
          </div>
        </>
      )}
    </section>
  );
}
