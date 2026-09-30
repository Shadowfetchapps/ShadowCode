import type { RolesSummary } from "./roles";
/** Activity derived only from recorded events: tool calls (native and vendor),
 * approvals, web sources, changed files, verification and completion. */
import { localProgressLabel, type LocalPhase } from "./localProgress";
import type { LocalRuntimeReceipt, RunRecord, TaskTimings } from "../api";

export type StepId =
  | "local"
  | "reading"
  | "editing"
  | "commands"
  | "testing"
  | "web"
  | "waiting"
  | "finished";

export type ActivityCall = {
  callId: string;
  completedEventId?: number;
  tool: string;
  step: StepId;
  label: string;
  live: boolean;
  ok?: boolean;
  output?: string;
  path?: string;
  command?: string;
};

export type WebSource = {
  url: string;
  final_url?: string;
  title?: string;
  status?: number | string;
};

export type VerificationCommand = {
  command: string;
  exit_code: number | null;
  success: boolean;
  timed_out?: boolean;
  kind?: string;
  state?: string;
  callId?: string;
  outputRef?: string;
  attemptId?: string;
  cwd?: string;
  fingerprint?: string;
  provenance?: string;
  scope?: string;
};

export type Verification = {
  status: string;
  commands: VerificationCommand[];
  presentedAs?: string;
  finalAssessment?: string;
  note?: string;
  vendor?: string;
};

export type TaskActivity = {
  taskId: string;
  startedAt?: number;
  localPhase?: LocalPhase;
  timings?: TaskTimings;
  localRuntime?: LocalRuntimeReceipt;
  finishedAt?: number;
  calls: ActivityCall[];
  sources: WebSource[];
  changed: string[];
  /** A file checkpoint was recorded for this task (file tools, shell
   * commands or a subscription turn), so Rewind can restore it. */
  checkpointed?: boolean;
  approvalsPending: number;
  approvalsSeen: number;
  verification?: Verification;
  /** A Plan → Implement → Review task: who did what (`roles.finished`). */
  roles?: RolesSummary;
  finished?: {
    success: boolean;
    cancelled: boolean;
    summary: string;
    /** The subscription that reported its plan limit ("Codex"). */
    limitReached?: string;
  };
  /** What exactly ran the task (agent.completed `run`). */
  run?: RunRecord;
  /** The plan limit this task stopped at (limit.reached): its job and when
   * the plan resets, if the vendor said. */
  limit?: { jobId?: string; resetsAt?: number };
  /** The prompt's @-mentions and "Only change these" (user.message) and
   * the task's mode (agent.started): Try on… continues with them. */
  mentions?: { path: string; kind: "file" | "dir" }[];
  onlyChange?: boolean;
  mode?: string;
};

export const emptyActivity = (taskId: string): TaskActivity => ({
  taskId,
  calls: [],
  sources: [],
  changed: [],
  approvalsPending: 0,
  approvalsSeen: 0,
});

const READ = new Set([
  "read_file",
  "list_files",
  "list_dir",
  "git_status",
  "git_diff",
  "git_log",
  "view_file",
  "view_image",
  "read",
  "glob",
  "grep",
  "ls",
  "search",
  "grep_search",
  "find_by_name",
  "codebase_search",
  "project_map",
  "symbols",
]);
const EDIT = new Set([
  "write_file",
  "edit_file",
  "apply_patch",
  "create_directory",
  "move_file",
  "delete_file",
  "file_change",
  "edit",
  "write",
  "multiedit",
  "notebookedit",
  "delete",
  "move",
  "write_to_file",
  "replace_file_content",
  "multi_replace_file_content",
]);
const EXEC = new Set([
  "exec",
  "command_execution",
  "bash",
  "execute",
  "run_command",
  "shell",
  "background_start",
]);
const WEB = new Set([
  "web_fetch",
  "web_search",
  "webfetch",
  "websearch",
  "fetch",
  "read_url_content",
  "search_web",
]);

/** "codex.command_execution" → "command_execution"; "cursor.read" → "read";
 * "codex.mcp:server/tool" → "tool". */
export function baseToolName(tool: string): string {
  const lower = tool.toLowerCase();
  const tail = lower.split(/[.:/]/).pop() || lower;
  return tail;
}

/** Test, build, lint and type-check runners (mirrors hooks::is_test, plus
 * common build/lint commands). */
export function isVerificationCommand(command: string): boolean {
  const words = command
    .split(/[\s;&|"']+/)
    .filter(Boolean)
    .map((w) => w.replace(/^.*\//, ""));
  const single = new Set([
    "pytest",
    "pytest-3",
    "unittest",
    "vitest",
    "jest",
    "ctest",
    "tsc",
    "eslint",
    "ruff",
    "mypy",
    "flake8",
    "pylint",
    "clippy",
    "cargo-clippy",
    "shellcheck",
    "phpunit",
    "rspec",
    "playwright",
  ]);
  if (words.some((w) => single.has(w))) return true;
  for (let i = 0; i < words.length - 1; i++) {
    const [a, b] = [words[i], words[i + 1]];
    if (
      [
        "cargo",
        "go",
        "dotnet",
        "mvn",
        "gradle",
        "./gradlew",
        "swift",
        "zig",
        "mix",
      ].includes(a) &&
      ["test", "build", "check", "clippy", "vet", "fmt", "lint"].includes(b)
    )
      return true;
    if (["npm", "pnpm", "yarn", "bun", "npx"].includes(a)) {
      if (["test", "t"].includes(b)) return true;
      const rest = words.slice(i + 1);
      const run = rest.indexOf("run");
      const script = run >= 0 ? rest[run + 1] || "" : b;
      if (/^(test|build|lint|typecheck|check|e2e)(:|$)/.test(script))
        return true;
    }
    if (a === "make" && /^(test|check|build|lint)$/.test(b)) return true;
    if (a === "python" || a === "python3") {
      if (
        b === "-m" &&
        ["pytest", "unittest", "mypy"].includes(words[i + 2] || "")
      )
        return true;
    }
  }
  return false;
}

function commandOf(args: Record<string, unknown> | undefined): string {
  if (!args) return "";
  const value =
    args.command ??
    args.cmd ??
    (args.input as Record<string, unknown> | undefined)?.command;
  if (Array.isArray(value)) return value.map(String).join(" ");
  return typeof value === "string" ? value : "";
}

export function classifyTool(
  tool: string,
  args?: Record<string, unknown>,
): StepId {
  const base = baseToolName(tool);
  if (EDIT.has(base)) return "editing";
  if (WEB.has(base) || base.startsWith("web_")) return "web";
  if (EXEC.has(base)) {
    const command = commandOf(args);
    return command && isVerificationCommand(command) ? "testing" : "commands";
  }
  if (READ.has(base) || base.startsWith("search_") || base.startsWith("read_"))
    return "reading";
  return "commands";
}

const LABELS: Record<StepId, string> = {
  local: "Preparing local model",
  reading: "Reading project",
  editing: "Editing files",
  commands: "Running commands",
  testing: "Running checks",
  web: "Looking up the web",
  waiting: "Waiting for approval",
  finished: "Finished",
};
const ORDER: StepId[] = [
  "reading",
  "web",
  "editing",
  "commands",
  "testing",
  "waiting",
  "finished",
];

export type TimelineStep = {
  id: StepId;
  label: string;
  state: "active" | "done" | "failed" | "incomplete";
  calls: ActivityCall[];
  detail?: string;
};

function checkState(
  verification: Verification | undefined,
  failedCall: boolean,
): TimelineStep["state"] {
  if (
    verification?.finalAssessment === "not_completed" ||
    verification?.finalAssessment === "interrupted"
  )
    return "incomplete";
  const hasChecks = verification?.commands.some(
    (command) => command.kind === "configured_check",
  );
  // The engine's final verdict includes reruns and workspace freshness. A
  // prior failed attempt must not override a later authoritative pass.
  if (hasChecks && verification?.status === "passed") return "done";
  if (
    verification &&
    [
      "cancelled",
      "stale",
      "skipped",
      "incomplete",
      "unavailable",
      "checking",
    ].includes(verification.status)
  )
    return "incomplete";
  if (failedCall || (hasChecks && verification?.status === "failed"))
    return "failed";
  // A successful tool process or provider report is not a configured-check
  // verdict. Keep those facts in the expanded calls without a green check.
  return "incomplete";
}

function checkLabel(
  state: TimelineStep["state"],
  verification: Verification | undefined,
): string {
  if (state === "active") return LABELS.testing;
  if (state === "done") return "Checks passed";
  if (state === "failed") return "Checks failed";
  if (
    verification?.finalAssessment === "not_completed" ||
    verification?.finalAssessment === "interrupted"
  )
    return "Checks incomplete";
  if (verification?.status === "cancelled") return "Checks cancelled";
  if (verification?.status === "stale") return "Checks stale";
  if (verification?.status === "unavailable") return "Checks unavailable";
  if (verification?.status === "vendor_owned") return "Vendor checks";
  if (verification?.status === "not_run") return "Checks not run";
  if (!verification?.commands.some((c) => c.kind === "configured_check"))
    return "Checks not verified";
  return "Checks incomplete";
}

/** Only steps with real evidence appear. A step is active while one of its
 * calls is running (or an approval is pending); Finished appears only after
 * agent.completed. */
export function deriveSteps(
  activity: TaskActivity | undefined,
  pendingApprovals = 0,
): TimelineStep[] {
  if (!activity) return [];
  const steps: TimelineStep[] = [];
  if (
    activity.localPhase &&
    !activity.finished &&
    Math.max(pendingApprovals, activity.approvalsPending) === 0
  )
    steps.push({
      id: "local",
      label: localProgressLabel(activity.localPhase),
      state: "active",
      calls: [],
    });
  for (const id of ORDER) {
    if (id === "waiting") {
      // A state, not work done: shown only while a request waits.
      const waiting = Math.max(pendingApprovals, activity.approvalsPending);
      if (waiting > 0 && !activity.finished)
        steps.push({
          id,
          label: LABELS[id],
          state: "active",
          calls: [],
          detail: `${waiting} request${waiting === 1 ? "" : "s"} waiting`,
        });
      continue;
    }
    if (id === "finished") {
      if (activity.finished)
        steps.push({
          id,
          label: activity.finished.cancelled
            ? "Stopped"
            : activity.finished.success
              ? LABELS.finished
              : activity.finished.limitReached
                ? "Plan limit reached"
                : "Finished with problems",
          state: activity.finished.success ? "done" : "failed",
          calls: [],
        });
      continue;
    }
    // A command the harness recorded as a check is listed once, under
    // checks, not again under commands.
    const checks = new Set(
      (activity.verification?.commands || [])
        .filter((c) => c.kind === "configured_check")
        .map((c) => c.command.trim()),
    );
    const isCheck = (call: ActivityCall) =>
      call.step === "commands" &&
      Boolean(call.command) &&
      checks.has(call.command!.trim());
    const calls = activity.calls.filter((call) =>
      id === "testing"
        ? call.step === id || isCheck(call)
        : call.step === id && !isCheck(call),
    );
    // Harness verification commands and web.source events are evidence even
    // without a matching tool call in this list.
    const evidence =
      calls.length > 0 ||
      (id === "testing" &&
        Boolean(
          activity.verification?.commands.some(
            (c) => c.kind === "configured_check",
          ),
        )) ||
      (id === "web" && activity.sources.length > 0);
    if (!evidence) continue;
    const live =
      !activity.finished &&
      (calls.some((call) => call.live) ||
        (id === "testing" && activity.verification?.status === "checking"));
    const failed = calls.some((call) => call.ok === false);
    const state: TimelineStep["state"] = live
      ? "active"
      : id === "testing"
        ? checkState(activity.verification, failed)
        : failed
          ? "failed"
          : calls.some((call) => call.ok !== true)
            ? "incomplete"
            : "done";
    steps.push({
      id,
      label:
        id === "testing"
          ? checkLabel(state, activity.verification)
          : LABELS[id],
      state,
      calls,
      detail:
        id === "testing"
          ? verificationLine(activity.verification) ||
            (failed ? "A check failed" : undefined)
          : id === "web" && activity.sources.length
            ? `${activity.sources.length} source${activity.sources.length === 1 ? "" : "s"}`
            : undefined,
    });
  }
  return steps;
}

export function verificationLine(
  verification: Verification | undefined,
): string | undefined {
  if (!verification) return undefined;
  if (verification.status === "checking") return "Assessing current files…";
  if (verification.status === "unavailable")
    return "Current verification unavailable";
  if (verification.status === "vendor_owned")
    return "Checks are run and judged by the vendor agent";
  const checks = verification.commands.filter(
    (c) => c.kind === "configured_check",
  );
  if (!checks.length) return "Verification not run";
  if (
    verification.finalAssessment === "not_completed" ||
    verification.finalAssessment === "interrupted"
  )
    return "Final verification did not finish";
  if (verification.status === "stale")
    return "Checks are stale — files changed";
  if (verification.status === "cancelled") return "Verification cancelled";
  if (verification.status === "skipped" || verification.status === "incomplete")
    return "Verification incomplete";
  if (verification.status === "passed") return "Configured checks passed";
  return "Configured checks did not pass";
}

export function parseVerification(value: unknown): Verification | undefined {
  if (!value || typeof value !== "object") return undefined;
  const v = value as Record<string, unknown>;
  const commands = Array.isArray(v.commands)
    ? v.commands.map((raw) => {
        const c = (raw || {}) as Record<string, unknown>;
        const exit =
          typeof c.exit_code === "number"
            ? c.exit_code
            : c.exit_code == null
              ? null
              : Number(c.exit_code);
        return {
          command: Array.isArray(c.command)
            ? c.command.map(String).join(" ")
            : String(c.command ?? ""),
          exit_code: Number.isFinite(exit as number) ? (exit as number) : null,
          success: c.success === true || (c.success == null && exit === 0),
          timed_out: c.timed_out === true,
          kind: typeof c.kind === "string" ? c.kind : "command",
          state: typeof c.state === "string" ? c.state : undefined,
          callId:
            typeof c.tool_call_id === "string" ? c.tool_call_id : undefined,
          // Invalid explicit references must not use the legacy ID fallback.
          outputRef:
            c.output_ref === undefined
              ? undefined
              : typeof c.output_ref === "string"
                ? c.output_ref
                : "",
          attemptId:
            typeof c.attempt_id === "string" ? c.attempt_id : undefined,
          cwd: typeof c.cwd === "string" ? c.cwd : undefined,
          fingerprint:
            typeof c.workspace_fingerprint === "string"
              ? c.workspace_fingerprint
              : undefined,
          provenance:
            typeof c.provenance === "string" ? c.provenance : undefined,
          scope: typeof c.scope === "string" ? c.scope : undefined,
        };
      })
    : [];
  return {
    status: String(v.status || (commands.length ? "ran" : "not_run")),
    commands,
    presentedAs:
      typeof v.presented_as === "string" ? v.presented_as : undefined,
    finalAssessment:
      typeof v.final_assessment === "string" ? v.final_assessment : undefined,
    note: typeof v.note === "string" ? v.note : undefined,
    vendor: typeof v.vendor_agent === "string" ? v.vendor_agent : undefined,
  };
}

/** Redaction placeholders cannot identify a tool execution. */
export function usableCallId(value: unknown): value is string {
  return (
    typeof value === "string" &&
    value.length > 0 &&
    !value.includes("[redacted")
  );
}

/** Resolve within this task only. An explicit reference never falls back to
 * a potentially reused provider ID, even when its event is unavailable. */
export function receiptOutput(
  activity: TaskActivity,
  receipt: VerificationCommand,
): string | undefined {
  let matches: ActivityCall[];
  if (receipt.outputRef !== undefined) {
    const match = /^event:([1-9][0-9]*)$/.exec(receipt.outputRef);
    const id = match ? Number(match[1]) : NaN;
    if (!Number.isSafeInteger(id)) return undefined;
    matches = activity.calls.filter((call) => call.completedEventId === id);
  } else {
    if (!usableCallId(receipt.callId)) return undefined;
    matches = activity.calls.filter((call) => call.callId === receipt.callId);
  }
  return matches.length === 1 && !matches[0].live && matches[0].tool === "exec"
    ? matches[0].output
    : undefined;
}

export function addChanged(list: string[], paths: unknown): string[] {
  const next = [...list];
  const values = Array.isArray(paths) ? paths : paths ? [paths] : [];
  for (const raw of values) {
    const path = String(raw || "").trim();
    if (path && !next.includes(path)) next.push(path);
  }
  return next;
}

export function formatDuration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) return `${s}s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m}m ${s % 60}s`;
  return `${Math.floor(m / 60)}h ${m % 60}m`;
}
