import { GitBranchPlus } from "lucide-react";

/** Start this message as a new conversation in a fresh worktree of the
 * project (More menu or Ctrl+Shift+Enter), beside a task in the main checkout. */
export function RunInWorktreeButton({
  reason,
  enabled,
  queueing,
  onRun,
}: {
  /** Why it cannot be used here. The disabled item stays visible so the user
   * can discover the feature and understand the current limitation. */
  reason: string | null;
  enabled: boolean;
  /** Another task is running here: Send would queue. */
  queueing: boolean;
  onRun: () => void;
}) {
  const label = queueing
    ? "Run now in a new worktree instead of queueing"
    : "Run in a new worktree";
  const disabled = !enabled || Boolean(reason);
  return (
    <button
      type="button"
      className={`worktree-btn${queueing ? " emphasis" : ""}`}
      aria-label={reason ? `${label} unavailable: ${reason}` : label}
      title={
        reason ||
        `${label} (Ctrl+Shift+Enter). It starts from the project's current files, including uncommitted work; you apply, keep or discard the result.`
      }
      aria-disabled={disabled}
      onClick={() => {
        if (!disabled) onRun();
      }}
    >
      <GitBranchPlus size={15} aria-hidden="true" />
      <span className="worktree-btn-text">
        {queueing ? "Run now in worktree" : "Worktree"}
      </span>
    </button>
  );
}
