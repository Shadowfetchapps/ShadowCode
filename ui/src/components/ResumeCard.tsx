import type { ChatItem } from "./cards";
import { scheduledResumeText } from "../lib/spending";
import { useWallClock } from "../hooks/useWallClock";

type ResumeItem = Extract<ChatItem, { kind: "resume" }>;

/** A scheduled resume after a plan limit: waiting (with Cancel), started,
 * cancelled, missed, failed, or ready but needing the user's review before
 * the conversation goes to a cloud model. */
export function ResumeCard({
  item,
  disabled,
  onCancel,
  onResumeNow,
  now,
}: {
  item: ResumeItem;
  disabled?: boolean;
  onCancel: () => void;
  onResumeNow: (item: ResumeItem) => void;
  now?: number;
}) {
  const warning = item.state === "failed" || item.state === "missed";
  // "tomorrow" is true only until midnight: a waiting resume is worded now,
  // and again at midnight in a window left open.
  const current = useWallClock(now);
  const text =
    item.state === "scheduled" && item.at
      ? scheduledResumeText(item.label, item.at, current)
      : item.text;
  return (
    <div
      className={`msg-note limit-note resume-note${warning ? " warning" : ""}`}
      role="status"
      aria-label="Scheduled resume"
    >
      <span>{text}</span>
      {item.state === "scheduled" && (
        <button
          type="button"
          className="mini"
          disabled={disabled}
          onClick={onCancel}
        >
          Cancel resume
        </button>
      )}
      {item.state === "needs_consent" && item.task && (
        <button
          type="button"
          className="mini primary-mini"
          disabled={disabled}
          onClick={() => onResumeNow(item)}
        >
          Review and resume
        </button>
      )}
    </div>
  );
}
