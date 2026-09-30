import { useState } from "react";
import { GitBranch, GitMerge, LoaderCircle, Trash2 } from "lucide-react";
import type { WorktreeTask } from "../api";
import type { WorktreeAction } from "../hooks/useWorktreeTask";
import { Term } from "./Term";

const ACTIVE = ["queued", "running", "paused", "cancelling"];

type SetupCommand = NonNullable<
  NonNullable<WorktreeTask["setup"]>["commands"]
>[number];

/** Why a setup command failed, in a few words. */
export function setupFailure(command: SetupCommand): string {
  if (command.exit_code != null) return `exited with ${command.exit_code}`;
  switch (command.stopped) {
    case "timeout":
      return "took too long and was stopped";
    case "cancelled":
      return "was stopped when ShadowCode closed";
    case "signal":
      return command.signal
        ? `was stopped (signal ${command.signal})`
        : "was stopped";
    case "not_started":
      return "could not start";
    default:
      return "did not finish";
  }
}

/** Shown above the composer while a conversation runs in its own worktree:
 * what changed there and, once it is done, Apply to project / Keep as
 * branch / Discard. */
export function WorktreeBar({
  task,
  acting,
  onAct,
}: {
  task: WorktreeTask;
  acting: WorktreeAction | "";
  onAct: (action: WorktreeAction) => void;
}) {
  const [confirmDiscard, setConfirmDiscard] = useState(false);
  const working = task.state === "running" || ACTIVE.includes(task.status);
  const files = task.changed_files.length;
  const busy = Boolean(acting);
  const project = task.workspace.split("/").pop() || task.workspace;
  const failed = task.setup?.commands?.find((c) => !c.ok);
  return (
    <section
      className="worktree-bar"
      aria-label="This conversation runs in its own worktree"
    >
      <div className="worktree-bar-head">
        <span className="worktree-badge">
          <GitBranch size={13} aria-hidden="true" />
          <Term word="worktree">Worktree</Term>
        </span>
        <span className="worktree-summary" title={task.worktree}>
          {working
            ? `Working in its own copy of ${project}; the main checkout is free for other tasks.`
            : files
              ? `${files}${task.changed_files_truncated ? "+" : ""} file${files === 1 ? "" : "s"} changed in its own copy of ${project}.`
              : `No changes in its own copy of ${project}.`}
        </span>
        {task.port ? (
          <span
            className="worktree-port"
            title="Dev servers started in this task use this port (PORT)"
          >
            Port {task.port}
          </span>
        ) : null}
      </div>
      {failed && (
        <details className="worktree-setup-failed">
          <summary>
            Setup did not finish: <code>{failed.command}</code>{" "}
            {setupFailure(failed)}. The task ran anyway; install steps may be
            missing.
          </summary>
          <pre>{failed.output}</pre>
        </details>
      )}
      {files > 0 && (
        <ul className="worktree-files" aria-label="Changed files">
          {task.changed_files.slice(0, 6).map((file) => (
            <li key={file.path}>
              <span className={`wt-status wt-${file.status}`}>
                {file.status === "added"
                  ? "A"
                  : file.status === "deleted"
                    ? "D"
                    : "M"}
              </span>
              <span className="wt-path">{file.path}</span>
              {!file.binary && (
                <span className="wt-counts">
                  +{file.additions} −{file.deletions}
                </span>
              )}
            </li>
          ))}
          {files > 6 && <li className="hint">and {files - 6} more</li>}
        </ul>
      )}
      {task.conflicts.length > 0 && (
        <div className="worktree-conflicts" role="alert">
          <strong>Nothing was applied.</strong> These files changed in the
          project since this task started: {task.conflicts.join(", ")}. Update
          or revert them there, keep this result as a branch, or discard it.
        </div>
      )}
      {!working && (
        <div className="worktree-actions">
          <button
            type="button"
            className="primary"
            disabled={busy}
            onClick={() => onAct("apply")}
            title="Check that the changes apply cleanly, then write them to the project's files (not committed)"
          >
            {acting === "apply" ? (
              <LoaderCircle size={13} className="spin" aria-hidden="true" />
            ) : (
              <GitMerge size={13} aria-hidden="true" />
            )}
            Apply to project
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => onAct("keep-branch")}
            title={`Commit the result on branch ${task.branch} and remove the worktree`}
          >
            <GitBranch size={13} aria-hidden="true" />
            Keep as branch
          </button>
          {confirmDiscard ? (
            <>
              <span className="hint">Remove the worktree and its changes?</span>
              <button
                type="button"
                className="danger"
                disabled={busy}
                onClick={() => {
                  setConfirmDiscard(false);
                  onAct("discard");
                }}
              >
                Discard
              </button>
              <button type="button" onClick={() => setConfirmDiscard(false)}>
                Cancel
              </button>
            </>
          ) : (
            <button
              type="button"
              disabled={busy}
              onClick={() => setConfirmDiscard(true)}
            >
              <Trash2 size={13} aria-hidden="true" />
              Discard…
            </button>
          )}
        </div>
      )}
    </section>
  );
}
