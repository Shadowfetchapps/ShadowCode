import type { RunRecord } from "../api";

/** US dollars for people: `$1.00`, `$0.04`, or `less than $0.01`. */
export function money(usd: number): string {
  if (usd > 0 && usd < 0.005) return "less than $0.01";
  return `$${Math.max(0, usd).toFixed(2)}`;
}

/** A clock time for "Resume at 3:40 PM": the time alone today, "tomorrow
 * at …" tomorrow, the weekday and time within a week, else the date. */
export function clockTime(seconds: number, now = Date.now() / 1000): string {
  const at = new Date(seconds * 1000);
  const today = new Date(now * 1000);
  const time = at.toLocaleTimeString(undefined, {
    hour: "numeric",
    minute: "2-digit",
  });
  const day = (d: Date) =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((day(at) - day(today)) / 86_400_000);
  if (days <= 0) return time;
  if (days === 1) return `tomorrow at ${time}`;
  if (days < 7)
    return `${at.toLocaleDateString(undefined, { weekday: "long" })} at ${time}`;
  return `${at.toLocaleDateString(undefined, { month: "short", day: "numeric" })} at ${time}`;
}

const DROPPED = new Set(["disconnected", "stalled", "connect_failed"]);

/** The retry line for a `model.retry` event:
 * "Provider busy, retrying (2 of 5) in 4 s…". */
export function retryText(payload: Record<string, unknown>): string {
  const attempt = Number(payload.attempt) || 1;
  const max = Math.max(attempt, Number(payload.max_attempts) || attempt);
  const seconds = Math.round((Number(payload.delay_ms) || 0) / 1000);
  const wait = seconds >= 1 ? `in ${seconds} s` : "now";
  const why = DROPPED.has(String(payload.reason))
    ? "Connection to the provider dropped"
    : "Provider busy";
  return `${why}, retrying (${attempt} of ${max}) ${wait}…`;
}

/** The retry line once the provider answered. */
export function retriedText(attempts: number): string {
  return `The provider was busy; it answered after ${attempts} ${
    attempts === 1 ? "retry" : "retries"
  }.`;
}

/** A run record from an `agent.completed` payload or a job, if present. */
export function parseRunRecord(value: unknown): RunRecord | undefined {
  if (!value || typeof value !== "object" || Array.isArray(value))
    return undefined;
  const run = value as Record<string, unknown>;
  if (typeof run.model_id !== "string" || typeof run.app_version !== "string")
    return undefined;
  const text = (key: string) =>
    typeof run[key] === "string" && run[key] ? String(run[key]) : null;
  return {
    model_id: run.model_id,
    model: text("model") || "",
    provider: text("provider") || "",
    route: text("route") || "",
    vendor: text("vendor"),
    vendor_version: text("vendor_version"),
    effort: text("effort"),
    app_version: run.app_version,
    app_commit: text("app_commit"),
    settings_hash: text("settings_hash") || "",
    rules_hash: text("rules_hash"),
    recorded_at: Number(run.recorded_at) || undefined,
  };
}

/** Rows for the "Run details" disclosure, in reading order. */
export function runDetailRows(run: RunRecord): [string, string][] {
  const rows: [string, string][] = [];
  rows.push(["Model", run.model_id || run.model]);
  if (run.model && run.model !== run.model_id)
    rows.push(["Model name", run.model]);
  if (run.provider) rows.push(["Provider", run.provider]);
  if (run.vendor)
    rows.push([
      "Ran with",
      run.vendor_version ? `${run.vendor} · ${run.vendor_version}` : run.vendor,
    ]);
  rows.push(["Effort", run.effort || "Model default"]);
  rows.push([
    "ShadowCode",
    run.app_commit
      ? `${run.app_version} (${run.app_commit.slice(0, 12)})`
      : run.app_version,
  ]);
  if (run.settings_hash) rows.push(["Settings", run.settings_hash]);
  rows.push(["Rules and skills", run.rules_hash || "None sent"]);
  return rows;
}

/** A spending limit from the settings form: empty text is "no limit". */
export function parseLimit(text: string): number | null | "invalid" {
  const trimmed = text.trim().replace(/^\$/, "");
  if (!trimmed) return null;
  const value = Number(trimmed);
  if (!Number.isFinite(value) || value < 0.01 || value > 100_000)
    return "invalid";
  return Math.round(value * 100) / 100;
}
