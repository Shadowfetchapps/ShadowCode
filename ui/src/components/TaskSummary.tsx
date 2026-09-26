import { memo, useEffect, useState } from "react";
import { FileDiff, FlaskConical, Timer } from "lucide-react";
import {
  formatDuration,
  verificationLine,
  type TaskActivity,
} from "../lib/activity";
import type { LineCounts } from "../lib/diffStats";

export type DiffStat = LineCounts;

/** Final card for a finished task: changed files (with +/- from the diff API
 * when it has them), recorded checks with exit codes, duration, and a way to
 * review the changes. `diffStats` counts every file in one request;
 * `diffStat` (one file per call) remains for callers without it. */
export const TaskSummary = memo(function TaskSummary({
  activity,
  onReview,
  onRewind,
  diffStat,
  diffStats,
}: {
  activity: TaskActivity;
  onReview: (path?: string) => void;
  onRewind?: () => void;
  diffStat?: (path: string) => Promise<DiffStat>;
  diffStats?: (paths: string[]) => Promise<Record<string, DiffStat>>;
}) {
  const [stats, setStats] = useState<Record<string, DiffStat>>({});
  const changed = activity.changed;
  const changedKey = changed.join("\n");
  useEffect(() => {
    if ((!diffStat && !diffStats) || !changedKey) return;
    let live = true;
    const paths = changedKey.split("\n").slice(0, 20);
    const read = diffStats
      ? diffStats(paths).catch(() => ({}) as Record<string, DiffStat>)
      : Promise.all(
          paths.map(async (path) => {
            try {
              return [path, await diffStat!(path)] as const;
            } catch {
              return [path, null] as const;
            }
          }),
        ).then((rows) => Object.fromEntries(rows));
    void read.then((next) => {
      if (live) setStats(next);
    });
    return () => {
      live = false;
    };
  }, [diffStat, diffStats, changedKey]);
  const verification = activity.verification;
  const duration =
    activity.startedAt && activity.finishedAt
      ? formatDuration(activity.finishedAt - activity.startedAt)
      : null;
  const stopped = Boolean(activity.finished?.cancelled);
  // A subscription that ran out of plan is a warning, not a failure: the
  // work so far stands and the conversation can continue elsewhere.
  const limited = !stopped && Boolean(activity.finished?.limitReached);
  const outcome = stopped
    ? "Stopped"
    : activity.finished?.success
      ? "Finished"
      : limited
        ? "Plan limit reached"
        : "Finished with problems";
  // A stop the user asked for is not a failure, and an answer that changed
  // nothing needs no report. With nothing changed, no checks run and no
  // unverified claims, one quiet line says so.
  const quiet =
    (stopped || limited || Boolean(activity.finished?.success)) &&
    changed.length === 0 &&
    !verification?.commands.length &&
    verification?.presentedAs !== "unverified";
  if (quiet) {
    return (
      <section
        className={`task-summary is-quiet${limited ? " is-warn" : ""}`}
        aria-label="Task summary"
      >
        <header>
          <strong>{outcome}</strong>
          {duration && (
            <span className="dim">
              <Timer size={12} aria-hidden="true" /> {duration}
            </span>
          )}
          <span className="dim">No files were changed.</span>
          {!stopped && !limited && (
            <span className="dim">
              {verification?.status === "vendor_owned"
                ? "Verification is provider-reported."
                : "Verification not run."}
            </span>
          )}
        </header>
      </section>
    );
  }
  return (
    <section
      className={`task-summary ${activity.finished?.success || stopped ? "" : limited ? "is-warn" : "is-bad"}`}
      aria-label="Task summary"
    >
      <header>
        <strong>{outcome}</strong>
        {duration && (
          <span className="dim">
            <Timer size={12} aria-hidden="true" /> {duration}
          </span>
        )}
      </header>
      <div className="task-summary-block">
        <h4>
          <FileDiff size={13} aria-hidden="true" /> Changed files
        </h4>
        {changed.length ? (
          <ul>
            {changed.map((path) => {
              const stat = stats[path];
              return (
                <li key={path}>
                  <button
                    type="button"
                    className="link"
                    onClick={() => onReview(path)}
                  >
                    {path}
                  </button>
                  {stat && (stat.add > 0 || stat.del > 0) && (
                    <span className="diff-stat">
                      <span className="add">+{stat.add}</span>{" "}
                      <span className="del">−{stat.del}</span>
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
        ) : (
          <p className="dim">No file changes were recorded.</p>
        )}
      </div>
      <div className="task-summary-block">
        <h4>
          <FlaskConical size={13} aria-hidden="true" /> Commands and checks
        </h4>
        {verification?.status === "vendor_owned" ? (
          <p className="dim">
            {verification.note ||
              "The vendor agent ran and judged its own checks; ShadowCode did not verify them."}
          </p>
        ) : verification?.commands.length ? (
          <>
            <p className="dim">{verificationLine(verification)}</p>
            <ul>
              {verification.commands.map((c, i) => (
                <li
                  key={`${c.command}-${i}`}
                  className={c.success ? "" : "bad"}
                >
                  <code>{c.command}</code>{" "}
                  <span className={c.success ? "ok" : "bad"}>
                    {c.state && c.kind === "configured_check"
                      ? c.state
                      : c.timed_out
                        ? "timed out"
                        : c.exit_code == null
                          ? c.success
                            ? "completed"
                            : "failed"
                          : `exit ${c.exit_code}`}
                  </span>
                  {c.callId && (
                    <details>
                      <summary>Execution receipt</summary>
                      <p>{c.scope}</p>
                      <p>
                        Source: {c.provenance} · Attempt: {c.attemptId}
                      </p>
                      <p>
                        Directory: <code>{c.cwd}</code>
                      </p>
                      {c.fingerprint && (
                        <p>
                          Content fingerprint: <code>{c.fingerprint}</code>
                        </p>
                      )}
                      <pre>
                        {activity.calls.find((call) => call.callId === c.callId)
                          ?.output ||
                          "Recorded output is available in this task’s command activity."}
                      </pre>
                    </details>
                  )}
                </li>
              ))}
            </ul>
          </>
        ) : (
          <p className="dim">Verification not run.</p>
        )}
        {verification?.presentedAs === "unverified" && (
          <p className="warn-text">
            The answer claims results that no recorded check confirms.
          </p>
        )}
      </div>
      <div className="row task-summary-actions">
        {changed.length > 0 && (
          <button
            type="button"
            className="mini"
            onClick={() => onReview(changed[0])}
          >
            Review changes
          </button>
        )}
        {onRewind && changed.length > 0 && (
          <button
            type="button"
            className="mini ghost"
            title="Undo every file change made by this task"
            onClick={onRewind}
          >
            Rewind
          </button>
        )}
      </div>
    </section>
  );
});
