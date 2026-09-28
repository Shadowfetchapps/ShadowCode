/** Plain-language labels for engine states that the window shows as text.
 * The engine stores these as identifiers (`RUNNING`, `paused`); people read
 * short sentence-case words. Unknown values are shown, not hidden, in the
 * same case as the rest of the interface. */

function sentenceCase(value: string): string {
  const words = value.replace(/[_-]+/g, " ").trim().toLowerCase();
  return words ? words[0].toUpperCase() + words.slice(1) : "";
}

const PROCESS: Record<string, string> = {
  STARTING: "Starting",
  RUNNING: "Running",
  STOPPING: "Stopping",
  COMPLETED: "Finished",
  FAILED: "Failed",
  CANCELLED: "Stopped",
  INTERRUPTED: "Interrupted",
};

/** A background process's state (Tools › Processes). */
export function processStatusLabel(status: string): string {
  return PROCESS[status.toUpperCase()] ?? sentenceCase(status);
}

/** A goal's state (Tools › Goals). */
export function goalStatusLabel(goal: {
  status: string;
  running?: boolean;
  progress?: number;
}): string {
  if (goal.running) return "Running…";
  switch (goal.status) {
    case "active":
      return (goal.progress ?? 0) > 0 ? "In progress" : "Not started";
    case "completed":
      return "Done";
    default:
      return sentenceCase(goal.status);
  }
}
