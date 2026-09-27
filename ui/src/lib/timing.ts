import type { TaskTimings } from "../api";

export const measured = (value: unknown): value is number =>
  typeof value === "number" && Number.isFinite(value) && value >= 0;

export function parseTimings(value: unknown): TaskTimings | undefined {
  if (!value || typeof value !== "object") return;
  const row = value as Record<string, unknown>;
  if (
    row.schema_version !== 1 ||
    !measured(row.total_seconds) ||
    !measured(row.queue_seconds)
  )
    return;
  const optional = Object.fromEntries(
    [
      "active_seconds",
      "preparation_seconds",
      "runtime_wait_seconds",
      "model_load_seconds",
      "model_requests_seconds",
      "first_text_seconds",
      "first_text_request",
      "tool_batches_seconds",
      "final_checks_seconds",
      "check_process_seconds",
    ].map((key) => [key, measured(row[key]) ? row[key] : null]),
  );
  return {
    ...optional,
    model_reused:
      typeof row.model_reused === "boolean" ? row.model_reused : null,
    schema_version: 1,
    complete: row.complete === true,
    total_seconds: row.total_seconds,
    queue_seconds: row.queue_seconds,
    model_requests: measured(row.model_requests) ? row.model_requests : 0,
  };
}

export function timingSeconds(value: number): string {
  if (value === 0) return "0s";
  if (value < 0.01) return "<0.01s";
  if (value < 60) return `${value.toFixed(value < 10 ? 2 : 1)}s`;
  return `${Math.floor(value / 60)}m ${(value % 60).toFixed(1)}s`;
}
