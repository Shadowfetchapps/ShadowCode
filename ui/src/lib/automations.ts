import { request } from "./transport";

/** Scheduled automations (`/api/automations…`, docs/API_CONTRACT.md
 * and docs/AUTOMATIONS.md). Times are Unix seconds. */

export type Schedule =
  | { kind: "hourly"; minute: number }
  | { kind: "daily"; time: string }
  | { kind: "weekdays"; time: string }
  | { kind: "weekly"; day: number; time: string }
  | { kind: "cron"; expr: string };

export type AutomationOptions = {
  checkout: "worktree" | "main";
  permission: "project" | "read_only";
  on_approval: "stop" | "wait";
  max_runtime_minutes: number;
  catch_up_minutes: number;
  notify: boolean;
};

export type AutomationMode = "code" | "plan" | "ask";

export type AutomationDraft = {
  name: string;
  prompt: string;
  /** A picker id; "" runs on the project's model. */
  model: string;
  mode: AutomationMode;
  schedule: Schedule;
  timezone: "local" | "utc";
  options: AutomationOptions;
};

export type RunStatus =
  | "running"
  | "completed"
  | "failed"
  | "cancelled"
  | "timed_out"
  | "needs_approval"
  | "interrupted"
  | "missed"
  | "skipped";

export type AutomationRun = {
  id: string;
  automation_id: string;
  status: RunStatus | string;
  trigger: "schedule" | "catch_up" | "manual" | string;
  scheduled_for?: number | null;
  started_at: number;
  finished_at?: number | null;
  duration?: number | null;
  session_id?: string | null;
  job_id?: string | null;
  summary?: string;
  detail?: string;
  usage?: {
    total_tokens?: number;
    cost_usd?: number | null;
    cost_estimated?: boolean;
  } | null;
  worktree?: {
    id: string;
    path: string;
    branch: string;
    removed?: boolean;
  } | null;
  missed?: number;
};

export type Automation = AutomationDraft & {
  id: string;
  workspace: string;
  paused: boolean;
  next_run_at: number | null;
  created_at: number;
  updated_at: number;
  description: string;
  running_run: string | null;
  last_run: AutomationRun | null;
  runs?: AutomationRun[];
};

export type AutomationList = {
  workspace: string;
  automations: Automation[];
  /** False when no desktop or `shadowcode serve` runs the schedules. */
  scheduler: boolean;
  now: number;
};

export type SchedulePreview =
  | { ok: true; description: string; next: number[]; now: number }
  | { ok: false; error: string };

export const automationsApi = {
  list: () => request<AutomationList>("/api/automations"),
  get: (id: string) => request<Automation>(`/api/automations/${id}`),
  create: (draft: AutomationDraft) =>
    request<Automation>("/api/automations", "POST", draft),
  update: (id: string, draft: AutomationDraft) =>
    request<Automation>(`/api/automations/${id}`, "POST", draft),
  remove: (id: string) =>
    request<{ ok: boolean }>(`/api/automations/${id}`, "DELETE"),
  pause: (id: string) =>
    request<Automation>(`/api/automations/${id}/pause`, "POST", {}),
  resume: (id: string) =>
    request<Automation>(`/api/automations/${id}/resume`, "POST", {}),
  runNow: (id: string) =>
    request<AutomationRun>(`/api/automations/${id}/run`, "POST", {}),
  stop: (id: string) =>
    request<AutomationRun>(`/api/automations/${id}/stop`, "POST", {}),
  preview: (schedule: Schedule, timezone: "local" | "utc") =>
    request<SchedulePreview>("/api/automations/preview", "POST", {
      schedule,
      timezone,
    }),
};

export const DAYS = [
  "Sunday",
  "Monday",
  "Tuesday",
  "Wednesday",
  "Thursday",
  "Friday",
  "Saturday",
];

export function defaultDraft(repo: boolean): AutomationDraft {
  return {
    name: "",
    prompt: "",
    model: "",
    mode: "code",
    schedule: { kind: "weekdays", time: "09:00" },
    timezone: "local",
    options: {
      checkout: repo ? "worktree" : "main",
      permission: "project",
      on_approval: "stop",
      max_runtime_minutes: 60,
      catch_up_minutes: 120,
      notify: true,
    },
  };
}

export function draftOf(a: Automation): AutomationDraft {
  return {
    name: a.name,
    prompt: a.prompt,
    model: a.model,
    mode: a.mode,
    schedule: a.schedule,
    timezone: a.timezone,
    options: a.options,
  };
}

/** Switch schedule kind, keeping the time the user already chose. */
export function withKind(schedule: Schedule, kind: Schedule["kind"]): Schedule {
  const time = "time" in schedule ? schedule.time : "09:00";
  switch (kind) {
    case "hourly":
      return { kind, minute: 0 };
    case "daily":
    case "weekdays":
      return { kind, time };
    case "weekly":
      return { kind, day: 1, time };
    case "cron":
      return { kind, expr: "0 9 * * 1-5" };
  }
}

const pad = (n: number) => String(n).padStart(2, "0");

/** "in 5 min", "Today 09:00", "Tomorrow 09:00", "Mon 28 Sep 09:00" —
 * in the viewer's local time. */
export function formatWhen(ts: number, now: number): string {
  const at = new Date(ts * 1000);
  const current = new Date(now * 1000);
  const minutes = Math.round((ts - now) / 60);
  if (minutes >= 0 && minutes < 60)
    return minutes <= 1 ? "in a minute" : `in ${minutes} min`;
  const clock = `${pad(at.getHours())}:${pad(at.getMinutes())}`;
  const day = (d: Date) =>
    new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime();
  const days = Math.round((day(at) - day(current)) / 86_400_000);
  if (days === 0) return `Today ${clock}`;
  if (days === 1) return `Tomorrow ${clock}`;
  if (days === -1) return `Yesterday ${clock}`;
  const month = at.toLocaleString("en", { month: "short" });
  const weekday = at.toLocaleString("en", { weekday: "short" });
  return `${weekday} ${at.getDate()} ${month} ${clock}`;
}

/** The list's "Next:" line. */
export function nextRunText(a: Automation, now: number): string {
  if (a.running_run) return "Running now";
  if (a.paused) return "Paused";
  if (!a.next_run_at) return "No upcoming time";
  return `Next: ${formatWhen(a.next_run_at, now)}`;
}

export function formatDuration(seconds: number | null | undefined): string {
  if (seconds == null) return "";
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))} s`;
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes} min`;
  return `${Math.floor(minutes / 60)} h ${minutes % 60} min`;
}

export function formatCost(run: AutomationRun): string {
  const usage = run.usage;
  if (!usage) return "";
  if (usage.cost_usd != null)
    return `${usage.cost_estimated ? "about " : ""}$${usage.cost_usd.toFixed(
      usage.cost_usd < 0.01 ? 4 : 2,
    )}`;
  if (usage.total_tokens)
    return `${usage.total_tokens.toLocaleString()} tokens`;
  return "";
}

export const STATUS_TEXT: Record<string, string> = {
  running: "Running",
  completed: "Finished",
  failed: "Failed",
  cancelled: "Stopped",
  timed_out: "Hit its time limit",
  needs_approval: "Stopped for approval",
  interrupted: "Interrupted",
  missed: "Missed",
  skipped: "Skipped",
};

export function runText(run: AutomationRun): string {
  const status = STATUS_TEXT[run.status] || run.status;
  if (run.status === "missed" && (run.missed || 0) > 1)
    return `${status} (${run.missed} times)`;
  return status;
}

export const TRIGGER_TEXT: Record<string, string> = {
  schedule: "On schedule",
  catch_up: "Caught up after start",
  manual: "Run now",
};
