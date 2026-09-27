import type { TaskTimings } from "../api";
import { measured, parseTimings, timingSeconds } from "../lib/timing";
import "./TaskTimingDetails.css";

/** Versioned observations only: older tasks and opaque vendor internals do
 * not acquire invented timings when a history is replayed. */
export function TaskTimingDetails({
  timings,
}: {
  timings?: TaskTimings | null;
}) {
  const t = parseTimings(timings);
  if (!t) return null;
  const rows: [string, number | null | undefined][] = [
    ["Total elapsed", t.total_seconds],
    ["In queue", t.queue_seconds],
    ["Active task", t.active_seconds],
    ["Local preparation", t.preparation_seconds],
    ["Runtime wait · within preparation", t.runtime_wait_seconds],
    ["Model load · within preparation", t.model_load_seconds],
    [
      `Model requests · ${t.model_requests} attempt${t.model_requests === 1 ? "" : "s"}`,
      t.model_requests_seconds,
    ],
    [`First text · request ${t.first_text_request}`, t.first_text_seconds],
    ["Tool batches", t.tool_batches_seconds],
    ["Completion assessment", t.final_checks_seconds],
    ["Configured check processes", t.check_process_seconds],
  ];
  return (
    <details className="task-timings">
      <summary>Timing details{!t.complete && " · partial"}</summary>
      <dl>
        {t.model_reused != null && (
          <div>
            <dt>Local model startup</dt>
            <dd>
              {t.model_reused ? "Already loaded" : "Loaded for this task"}
            </dd>
          </div>
        )}
        {rows
          .filter(([, value]) => measured(value))
          .map(([label, value]) => (
            <div key={label}>
              <dt>{label}</dt>
              <dd>{timingSeconds(value!)}</dd>
            </div>
          ))}
      </dl>
      <p className="dim">
        Total includes the queue. Preparation includes runtime waiting and
        loading, which may include replacing a prior model. Timings overlap and
        do not add up to the total.
      </p>
      {t.model_requests > 0 && (
        <p className="dim">
          Model requests include foreground attempts, retries and response
          decoding; they exclude tools and context compaction. First text is
          measured from its request start and can be buffered. It is not time to
          first token.
          {!measured(t.first_text_seconds) && " No text was observed."}
        </p>
      )}
      {(measured(t.tool_batches_seconds) ||
        measured(t.final_checks_seconds)) && (
        <p className="dim">
          Tool batches include approval waits and hooks. Completion assessment
          includes completion hooks and evidence refresh, or an explicit check
          task. Command checks may also appear within tool time.
        </p>
      )}
      {!t.complete && (
        <p className="dim">
          Observed so far; this is not a complete task measurement.
        </p>
      )}
      {measured(t.check_process_seconds) && (
        <p className="dim">
          Configured check process time excludes approval waits and file
          fingerprinting. It overlaps tool or completion-assessment time and
          includes failed or cancelled processes with recorded results.
        </p>
      )}
    </details>
  );
}
