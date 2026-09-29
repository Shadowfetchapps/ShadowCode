import type { RoleId, RolesView, RoleTarget, Usage } from "../api";

/** Roles: which model plans, implements, reviews and explores in a project
 * (GET /api/roles, docs/SUBAGENTS.md). */
export const ROLE_ORDER: RoleId[] = ["plan", "implement", "review", "explore"];

export const ROLE_LABELS: Record<RoleId, string> = {
  plan: "Plan",
  implement: "Implement",
  review: "Review",
  explore: "Explore",
};

export const ROLE_HINTS: Record<RoleId, string> = {
  plan: "Reads the project and writes the plan. Changes nothing.",
  implement:
    "Makes the changes in its own copy of the project. You approve them before they reach your files.",
  review: "Checks the changes before they are applied. Changes nothing.",
  explore:
    "Searches the code when the agent asks a question (the @explore subagent).",
};

export const SKIP = "skip";

export function isRoleId(value: unknown): value is RoleId {
  return ROLE_ORDER.includes(value as RoleId);
}

/** How a role is paid for, as shown on its card. */
export function roleCost(
  cost: string | undefined,
  usage?: Pick<Usage, "cost_usd" | "cost_estimated"> | null,
): string {
  if (cost === "local") return "$0 · local";
  if (cost === "subscription") return "Subscription";
  const usd = usage?.cost_usd;
  if (typeof usd === "number" && Number.isFinite(usd)) {
    const amount =
      usd === 0
        ? "$0"
        : usd < 0.01
          ? `$${usd.toFixed(3)}`
          : `$${usd.toFixed(2)}`;
    return usage?.cost_estimated ? `${amount} est.` : amount;
  }
  return cost === "api" ? "API key · billed per token" : "";
}

/** The model a role runs on, for one line of text. */
export function roleModel(target: RoleTarget | undefined): string {
  if (!target) return "";
  if (target.skipped) return "Skipped";
  return target.name || target.id || "Conversation model";
}

/** "Plan: Claude Code · Implement: Codex · Review: Qwen3 14B". Skipped
 * steps are left out. */
export function pipelineLine(view: RolesView | null | undefined): string {
  if (!view) return "";
  return (["plan", "implement", "review"] as RoleId[])
    .map((role) => view.roles[role])
    .filter((target) => target && !target.skipped)
    .map((target) => `${ROLE_LABELS[target.role]}: ${roleModel(target)}`)
    .join(" · ");
}

/** Why the next task cannot run its roles, or null. */
export function rolesBlocked(
  view: RolesView | null | undefined,
): string | null {
  if (!view) return null;
  for (const role of ["plan", "implement", "review"] as RoleId[]) {
    const target = view.roles[role];
    if (target && !target.skipped && target.blocked) return target.blocked;
  }
  return null;
}

/** Cloud roles this conversation will ask about before they run. */
export function rolesAsking(view: RolesView | null | undefined): string[] {
  if (!view) return [];
  const names = new Set<string>();
  for (const role of ["plan", "implement", "review"] as RoleId[]) {
    const target = view.roles[role];
    if (target && !target.skipped && target.needs_consent)
      names.add(roleModel(target));
  }
  return [...names];
}

export function verdictLabel(verdict: unknown): string {
  if (verdict === "ready") return "Ready to apply";
  if (verdict === "needs_changes") return "Needs changes";
  return "";
}

/** One role of a finished Plan → Implement → Review task (`roles.finished`). */
export type RoleStage = {
  role: RoleId;
  name: string;
  status: string;
  runner: string;
  route: string;
  cost: string;
  usage?: Usage | null;
  files: number;
  additions: number;
  deletions: number;
  verdict?: string;
  skipped?: string;
  error?: string;
  sessionId?: string;
  durationS?: number;
};

export type RolesSummary = {
  stages: RoleStage[];
  /** true applied, false not applied, null nothing to apply. */
  applied: boolean | null;
  note: string;
};

const str = (value: unknown) =>
  typeof value === "string" ? value : value == null ? "" : String(value);

export function parseRolesFinished(
  payload: Record<string, unknown>,
): RolesSummary {
  const stages = (Array.isArray(payload.stages) ? payload.stages : [])
    .map((raw) => (raw || {}) as Record<string, unknown>)
    .filter((raw) => isRoleId(raw.role))
    .map((raw): RoleStage => ({
      role: raw.role as RoleId,
      name: str(raw.name),
      status: str(raw.status) || "failed",
      runner: str(raw.runner),
      route: str(raw.route),
      cost: str(raw.cost),
      usage:
        raw.usage && typeof raw.usage === "object"
          ? (raw.usage as Usage)
          : null,
      files: Number(raw.files) || 0,
      additions: Number(raw.additions) || 0,
      deletions: Number(raw.deletions) || 0,
      verdict: raw.verdict ? str(raw.verdict) : undefined,
      skipped: raw.skipped ? str(raw.skipped) : undefined,
      error: raw.error ? str(raw.error) : undefined,
      sessionId: raw.session_id ? str(raw.session_id) : undefined,
      durationS: raw.duration_s == null ? undefined : Number(raw.duration_s),
    }));
  return {
    stages,
    applied: typeof payload.applied === "boolean" ? payload.applied : null,
    note: str(payload.apply_note),
  };
}

/** "Done", "Skipped", "Stopped", … for a role's status. */
export function stageStatus(stage: Pick<RoleStage, "status" | "skipped">) {
  if (stage.skipped || stage.status === "skipped") return "Skipped";
  switch (stage.status) {
    case "completed":
      return "Done";
    case "cancelled":
      return "Stopped";
    case "interrupted":
      return "Interrupted";
    case "limit_reached":
      return "Plan limit reached";
    case "running":
    case "queued":
      return "Working…";
    default:
      return "Failed";
  }
}
