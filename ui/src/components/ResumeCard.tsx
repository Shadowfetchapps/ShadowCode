import type { ChatItem } from "./cards";

type ResumeItem = Extract<ChatItem, { kind: "resume" }>;

/** A scheduled resume after a plan limit: waiting (with Cancel), started,
 * cancelled, missed, failed, or ready but needing the user's review before
 * the conversation goes to a cloud model. */
export function ResumeCard({
  item,
  disabled,
  onCancel,
  onResumeNow,
}: {
  item: ResumeItem;
  disabled?: boolean;
  onCancel: () => void;
  onResumeNow: (item: ResumeItem) => void;
}) {
  const warning = item.state === "failed" || item.state === "missed";
  return (
    <div
      className={`msg-note limit-note resume-note${warning ? " warning" : ""}`}
      role="status"
      aria-label="Scheduled resume"
    >
      <span>{item.text}</span>
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
