import { memo, type ReactNode } from "react";
import { Check, CircleAlert, CircleDashed, LoaderCircle } from "lucide-react";
import {
  deriveSteps,
  type TaskActivity,
  type TimelineStep,
} from "../lib/activity";

/** One compact timeline for a task. Every step is backed by recorded events
 * and expands to the real tool calls and their output. Memoized: a task's
 * activity object only changes when its own events arrive. */
export const ActivityTimeline = memo(function ActivityTimeline({
  activity,
  pendingApprovals = 0,
  elapsed,
  withSummary = false,
  jobStatus,
  localQueued = false,
}: {
  activity: TaskActivity | undefined;
  pendingApprovals?: number;
  /** Elapsed time of the running task (text or a ticking `<Elapsed>`). */
  elapsed?: ReactNode;
  /** A summary card follows and states the outcome; skip the last step. */
  withSummary?: boolean;
  jobStatus?: string;
  localQueued?: boolean;
}) {
  const statusLabel =
    jobStatus === "cancelling"
      ? "Stopping…"
      : jobStatus === "paused"
        ? "Paused"
        : jobStatus === "queued"
          ? localQueued
            ? "Queued for local model"
            : "Queued"
          : undefined;
  const steps = deriveSteps(activity, pendingApprovals).filter(
    (step) => !(withSummary && step.id === "finished"),
  );
  if (!steps.length && withSummary) return null;
  if (!steps.length || statusLabel)
    return (
      <div className="activity-timeline" aria-label="Agent activity">
        <div className="activity-step is-active">
          <LoaderCircle size={14} className="spin" aria-hidden="true" />
          <span>{statusLabel || "Working"}</span>
          {elapsed && <small>{elapsed}</small>}
        </div>
      </div>
    );
  return (
    <div className="activity-timeline" aria-label="Agent activity">
      {steps.map((step) => (
        <Step key={step.id} step={step} activity={activity!} />
      ))}
      {elapsed && !activity?.finished && (
        <small className="activity-elapsed">{elapsed}</small>
      )}
    </div>
  );
});

function StepIcon({ state }: { state: TimelineStep["state"] }) {
  if (state === "active")
    return <LoaderCircle size={14} className="spin" aria-hidden="true" />;
  if (state === "failed") return <CircleAlert size={14} aria-hidden="true" />;
  if (state === "done") return <Check size={14} aria-hidden="true" />;
  return <CircleDashed size={14} aria-hidden="true" />;
}

function Step({
  step,
  activity,
}: {
  step: TimelineStep;
  activity: TaskActivity;
}) {
  const sources = step.id === "web" ? activity.sources : [];
  const expandable = step.calls.length > 0 || sources.length > 0;
  const head = (
    <>
      <StepIcon state={step.state} />
      <span>{step.label}</span>
      {step.calls.length > 1 && (
        <small className="dim">{step.calls.length} actions</small>
      )}
      {step.detail && <small>{step.detail}</small>}
      <span className="sr-only">
        {step.state === "active"
          ? "in progress"
          : step.state === "failed"
            ? "with problems"
            : step.state === "done"
              ? "done"
              : "not verified or incomplete"}
      </span>
    </>
  );
  if (!expandable)
    return <div className={`activity-step is-${step.state}`}>{head}</div>;
  return (
    <details className={`activity-step is-${step.state}`}>
      <summary>{head}</summary>
      <ol className="activity-calls">
        {step.calls.map((call, index) => (
          <li
            key={`${call.callId}-${index}`}
            className={call.ok === false ? "bad" : ""}
          >
            <code>{call.command || call.path || call.label}</code>
            <span className="dim">
              {call.live
                ? "running"
                : call.ok === false
                  ? "failed"
                  : call.ok !== true
                    ? "outcome unknown"
                    : call.tool.includes(".")
                      ? call.tool
                      : "done"}
            </span>
            {call.output && (
              <pre>
                {call.output.slice(0, 4000)}
                {call.output.length > 4000 ? "\n… (truncated)" : ""}
              </pre>
            )}
          </li>
        ))}
      </ol>
      {sources.length > 0 && (
        <ul className="activity-sources" aria-label="Web sources">
          {sources.map((source) => (
            <li key={source.url}>
              <a
                href={source.final_url || source.url}
                target="_blank"
                rel="noreferrer"
              >
                {source.title || source.final_url || source.url}
              </a>
              {source.status != null && (
                <span className="dim"> · {String(source.status)}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </details>
  );
}
