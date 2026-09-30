import { memo, useState } from "react";
import type { CommandResult } from "../api";
import type { SubagentRun } from "../lib/subagents";
import type { TaskMode } from "../lib/effort";
import { readableError } from "../lib/transport";
import { Markdown } from "./Markdown";

/** Every row carries a React `key` that survives updates (lib/rowKeys). */
export type ChatItem = (
  | { kind: "command"; card: CommandResult; text: string; taskId?: string }
  /** The agent seems stuck (agent.stuck). `paused`: the task waits for the
   * user; otherwise it went on (a subagent, or a task nobody could answer
   * for). `resolved`: the task went on or ended since. */
  | {
      kind: "stuck";
      jobId: string;
      text: string;
      taskId?: string;
      paused: boolean;
      resolved?: "continued" | "ended";
    }
  | {
      kind: "user";
      text: string;
      taskId?: string;
      /** The event that recorded this prompt (Edit & resend forks just
       * before it). */
      eventId?: number;
      /** A follow-up ShadowCode wrote after a plan limit: "auto" when the
       * engine started it, "manual" when the user chose Continue on …. */
      continued?: "auto" | "manual";
    }
  | {
      kind: "note";
      text: string;
      taskId?: string;
      warning?: boolean;
      /** The plain limit.reached note of this task; a limit.fallback record
       * replaces it. */
      limitOf?: string;
      /** A provider-busy retry line (model.retry), updated in place. */
      retry?: { attempt: number; done?: boolean };
    }
  /** A spending limit reached on a paid model (spend.limit_reached): the
   * task waits between steps for Continue or Stop. */
  | {
      kind: "spend";
      taskId: string;
      jobId: string;
      promptId: string;
      limitKind: "task" | "daily";
      title: string;
      text: string;
      continueLabel: string;
      /** How it ended: answered, lifted by a changed setting, or the task
       * finished. */
      resolved?: "continue" | "stop" | "lifted" | "ended";
      outcome?: string;
    }
  /** "Resume at <time>" after a plan limit (resume.* events). */
  | {
      kind: "resume";
      taskId?: string;
      resumeId: string;
      state:
        | "scheduled"
        | "cancelled"
        | "started"
        | "missed"
        | "failed"
        | "needs_consent";
      at: number;
      label: string;
      target: string;
      text: string;
      /** The continuation to send (needs_consent). */
      task?: string;
      /** The limited task's mode and web access, which the continuation
       * keeps. */
      mode?: TaskMode;
      web?: boolean;
      /** The limited task's @-mentions and "Only change these", which the
       * continuation keeps too. */
      mentions?: { path: string; kind: "file" | "dir" }[];
      onlyChange?: boolean;
    }
  /** What happened after a plan limit (limit.fallback). */
  | {
      kind: "limit";
      taskId: string;
      /** The record as one line of plain text. */
      text: string;
      /** continued: the engine started a local follow-up; ask: the user
       * decides; unavailable: nothing could continue (reason). */
      mode: "continued" | "ask" | "unavailable";
      from: string;
      to?: string;
      target?: string;
      reason?: string;
      /** The limited task's request, for Continue on …. */
      request?: string;
      /** A follow-up was started from this card. */
      resolved?: boolean;
      /** The limited job and when its plan resets, if the vendor said. */
      jobId?: string;
      resetsAt?: number;
      /** A resume is scheduled for this card's task. */
      resumeScheduled?: boolean;
    }
  /** A local model ran out of memory while loading (agent.completed with
   * local_out_of_memory). Nothing ran; the card offers a way on. */
  | {
      kind: "memory";
      taskId: string;
      /** The engine's plain explanation (also shown above the card). */
      text: string;
      model: string;
      /** A smaller context (tokens) that is still usable, if any. */
      smallerContext?: number;
      /** The task's request, sent again by "Use … and retry". */
      request: string;
      /** A later message was sent after this card. */
      resolved?: boolean;
    }
  /** A subagent run started by this task (subagent.* events). */
  | { kind: "subagent"; taskId?: string; text: string; run: SubagentRun }
  /** Provider change inside one conversation (agent.handoff). */
  | {
      kind: "divider";
      text: string;
      taskId?: string;
      /** A rewind: files went back to how they were before this task. */
      rewound?: boolean;
    }
  /** Final card for a finished task; content comes from transcript.activity. */
  | { kind: "summary"; taskId: string; text: string }
  | {
      kind: "agent";
      text: string;
      who?: string;
      /** A failed task's request, for "Try again". */
      request?: string;
      messageId?: string;
      eventId?: number;
      taskId?: string;
      live?: boolean;
    }
  | {
      kind: "tool";
      originEventId?: number;
      tool: string;
      ok?: boolean;
      text: string;
      live?: boolean;
      icon?: string;
      headline?: string;
      fullOutput?: string;
      collapsed?: boolean;
      taskId?: string;
      callId?: string;
      path?: string;
    }
) & { key?: string };

const MUTATING = new Set([
  "write_file",
  "edit_file",
  "delete_file",
  "move_file",
  "apply_patch",
]);

export function isMutatingTool(tool: string): boolean {
  return MUTATING.has(tool);
}

const BACKGROUND_LABELS = new Map([
  ["background_start", "Start background process"],
  ["background_list", "List background processes"],
  ["background_output", "Read process output"],
  ["background_stop", "Stop background process"],
]);

/** Codex-style collapsed one-liner. Click to expand; expanded cards expose
 *  Rewind (per-task file undo) and Review diff (jump to the Changes tab). */
export const OpCard = memo(function OpCard({
  item,
  onToggle,
  onRewind,
  onReviewDiff,
}: {
  item: Extract<ChatItem, { kind: "tool" }>;
  onToggle: () => void;
  onRewind?: (taskId: string) => void;
  onReviewDiff?: (path: string) => void;
}) {
  const open = item.collapsed === false;
  const canRewind =
    Boolean(item.taskId) &&
    isMutatingTool(item.tool) &&
    item.ok !== false &&
    !item.live;
  const canDiff =
    Boolean(item.path) &&
    isMutatingTool(item.tool) &&
    item.ok !== false &&
    !item.live;
  return (
    <div
      className={`op-card ${item.ok === false ? "bad" : ""} ${open ? "open" : ""} ${item.live ? "live" : ""}`}
    >
      <header
        role="button"
        tabIndex={0}
        aria-expanded={open}
        onClick={onToggle}
        onKeyDown={(e) => {
          if (e.key === "Enter" || e.key === " ") {
            e.preventDefault();
            onToggle();
          }
        }}
      >
        <span className="op-icon">
          {item.icon || (item.ok === false ? "✗" : item.ok ? "✓" : "●")}
        </span>
        <span className="op-headline">
          {item.headline && item.headline !== item.tool
            ? item.headline
            : BACKGROUND_LABELS.get(item.tool) || item.tool}
          {item.live ? " · running" : ""}
        </span>
        <span className="op-chev">{open ? "▾" : "▸"}</span>
      </header>
      {open && (
        <div className="op-body">
          {item.fullOutput ? (
            <pre className="op-full">
              {item.fullOutput.slice(0, 4000)}
              {item.fullOutput.length > 4000 ? "\n… (truncated)" : ""}
            </pre>
          ) : (
            <p className="hint op-none">No output.</p>
          )}
          {(canRewind || canDiff) && (
            <div className="op-actions">
              {canDiff && item.path && onReviewDiff && (
                <button
                  type="button"
                  className="mini"
                  onClick={() => onReviewDiff(item.path as string)}
                >
                  Review diff
                </button>
              )}
              {canRewind && item.taskId && onRewind && (
                <button
                  type="button"
                  className="mini"
                  title="Undo every file change made by this task"
                  onClick={() => onRewind(item.taskId as string)}
                >
                  ↶ Rewind
                </button>
              )}
            </div>
          )}
        </div>
      )}
    </div>
  );
});

export const CommandCardView = memo(function CommandCardView({
  card,
}: {
  card: CommandResult;
}) {
  if (card.kind === "text" && card.text) {
    return (
      <div className="msg-agent">
        <div className="who">Command</div>
        {card.text}
      </div>
    );
  }
  if (card.kind === "error") {
    return (
      <div className="tool-card bad">
        <header>
          <span>
            {card.icon || "✗"} {card.headline}
          </span>
          <span>error</span>
        </header>
        <pre>{card.body}</pre>
      </div>
    );
  }
  if (card.kind === "diff") {
    return (
      <div className="tool-card">
        <header>
          <span>diff · {card.path}</span>
        </header>
        {card.diff
          .split("\n")
          .slice(0, 200)
          .map((line, i) => (
            <div
              key={i}
              className={`diff-line ${line.startsWith("+") ? "diff-add" : line.startsWith("-") ? "diff-del" : "diff-ctx"}`}
            >
              {line}
            </div>
          ))}
      </div>
    );
  }
  if (card.kind === "list") {
    return (
      <div className="tool-card">
        <header>
          <span>
            {card.icon || "◆"} {card.headline}
          </span>
        </header>
        {card.body && <pre className="plan">{card.body}</pre>}
        {card.items.map((item, i) => (
          <div key={i} className="status-row">
            <span>{item.label}</span>
            <code>{item.value}</code>
          </div>
        ))}
      </div>
    );
  }
  return (
    <div className="tool-card">
      <header>
        <span>
          {card.icon || "◆"} {card.headline}
        </span>
      </header>
      {card.body &&
        (card.metadata.project_map ? (
          <Markdown>{card.body}</Markdown>
        ) : (
          <pre>{card.body}</pre>
        ))}
    </div>
  );
});

export function Empty({ title, body }: { title: string; body?: string }) {
  return (
    <div className="empty">
      <h3>{title}</h3>
      {body && <p>{body}</p>}
    </div>
  );
}

/** A panel that could not load: what went wrong and a way to try again,
 * instead of a dead end. */
export function LoadError({
  message,
  onRetry,
}: {
  message: string;
  onRetry: () => void | Promise<void>;
}) {
  const [retrying, setRetrying] = useState(false);
  return (
    <div className="load-error" role="alert">
      <p>{readableError(message)}</p>
      <button
        type="button"
        className="mini"
        disabled={retrying}
        onClick={async () => {
          setRetrying(true);
          try {
            await onRetry();
          } finally {
            setRetrying(false);
          }
        }}
      >
        {retrying ? "Trying again…" : "Try again"}
      </button>
    </div>
  );
}
