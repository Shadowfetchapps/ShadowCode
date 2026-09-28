import { memo, useEffect, useRef, useState } from "react";
import { FileDiff, FlaskConical, Timer } from "lucide-react";
import {
  formatDuration,
  verificationLine,
  parseVerification,
  receiptOutput,
  type Verification,
  type TaskActivity,
} from "../lib/activity";
import { isNative, listen } from "../lib/transport";
import { onWorkspaceFilesChanged } from "../lib/workspaceChanges";
import { onVerificationRefresh } from "../lib/verificationRefresh";
import type { LineCounts } from "../lib/diffStats";
import { TaskTimingDetails } from "./TaskTimingDetails";
import { LocalModelDetails } from "./LocalModelDetails";
import { RunCheck, type RunCheckAction } from "./RunCheck";

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
  readVerification,
  runCheck,
  workspace,
  sessionId,
}: {
  activity: TaskActivity;
  workspace?: string;
  sessionId?: string;
  runCheck?: RunCheckAction;
  readVerification?: (
    attemptId: string,
    signal?: AbortSignal,
  ) => Promise<unknown>;
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
  const summaryRef = useRef<HTMLElement>(null);
  const [fresh, setFresh] = useState<{
    attemptId: string;
    source: Verification | undefined;
    value: Verification;
    workspace: string | undefined;
    sessionId: string | undefined;
    read: typeof readVerification;
  }>();
  const [assessment, setAssessment] = useState("checking");
  const refreshAssessment = useRef<() => void>(() => {});
  const attemptId = activity.verification?.commands.find(
    (c) => c.kind === "configured_check",
  )?.attemptId;
  const canAssess = Boolean(readVerification && attemptId);
  useEffect(() => {
    if (!readVerification || !attemptId) return;
    let live = true,
      visible = false,
      running = false,
      pending = false;
    let generation = 0;
    let controller: AbortController | undefined;
    let unlisten: (() => void) | undefined;
    setFresh(undefined);
    setAssessment("checking");
    const assess = async () => {
      if (!visible || document.visibilityState === "hidden" || running || !live)
        return;
      running = true;
      pending = false;
      const readingGeneration = generation;
      controller = new AbortController();
      setAssessment("checking");
      try {
        const result = parseVerification(
          await readVerification(attemptId, controller.signal),
        );
        if (live && readingGeneration === generation) {
          setFresh(
            result
              ? {
                  attemptId,
                  source: activity.verification,
                  value: result,
                  workspace,
                  sessionId,
                  read: readVerification,
                }
              : undefined,
          );
          setAssessment(result ? "current" : "unavailable");
        }
      } catch {
        if (live && readingGeneration === generation)
          setAssessment("unavailable");
      } finally {
        running = false;
        // An edit during the read invalidates its result and queues one fresh
        // read. Bursts coalesce without allowing the pre-edit green to win.
        if (live && pending) void assess();
      }
    };
    const invalidate = () => {
      if (!live) return;
      ++generation;
      pending = true;
      setFresh(undefined);
      setAssessment("checking");
      void assess();
    };
    refreshAssessment.current = invalidate;
    const suspend = () => {
      ++generation;
      controller?.abort();
      setFresh(undefined);
      setAssessment("checking");
    };
    const stopRefresh = onVerificationRefresh(() => {
      if (visible && !running) void assess();
    });
    const stopFiles = workspace
      ? onWorkspaceFilesChanged(workspace, invalidate)
      : undefined;
    if (isNative()) {
      void listen("shadowcode:events", (payload) => {
        const event = payload as {
          type?: unknown;
          session_id?: unknown;
        } | null;
        // Engine wake-ups lack workspace identity. Named events are scoped to
        // this conversation; untyped reconnect/lag wake-ups may hide anything.
        if (
          typeof event?.session_id === "string" &&
          event.session_id &&
          event.session_id !== sessionId
        )
          return;
        if (
          typeof event?.type !== "string" ||
          !event.type ||
          event.type.startsWith("view.") ||
          [
            "tool.completed",
            "files.changed",
            "checkpoint.updated",
            "checkpoint.restored",
            "checkpoint.rewind_undone",
            "review.undone",
            "agent.completed",
          ].includes(event.type)
        )
          invalidate();
      })
        .then((stop) => {
          if (live) unlisten = stop;
          else stop();
        })
        .catch(() => {
          // Mount, local edits, focus and explicit refresh still work offline.
        });
    }
    const observer =
      typeof IntersectionObserver === "undefined"
        ? null
        : new IntersectionObserver((entries) => {
            const nextVisible = entries.some((entry) => entry.isIntersecting);
            if (visible && !nextVisible) suspend();
            visible = nextVisible;
            if (visible) invalidate();
          });
    if (observer && summaryRef.current) observer.observe(summaryRef.current);
    else {
      visible = true;
      void assess();
    }
    const focus = () => {
      if (document.visibilityState !== "hidden") invalidate();
      else suspend();
    };
    window.addEventListener("focus", focus);
    document.addEventListener("visibilitychange", focus);
    return () => {
      live = false;
      controller?.abort();
      refreshAssessment.current = () => {};
      stopRefresh();
      observer?.disconnect();
      stopFiles?.();
      unlisten?.();
      window.removeEventListener("focus", focus);
      document.removeEventListener("visibilitychange", focus);
    };
  }, [
    attemptId,
    readVerification,
    activity.verification,
    workspace,
    sessionId,
  ]);
  const verification = canAssess
    ? assessment === "current" &&
      fresh &&
      fresh.attemptId === attemptId &&
      fresh.source === activity.verification &&
      fresh.workspace === workspace &&
      fresh.sessionId === sessionId &&
      fresh.read === readVerification
      ? fresh.value
      : {
          ...activity.verification!,
          status: assessment === "current" ? "checking" : assessment,
          commands: activity.verification!.commands.map((c) =>
            c.kind === "configured_check"
              ? { ...c, state: "not_run", success: false }
              : c,
          ),
        }
    : activity.verification;
  const duration = activity.timings?.complete
    ? `Total ${formatDuration(activity.timings.total_seconds)}`
    : activity.startedAt && activity.finishedAt
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
        ref={summaryRef}
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
        <TaskTimingDetails timings={activity.timings} />
        <LocalModelDetails receipt={activity.localRuntime} />
        {runCheck && (
          <div className="row task-summary-actions">
            <RunCheck action={runCheck} />
          </div>
        )}
      </section>
    );
  }
  return (
    <section
      ref={summaryRef}
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
        {canAssess && (
          <>
            <button
              type="button"
              className="mini"
              onClick={() => refreshAssessment.current()}
            >
              Refresh check evidence
            </button>
            <p className="dim">
              Reassessed every five seconds while visible. Point-in-time
              evidence; original receipts remain in history.
            </p>
          </>
        )}
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
                  {(c.callId || c.outputRef) && (
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
                        {receiptOutput(activity, c) ??
                          "The recorded output could not be matched to this execution receipt."}
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
      <TaskTimingDetails timings={activity.timings} />
      <LocalModelDetails receipt={activity.localRuntime} />
      <div className="row task-summary-actions">
        {runCheck && <RunCheck action={runCheck} />}
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
