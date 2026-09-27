import type {
  CommandResult,
  EventRow,
  PlanStep,
  RoutingDecision,
  Usage,
} from "../api";
import type { ChatItem } from "../components/cards";
import {
  addChanged,
  classifyTool,
  emptyActivity,
  parseVerification,
  type TaskActivity,
  type WebSource,
} from "./activity";
import type { UsageSnapshot } from "./picker";
import { CONTINUATION } from "./allowance";
import { applySubagentEvent, isSubagentEvent } from "./subagents";
import { keyRows } from "./rowKeys";
import { localPhase } from "./localProgress";
import { parseLocalRuntimeReceipt } from "./provenance";
import { parseTimings } from "./timing";

const ROUTE_PRODUCTS: Record<string, string> = {
  "cli:codex": "Codex",
  "cli:claude": "Claude Code",
  "cli:cursor": "Cursor",
  "cli:antigravity": "Antigravity",
  "cli:grok": "Grok",
  openrouter: "OpenRouter",
};

export type LimitReached = {
  vendor: string;
  usage?: UsageSnapshot | null;
  taskId?: string;
};

export type Transcript = {
  items: ChatItem[];
  cursor: number;
  stage: string;
  usage: Record<string, number>;
  plan: PlanStep[];
  routing?: RoutingDecision;
  activeTaskId?: string;
  /** Per-task activity derived from recorded events. */
  activity: Record<string, TaskActivity>;
  limit?: LimitReached;
  /** Increments on a vendor's usage.updated so the picker can refresh its rows. */
  usageVersion: number;
  /** The latest automatic continuation on a local model (limit.fallback). */
  fallback?: { jobId: string; target: string; to: string };
  /** The latest context estimate (context.budget, native loop only). */
  budget?: { used: number; limit: number };
  /** The conversation's usage so far (usage.updated `session`). */
  sessionUsage?: Usage;
  /** The latest context compaction (context.compacted). */
  compaction?: {
    ts: number;
    before: number;
    after: number;
    omitted: number;
    method: string;
  };
};
export const emptyTranscript = (): Transcript => ({
  items: [],
  cursor: 0,
  stage: "IDLE",
  usage: {},
  plan: [],
  activity: {},
  usageVersion: 0,
});

const VENDOR_LABELS: Record<string, string> = {
  codex: "Codex",
  claude: "Claude Code",
  cursor: "Cursor",
  antigravity: "Antigravity",
  grok: "Grok",
};

/** "cli:cursor:auto" / "cli-cursor" / "cursor" → "Cursor"; local routes →
 * "this computer". Unknown values are shown as given. */
export function providerLabel(value: unknown): string {
  const text = String(value || "").trim();
  if (!text) return "another model";
  if (/^(local|llamacpp)/.test(text) || text === "this-computer")
    return "this computer";
  const vendor = text
    .replace(/^cli[:-]/, "")
    .split(/[:\s]/)[0]
    .toLowerCase();
  return VENDOR_LABELS[vendor] || text;
}

function lastIndex(items: ChatItem[], test: (item: ChatItem) => boolean) {
  for (let index = items.length - 1; index >= 0; index--)
    if (test(items[index])) return index;
  return -1;
}

/** A prompt bubble. A follow-up written after a plan limit is labelled as
 * such: "manual" when it answers an open "Continue on …" card (which it
 * closes), otherwise "auto" (the engine started it). */
function userItem(
  items: ChatItem[],
  text: string,
  taskId: string,
  eventId?: number,
): { items: ChatItem[]; item: ChatItem } {
  if (!CONTINUATION.test(text))
    return { items, item: { kind: "user", text, taskId, eventId } };
  const ask = lastIndex(
    items,
    (item) => item.kind === "limit" && item.mode === "ask" && !item.resolved,
  );
  if (ask < 0)
    return {
      items,
      item: { kind: "user", text, taskId, eventId, continued: "auto" },
    };
  const next = [...items];
  next[ask] = {
    ...(items[ask] as Extract<ChatItem, { kind: "limit" }>),
    resolved: true,
  };
  return {
    items: next,
    item: { kind: "user", text, taskId, eventId, continued: "manual" },
  };
}

function sourcesFrom(value: unknown): WebSource[] {
  const list = Array.isArray(value) ? value : value ? [value] : [];
  return list
    .map((raw) => (raw || {}) as Record<string, unknown>)
    .filter((s) => typeof s.url === "string" && s.url)
    .map((s) => ({
      url: String(s.url),
      final_url: s.final_url ? String(s.final_url) : undefined,
      title: s.title ? String(s.title) : undefined,
      status: (s.status as number | string | undefined) ?? undefined,
    }));
}

function withSources(activity: TaskActivity, sources: WebSource[]) {
  if (!sources.length) return activity;
  const next = [...activity.sources];
  for (const source of sources)
    if (!next.some((s) => s.url === source.url)) next.push(source);
  return { ...activity, sources: next };
}

/** The event ID is the replay boundary. Native streaming messages and parallel
 * tool calls carry their own IDs; a final message replaces its streamed text. */
export function applyEvent(state: Transcript, event: EventRow): Transcript {
  if (event.id && event.id <= state.cursor) return state;
  const p = event.payload || {};
  let items = state.items;
  let stage = state.stage;
  let usage = state.usage;
  let plan = state.plan;
  let routing = state.routing;
  let activeTaskId = state.activeTaskId;
  let activity = state.activity;
  let limit = state.limit;
  let usageVersion = state.usageVersion;
  let fallback = state.fallback;
  let budget = state.budget;
  let sessionUsage = state.sessionUsage;
  let compaction = state.compaction;
  const taskId = event.task_id || "";
  const text = String(p.text || p.summary || "");
  const touch = (update: (current: TaskActivity) => TaskActivity) => {
    if (!taskId) return;
    activity = {
      ...activity,
      [taskId]: update(activity[taskId] || emptyActivity(taskId)),
    };
  };
  if (isSubagentEvent(event.type)) {
    items = applySubagentEvent(items, event);
  }
  if (event.type === "history.omitted") {
    items = [...items, { kind: "note", taskId, text, warning: true }];
  }
  if (event.type === "hook.started" || event.type === "hook.completed") {
    const callId = `hook-${String(p.id)}`;
    const index = items.findIndex(
      (item) =>
        item.kind === "tool" &&
        item.taskId === taskId &&
        item.callId === callId,
    );
    const previous = index < 0 ? undefined : items[index];
    const process = p.process as {
      stdout?: string;
      stderr?: string;
      truncated?: boolean;
    } | null;
    const card: ChatItem = {
      kind: "tool",
      tool: "hook",
      taskId,
      callId,
      headline: `Hook · ${String(p.name)}`,
      path: String(p.path || ""),
      text:
        event.type === "hook.started"
          ? `Running ${String(p.event)}`
          : `${String(p.status)} · ${String(p.detail || "")}`,
      fullOutput: [
        String(p.event),
        String(p.command || ""),
        String(p.detail || ""),
        process?.stdout || "",
        process?.stderr || "",
        process?.truncated ? "[Output truncated]" : "",
      ]
        .filter(Boolean)
        .join("\n"),
      live: event.type === "hook.started",
      ok: event.type === "hook.completed" ? Boolean(p.success) : undefined,
      collapsed: previous?.kind === "tool" ? previous.collapsed : true,
    };
    items = [...items];
    if (index < 0) items.push(card);
    else items[index] = { ...card, key: items[index].key };
  }
  if (
    event.type === "command.completed" &&
    p.result &&
    typeof p.result === "object"
  ) {
    items = [
      ...items,
      {
        kind: "command",
        card: p.result as CommandResult,
        text: (p.result as CommandResult).body || "",
        taskId,
      },
    ];
  }
  if (
    event.type === "context.budget" ||
    event.type === "autonomy.budget" ||
    event.type === "runaway.warning" ||
    event.type === "context.compacted"
  ) {
    const detail =
      event.type === "runaway.warning"
        ? p.kind === "assistant_text"
          ? `Loop ${String(p.action || "warn")}: assistant text repeated ${String(p.repeats || "")}`
          : p.kind === "prose_command"
            ? `Loop ${String(p.action || "warn")}: model described a command without calling a tool (${String(p.repeats || "")})`
            : `Loop ${String(p.action || "warn")}: ${String(p.tool || "tool")} repeated ${String(p.repeats || "")}`
        : event.type === "autonomy.budget"
          ? `Autonomy budget ${Math.round(Number(p.ratio || 0) * 100)}% of ${String(p.max_steps || "")} steps`
          : event.type === "context.compacted"
            ? `Context compacted; ${String(p.omitted_messages || 0)} earlier messages omitted`
            : `Context nearly full: ${String(p.used_estimated_tokens || 0)}/${String(p.limit || 0)} estimated tokens`;
    // Every model call reports its budget; the status bar shows the level, so
    // only a nearly full context earns a line in the conversation.
    const quietBudget =
      event.type === "context.budget" &&
      Number(p.used_estimated_tokens || 0) < 0.75 * Number(p.limit || Infinity);
    if (!quietBudget)
      items = [...items, { kind: "note", taskId, text: detail }];
  }
  if (event.type === "agent.warning") {
    items = [
      ...items,
      {
        kind: "note",
        taskId,
        warning: true,
        text: String(p.text || p.detail || "Vendor agent warning"),
      },
    ];
  }
  if (event.type === "files.changed") {
    const paths = Array.isArray(p.paths) ? p.paths.map(String) : [];
    touch((a) => ({
      ...a,
      changed: addChanged(a.changed, paths),
      calls: [
        ...a.calls,
        {
          callId: `files-${event.id ?? a.calls.length}`,
          tool: "files.changed",
          step: "editing",
          label: paths.length
            ? `Changed ${paths.join(", ")}`
            : "Reported file changes",
          live: false,
          ok: true,
          output: String(p.detail || ""),
        },
      ],
    }));
  }
  if (event.type === "checkpoint.updated") {
    touch((a) => ({
      ...a,
      checkpointed: true,
      changed: addChanged(a.changed, p.paths),
    }));
  }
  if (event.type === "checkpoint.restored") {
    // The files are back to how they were before this task: the divider
    // goes just above the task's prompt.
    const paths = Array.isArray(p.paths) ? p.paths : [];
    const divider: ChatItem = {
      kind: "divider",
      taskId,
      rewound: true,
      text: `Rewound to here · ${paths.length} file${paths.length === 1 ? "" : "s"} restored`,
    };
    const at = taskId ? items.findIndex((item) => item.taskId === taskId) : -1;
    items =
      at < 0
        ? [...items, divider]
        : [...items.slice(0, at), divider, ...items.slice(at)];
  }
  if (event.type === "checkpoint.rewind_undone") {
    const paths = Array.isArray(p.paths) ? p.paths : [];
    items = [
      ...items,
      {
        kind: "note",
        taskId,
        text: `Rewind undone · ${paths.length} file${paths.length === 1 ? "" : "s"} put back as they were`,
      },
    ];
  }
  if (event.type === "review.undone") {
    items = [
      ...items,
      {
        kind: "note",
        taskId,
        text: p.whole
          ? `Review: undid this task's changes to ${String(p.path)}`
          : `Review: undid one change in ${String(p.path)}`,
      },
    ];
  }
  if (
    event.type === "approval.resolved" &&
    p.approved === false &&
    typeof p.note === "string" &&
    p.note
  ) {
    items = [
      ...items,
      { kind: "note", taskId, text: `Denied with a note: “${p.note}”` },
    ];
  }
  if (event.type === "approval.requested") {
    touch((a) => ({
      ...a,
      approvalsPending: a.approvalsPending + 1,
      approvalsSeen: a.approvalsSeen + 1,
    }));
  }
  if (event.type === "approval.resolved") {
    touch((a) => ({
      ...a,
      approvalsPending: Math.max(0, a.approvalsPending - 1),
    }));
  }
  if (event.type === "web.source") {
    touch((a) => withSources(a, sourcesFrom(p)));
  }
  if (event.type === "verification.summary") {
    const verification = parseVerification(p);
    touch((a) => ({ ...a, verification }));
  }
  if (event.type === "agent.handoff") {
    items = [
      ...items,
      {
        kind: "divider",
        taskId,
        text: `Continued on ${providerLabel(p.to)} · previous context summarized${
          Number(p.excerpt_chars) > 0
            ? ` (${Number(p.excerpt_chars).toLocaleString()} characters)`
            : ""
        }`,
      },
    ];
  }
  if (event.type === "model.switched") {
    items = [
      ...items,
      {
        kind: "note",
        taskId,
        text: `Switched to ${String(p.to || "another model")} on ${providerLabel(p.provider)} · ${p.resumed ? "provider session resumed" : "new provider session"}`,
      },
    ];
  }
  if (event.type === "limit.reached") {
    const vendor = providerLabel(p.vendor);
    limit = {
      vendor,
      usage: (p.usage as UsageSnapshot | undefined) || null,
      taskId,
    };
    items = [
      ...items,
      {
        kind: "note",
        taskId,
        warning: true,
        limitOf: taskId,
        text: `Plan limit reached on ${vendor}. The task paused; choose another model to continue.`,
      },
    ];
  }
  if (event.type === "limit.fallback") {
    // What the engine did about the plan limit replaces the plain note.
    const from = String(
      p.from ||
        activity[taskId]?.finished?.limitReached ||
        (limit?.taskId === taskId ? limit.vendor : "") ||
        "The model",
    );
    items = items.filter(
      (item) => !(item.kind === "note" && item.limitOf === taskId),
    );
    if (p.ok) {
      const to = String(p.to || "a local model");
      const card: ChatItem = {
        kind: "limit",
        taskId,
        mode: "continued",
        text: `${from} reached its plan limit. Continuing on ${to} on this computer.`,
        from,
        to,
        target: String(p.target || ""),
      };
      // The follow-up's prompt is recorded before this event; the note goes
      // just above it.
      const own = lastIndex(items, (item) => item.taskId === taskId);
      const followUp = lastIndex(
        items,
        (item) =>
          item.kind === "user" &&
          item.taskId !== taskId &&
          CONTINUATION.test(item.text),
      );
      items = [...items];
      if (followUp > own) {
        items[followUp] = {
          ...(items[followUp] as Extract<ChatItem, { kind: "user" }>),
          continued: "auto",
        };
        items.splice(followUp, 0, card);
      } else items.push(card);
      if (p.target)
        fallback = {
          jobId: String(p.job_id || ""),
          target: String(p.target),
          to: String(p.to || ""),
        };
      limit = undefined;
    } else if (p.ask) {
      const request = items.find(
        (item) => item.kind === "user" && item.taskId === taskId,
      );
      items = [
        ...items,
        {
          kind: "limit",
          taskId,
          mode: "ask",
          text: `${from} reached its plan limit.`,
          from,
          request: request?.text || "",
        },
      ];
      limit = undefined;
    } else {
      const reason = String(
        p.reason || "No local model could continue this conversation.",
      );
      items = [
        ...items,
        {
          kind: "limit",
          taskId,
          mode: "unavailable",
          text: `${from} reached its plan limit. ${reason}`,
          from,
          reason,
        },
      ];
    }
  }
  // Vendor account pushes carry `vendor`; per-task token/cost updates
  // (`turn`/`job`/`session`) do not change the picker's rows.
  if (event.type === "usage.updated" && p.vendor) usageVersion += 1;
  // Per-task accounting (no `vendor`): the conversation's running total.
  if (event.type === "usage.updated" && !p.vendor && p.session)
    sessionUsage = p.session as Usage;
  if (event.type === "context.budget" && Number(p.limit) > 0)
    budget = {
      used: Number(p.used_estimated_tokens || 0),
      limit: Number(p.limit),
    };
  if (event.type === "context.compacted") {
    compaction = {
      ts: event.ts,
      before: Number(p.before_estimated_tokens || 0),
      after: Number(p.after_estimated_tokens || 0),
      omitted: Number(p.omitted_messages || 0),
      method: String(p.method || ""),
    };
    if (budget && compaction.after)
      budget = { ...budget, used: compaction.after };
  }
  if (event.type === "workflow.selected") {
    items = [
      ...items,
      {
        kind: "note",
        taskId,
        text: `Workflow /${String(p.name || "workflow")} · ${String(p.path || "project")} · ${String(p.effective_mode || p.mode || "current task mode")}`,
      },
    ];
  }
  if (event.type === "user.message") {
    const next = userItem(items, text, taskId, event.id);
    items = [...next.items, next.item];
    if (!activeTaskId) stage = "QUEUED";
  }
  if (event.type === "agent.started") {
    const pending = taskId
      ? items.find((item) => item.kind === "user" && item.taskId === taskId)
      : undefined;
    const created = pending
      ? { items, item: pending }
      : userItem(items, String(p.task || ""), taskId, event.id);
    // A "Continue on …" card is only an offer until other work starts.
    items = [
      ...created.items
        .filter((item) => item !== pending)
        .map((item) =>
          item.kind === "limit" &&
          item.mode === "ask" &&
          !item.resolved &&
          item.taskId !== taskId
            ? { ...item, resolved: true }
            : item,
        ),
      created.item,
    ];
    activeTaskId = taskId;
    stage = "UNDERSTAND";
    plan = [];
    usage = {};
    routing = undefined;
    limit = undefined;
    touch((a) => ({ ...a, startedAt: a.startedAt ?? event.ts }));
  }
  if (
    event.type === "local.runtime_progress" ||
    event.type === "local.runtime_ready"
  ) {
    const phase = localPhase(p.phase);
    if (
      taskId &&
      !activity[taskId]?.finished &&
      (phase || event.type === "local.runtime_ready")
    ) {
      touch((a) => ({
        ...a,
        localPhase: phase,
        localRuntime:
          event.type === "local.runtime_ready"
            ? parseLocalRuntimeReceipt(p)
            : a.localRuntime,
        startedAt: a.startedAt ?? event.ts,
      }));
      // Local preparation precedes agent.started. Keep it on its own task;
      // a background task cannot replace another task's active timeline.
      if (!activeTaskId || activeTaskId === taskId) {
        activeTaskId = taskId;
        stage = phase ? "PREPARING" : "UNDERSTAND";
      }
    }
  }
  if (event.type === "routing.selected" || event.type === "routing.fallback") {
    if (p.model_id && p.model_name && p.provider)
      routing = p as unknown as RoutingDecision;
    const raw = String(
      p.model_name || p.model_id || p.fallback || "configured model",
    );
    // Subscription and API rows name the product, as the picker does:
    // "Cursor · Auto", not a bare "auto".
    const product = ROUTE_PRODUCTS[String(p.provider || "")];
    const modelLabel =
      raw === "auto" ? "Auto" : raw === "default" ? "Default" : raw;
    const name = product ? `${product} · ${modelLabel}` : raw;
    // With `inference` the row name says enough; older records also name
    // the provider and purpose.
    const selected = p.inference
      ? name
      : [name, p.provider, p.purpose].filter(Boolean).map(String).join(" · ");
    const warning = event.type === "routing.fallback";
    const where =
      p.inference === "local"
        ? name.includes("This computer")
          ? ""
          : " · This computer"
        : p.inference === "cloud"
          ? " · Cloud"
          : "";
    const note = warning
      ? `Using default: ${selected}${where}. ${String(p.fallback_reason || "The saved model is unavailable.")}`
      : `Using ${selected}${where}`;
    // Say which model ran once, and again only when it changes: the picker
    // already names the conversation's model.
    const previous = [...items]
      .reverse()
      .find((item) => item.kind === "note" && item.text.startsWith("Using "));
    if (warning || previous?.text !== note)
      items = [...items, { kind: "note", taskId, warning, text: note }];
  }
  if (
    ["model.stream", "model.delta", "model.stream_end"].includes(event.type)
  ) {
    const messageId = String(p.message_id || "");
    const index = messageId
      ? items.findIndex(
          (item) =>
            item.kind === "agent" &&
            item.messageId === messageId &&
            item.taskId === taskId,
        )
      : -1;
    const previous = index < 0 ? undefined : items[index];
    if (event.type === "model.stream_end") {
      if (previous?.kind === "agent") {
        items = [...items];
        items[index] = {
          ...previous,
          live: false,
          // The partial text stays as it was; the summary card says the
          // task was stopped, so no extra label is needed here.
        };
      }
    } else if (text) {
      const next: ChatItem = {
        kind: "agent",
        taskId,
        messageId: messageId || undefined,
        text:
          event.type === "model.stream" && previous?.kind === "agent"
            ? previous.text + text
            : text,
        live: event.type === "model.stream" || p.complete === false,
        eventId:
          event.type === "model.delta" && p.complete !== false
            ? event.id
            : undefined,
      };
      items = [...items];
      if (index < 0) items.push(next);
      else items[index] = { ...next, key: items[index].key };
    }
  }
  if (event.type === "tool.started") {
    const args = (p.arguments || {}) as Record<string, unknown>;
    const command = args.command;
    touch((a) => ({
      ...a,
      calls: [
        ...a.calls,
        {
          callId: String(p.call_id || `call-${a.calls.length}`),
          tool: String(p.tool),
          step: classifyTool(String(p.tool), args),
          label: String(p.headline || p.tool),
          live: true,
          path: typeof args.path === "string" ? args.path : undefined,
          command: Array.isArray(command)
            ? command.map(String).join(" ")
            : typeof command === "string"
              ? command
              : undefined,
        },
      ],
    }));
    items = [
      ...items,
      {
        kind: "tool",
        tool: String(p.tool),
        text: "",
        live: true,
        collapsed: true,
        taskId,
        callId: String(p.call_id || ""),
        headline: String(p.tool),
        path: String(
          (p.arguments as Record<string, unknown> | undefined)?.path || "",
        ),
      },
    ];
  }
  if (event.type === "tool.completed") {
    const index = items.findIndex(
      (i) =>
        i.kind === "tool" &&
        i.live &&
        i.taskId === taskId &&
        (p.call_id ? i.callId === p.call_id : i.tool === p.tool),
    );
    const args = p.arguments as Record<string, unknown> | undefined;
    const previous = index >= 0 ? items[index] : undefined;
    const card: ChatItem = {
      kind: "tool",
      tool: String(p.tool),
      text: String(p.output_preview || p.error || ""),
      fullOutput: String(
        p.output_full ||
          (p.output ? JSON.stringify(p.output, null, 2) : "") ||
          p.output_preview ||
          p.error ||
          "",
      ),
      live: false,
      ok: Boolean(p.success),
      collapsed: previous?.kind === "tool" ? previous.collapsed : true,
      headline: String(p.headline || p.tool),
      icon: String(p.icon || ""),
      taskId,
      callId: String(p.call_id || ""),
      path: String(
        args?.path ||
          args?.dest ||
          (previous?.kind === "tool" ? previous.path : "") ||
          "",
      ),
    };
    items = [...items];
    if (index < 0) items.push(card);
    else items[index] = { ...card, key: items[index].key };
    const completedArgs = (args || {}) as Record<string, unknown>;
    touch((a) => {
      const at = a.calls.findIndex(
        (call) =>
          call.live &&
          (p.call_id
            ? call.callId === String(p.call_id)
            : call.tool === p.tool),
      );
      const previousCall = at >= 0 ? a.calls[at] : undefined;
      const step =
        previousCall?.step ?? classifyTool(String(p.tool), completedArgs);
      const path =
        (card.kind === "tool" && card.path) || previousCall?.path || undefined;
      const call = {
        callId: String(
          p.call_id || previousCall?.callId || `call-${a.calls.length}`,
        ),
        tool: String(p.tool),
        step,
        label: String(p.headline || previousCall?.label || p.tool),
        live: false,
        ok: Boolean(p.success),
        output: card.kind === "tool" ? card.fullOutput || card.text : "",
        path,
        command: previousCall?.command,
      };
      const calls = [...a.calls];
      if (at >= 0) calls[at] = call;
      else calls.push(call);
      const outputPaths =
        p.output && typeof p.output === "object"
          ? (p.output as Record<string, unknown>).paths
          : undefined;
      return withSources(
        {
          ...a,
          calls,
          changed:
            step === "editing" && p.success
              ? addChanged(a.changed, outputPaths || path)
              : a.changed,
        },
        sourcesFrom(p.sources),
      );
    });
  }
  if (p.stage && (!activeTaskId || activeTaskId === taskId))
    stage = String(p.stage);
  if (p.plan && (!activeTaskId || activeTaskId === taskId))
    plan = (p.plan as { steps?: PlanStep[] }).steps || plan;
  if (event.type === "agent.completed") {
    const limitInfo = p.limit_reached as { vendor?: string } | undefined;
    const limitReached =
      !p.success && !p.cancelled && limitInfo
        ? providerLabel(limitInfo.vendor)
        : undefined;
    // Completion checks can appear after the final model response. Match the
    // latest answer within this task, rather than whichever card is last.
    let last: ChatItem | undefined;
    for (let index = items.length - 1; index >= 0; index--) {
      if (items[index].kind === "agent" && items[index].taskId === taskId) {
        last = items[index];
        break;
      }
    }
    // A task the user stopped is summarised by its card; the engine's
    // "cancelled" text would only repeat that.
    if (
      text &&
      !p.cancelled &&
      !(
        // The streamed reply already says this; older vendor turns never
        // marked their stream finished, so a live item counts too.
        last?.kind === "agent" &&
        last.text.trim() === text.trim() &&
        p.success &&
        !p.cancelled
      )
    )
      items = [
        ...items,
        {
          kind: "agent",
          taskId,
          text,
          who: p.cancelled
            ? "Stopped"
            : p.success
              ? "Result"
              : limitReached
                ? "Plan limit reached"
                : "Needs attention",
        },
      ];
    let forkIndex = -1;
    for (let i = items.length - 1; i >= 0; i--) {
      if (items[i].kind === "agent" && items[i].taskId === taskId) {
        forkIndex = i;
        break;
      }
    }
    if (forkIndex >= 0) {
      items = [...items];
      const item = items[forkIndex];
      if (item.kind === "agent")
        items[forkIndex] = { ...item, eventId: event.id, live: false };
    }
    const verification = parseVerification(p.verification);
    touch((a) => ({
      ...a,
      finishedAt: event.ts,
      timings: parseTimings(p.timings),
      verification: verification || a.verification,
      calls: a.calls.map((call) =>
        call.live ? { ...call, live: false } : call,
      ),
      approvalsPending: 0,
      finished: {
        success: Boolean(p.success),
        cancelled: Boolean(p.cancelled),
        summary: text,
        ...(limitReached ? { limitReached } : {}),
      },
    }));
    if (
      taskId &&
      !items.some((item) => item.kind === "summary" && item.taskId === taskId)
    )
      items = [...items, { kind: "summary", taskId, text }];
    if (!activeTaskId || activeTaskId === taskId) {
      stage = p.cancelled ? "CANCELLED" : p.success ? "DONE" : "FAILED";
      usage = (p.usage as Record<string, number>) || {};
      activeTaskId = undefined;
    }
  }
  return {
    items: keyRows(state.items, items, event.id),
    stage,
    usage,
    plan,
    routing,
    activeTaskId,
    activity,
    limit,
    usageVersion,
    fallback,
    budget,
    sessionUsage,
    compaction,
    cursor: event.id || state.cursor,
  };
}

export function replay(events: EventRow[]): Transcript {
  return events.reduce(applyEvent, emptyTranscript());
}
