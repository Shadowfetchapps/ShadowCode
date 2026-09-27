import type { Job } from "../api";

export const isActive = (job: Job | null) =>
  !!job && ["queued", "running", "paused", "cancelling"].includes(job.status);

/** Jobs arrive newest submission first. Follow execution order, and prefer the
 * last actual completion over a newer submission cancelled while still queued. */
export function conversationJob(
  jobs: Job[],
  previous: Job | null,
): Job | undefined {
  return (
    jobs.find((job) => job.status === "running") ||
    jobs.find((job) => job.status === "paused") ||
    jobs.find((job) => job.status === "cancelling") ||
    jobs
      .slice()
      .reverse()
      .find((job) => job.status === "queued") ||
    [...jobs, ...(previous ? [previous] : [])].sort(
      (a, b) =>
        (b.finished_at || b.started_at) - (a.finished_at || a.started_at),
    )[0]
  );
}
