import { ListOrdered, LoaderCircle, X } from "lucide-react";
import { useEffect, useState } from "react";
import { api, type Job, type Session } from "../api";

const modeName = (job: Job) => {
  const mode = job.purpose || job.routing?.purpose || job.mode || "code";
  return (
    (
      {
        coder: "Build",
        code: "Build",
        planner: "Plan",
        plan: "Plan",
        reviewer: "Review",
        review: "Review",
        tester: "Test",
        test: "Test",
        command: "Command",
        roles: "Roles",
      } as Record<string, string>
    )[mode] || mode
  );
};

function QueuedPrompt({ job }: { job: Job }) {
  const [open, setOpen] = useState(false);
  const [full, setFull] = useState<string | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    if (!open || !job.task_truncated || full !== null) return;
    let live = true;
    setError("");
    void api
      .job(job.id)
      .then((record) => {
        if (live) setFull(record.task || "");
      })
      .catch(() => {
        if (live)
          setError(
            "Could not load the full message. Close and reopen to retry.",
          );
      });
    return () => {
      live = false;
    };
  }, [open, job.id, job.task_truncated, full]);
  return (
    <details onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary title={job.task}>
        {job.task || "Queued task"}
        {job.task_truncated ? "…" : ""}
      </summary>
      <p>{full ?? job.task ?? "Queued task"}</p>
      {job.task_truncated && full === null && (
        <p role="status">{error || "Loading full message…"}</p>
      )}
    </details>
  );
}

export function QueuedTasks({
  jobs,
  sessions,
  selected,
  cancelling,
  disabled,
  onCancel,
  onOpen,
}: {
  jobs: Job[];
  sessions: Session[];
  selected: string;
  cancelling: string[];
  disabled: boolean;
  onCancel: (job: Job) => void;
  onOpen: (id: string) => void;
}) {
  if (!jobs.length) return null;
  return (
    <section className="task-queue" aria-label="Queued follow-ups">
      <div className="queue-heading">
        <ListOrdered size={15} />
        <strong>Up next</strong>
        <span className="queue-count">{jobs.length}</span>
        <span className="queue-help">Runs in order for this project</span>
      </div>
      <ol tabIndex={0} aria-label="Queued messages">
        {jobs.map((job, index) => (
          <li key={job.id} data-job-id={job.id}>
            <span className="queue-position" aria-hidden="true">
              {index + 1}
            </span>
            <div className="queue-message">
              <QueuedPrompt job={job} />
              <div className="queue-meta">
                <span>{job.model || "Selected model"}</span>
                <span>{modeName(job)}</span>
                {job.session_id !== selected && (
                  <button
                    type="button"
                    disabled={disabled}
                    onClick={() => onOpen(job.session_id)}
                  >
                    {sessions.find((session) => session.id === job.session_id)
                      ?.title || "Open conversation"}
                  </button>
                )}
              </div>
            </div>
            <button
              type="button"
              className="icon-btn queue-cancel"
              aria-label={`Cancel queued task ${index + 1}`}
              title="Cancel this queued task"
              disabled={disabled || cancelling.includes(job.id)}
              onClick={() => onCancel(job)}
            >
              {cancelling.includes(job.id) ? (
                <LoaderCircle size={14} className="spin" />
              ) : (
                <X size={14} />
              )}
            </button>
          </li>
        ))}
      </ol>
    </section>
  );
}
