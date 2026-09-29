import type { EventRow, RoleId, Usage } from "../api";
import type { ChatItem } from "../components/cards";
import { isRoleId } from "./roles";

/** One changed file of a write subagent (its diffstat). */
export type SubagentFile = {
  path: string;
  status: string;
  additions: number;
  deletions: number;
  binary?: boolean;
};

/** A subagent run as the parent conversation shows it, built from the
 * `subagent.started` / `subagent.finished` / `subagent.applied` events. */
export type SubagentRun = {
  runId: string;
  agent: string;
  description: string;
  prompt: string;
  mode: string;
  model: string;
  status: string;
  summary: string;
  error?: string;
  jobId: string;
  sessionId: string;
  files: SubagentFile[];
  filesTruncated: boolean;
  binaryFiles: string[];
  patch: boolean;
  applied: boolean;
  notes: string[];
  steps: number;
  tokens: number;
  durationS?: number;
  /** The role this run played, when the project's roles chose its model. */
  role?: RoleId;
  /** `vendor` (a vendor CLI) or `shadowcode` (ShadowCode's own loop). */
  runner?: string;
  /** `local` or `cloud`. */
  route?: string;
  /** `local`, `subscription` or `api`. */
  cost?: string;
  usage?: Usage | null;
  /** A review role's verdict: `ready` or `needs_changes`. */
  verdict?: string;
};

const str = (value: unknown) =>
  typeof value === "string" ? value : value == null ? "" : String(value);
const list = (value: unknown) => (Array.isArray(value) ? value : []);

function files(value: unknown): SubagentFile[] {
  return list(value).map((raw) => {
    const f = (raw || {}) as Record<string, unknown>;
    return {
      path: str(f.path),
      status: str(f.status) || "modified",
      additions: Number(f.additions) || 0,
      deletions: Number(f.deletions) || 0,
      binary: Boolean(f.binary),
    };
  });
}

export function isSubagentEvent(type: string) {
  return (
    type === "subagent.started" ||
    type === "subagent.finished" ||
    type === "subagent.applied"
  );
}

/** Add or update the run card an event belongs to. */
export function applySubagentEvent(
  items: ChatItem[],
  event: EventRow,
): ChatItem[] {
  const p = event.payload || {};
  const runId = str(p.run_id);
  if (!runId) return items;
  const index = items.findIndex(
    (item) => item.kind === "subagent" && item.run.runId === runId,
  );
  const previous =
    index >= 0 && items[index].kind === "subagent"
      ? (items[index] as Extract<ChatItem, { kind: "subagent" }>).run
      : undefined;
  const base: SubagentRun = previous || {
    runId,
    agent: str(p.agent),
    description: str(p.description),
    prompt: str(p.prompt),
    mode: str(p.mode),
    model: str(p.model),
    status: "running",
    summary: "",
    jobId: str(p.job_id),
    sessionId: str(p.session_id),
    files: [],
    filesTruncated: false,
    binaryFiles: [],
    patch: false,
    applied: false,
    notes: [],
    steps: 0,
    tokens: 0,
    role: isRoleId(p.role) ? p.role : undefined,
    runner: str(p.runner) || undefined,
    route: str(p.route) || undefined,
    cost: str(p.cost) || undefined,
  };
  let run = base;
  if (event.type === "subagent.finished") {
    const usage = (p.usage || {}) as Record<string, unknown>;
    run = {
      ...base,
      agent: str(p.agent) || base.agent,
      description: str(p.description) || base.description,
      mode: str(p.mode) || base.mode,
      model: str(p.model) || base.model,
      status: str(p.status) || "failed",
      summary: str(p.summary),
      error: p.error ? str(p.error) : undefined,
      jobId: str(p.job_id) || base.jobId,
      sessionId: str(p.session_id) || base.sessionId,
      files: files(p.files),
      filesTruncated: Boolean(p.files_truncated),
      binaryFiles: list(p.binary_files).map(str),
      patch: Boolean(p.patch),
      notes: list(p.notes).map(str),
      steps: Number(p.steps) || 0,
      tokens: Number(usage.total_tokens) || 0,
      durationS: p.duration_s == null ? undefined : Number(p.duration_s),
      role: isRoleId(p.role) ? p.role : base.role,
      runner: str(p.runner) || base.runner,
      route: str(p.route) || base.route,
      cost: str(p.cost) || base.cost,
      usage: p.usage && typeof p.usage === "object" ? (p.usage as Usage) : null,
      verdict: p.verdict ? str(p.verdict) : undefined,
    };
  } else if (event.type === "subagent.applied") {
    run = { ...base, applied: true };
  } else if (previous) {
    return items;
  }
  const item: ChatItem = {
    // An updated card keeps its row key, so it stays open while it updates.
    ...(index >= 0 && items[index].key ? { key: items[index].key } : {}),
    kind: "subagent",
    taskId: event.task_id || undefined,
    text: `${run.role ? `${run.role} role` : `@${run.agent}`}: ${run.summary || run.description || run.prompt}`,
    run,
  };
  const next = [...items];
  if (index < 0) next.push(item);
  else next[index] = item;
  return next;
}

export function statusLabel(run: SubagentRun) {
  switch (run.status) {
    case "running":
    case "queued":
      return "Working…";
    case "completed":
      return "Done";
    case "cancelled":
      return "Stopped";
    case "interrupted":
      return "Interrupted";
    case "limit_reached":
      return "Plan limit reached";
    default:
      return "Failed";
  }
}
