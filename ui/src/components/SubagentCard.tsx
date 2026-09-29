import { useState } from "react";
import {
  Bot,
  Check,
  ChevronRight,
  CircleAlert,
  ClipboardList,
  Compass,
  ExternalLink,
  Hammer,
  LoaderCircle,
  SearchCheck,
} from "lucide-react";
import type { RoleId } from "../api";
import { Markdown } from "./Markdown";
import { statusLabel, type SubagentRun } from "../lib/subagents";
import { ROLE_LABELS, roleCost, verdictLabel } from "../lib/roles";

const ROLE_ICONS: Record<RoleId, typeof Bot> = {
  plan: ClipboardList,
  implement: Hammer,
  review: SearchCheck,
  explore: Compass,
};

/** A subagent run inside the parent transcript: one line while collapsed,
 * its result, changed files and a link to its own conversation when open.
 * A run the project's roles chose is a role card: the role, the model or
 * vendor CLI that played it, its cost and status. */
export function SubagentCard({
  run,
  onOpen,
}: {
  run: SubagentRun;
  /** Open the child's conversation (hidden from the sidebar). */
  onOpen?: (sessionId: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const running = run.status === "running" || run.status === "queued";
  const ok = run.status === "completed";
  const needsChanges = run.verdict === "needs_changes";
  const tone = running ? "running" : ok && !needsChanges ? "ok" : "warn";
  const label = run.description || run.prompt;
  const bodyId = `subagent-${run.runId}`;
  const role = run.role;
  const Icon = role ? ROLE_ICONS[role] : Bot;
  const cost =
    role || run.runner === "vendor" ? roleCost(run.cost, run.usage) : "";
  const verdict = verdictLabel(run.verdict);
  return (
    <div
      className={`subagent-card tone-${tone}${role ? " role-card" : ""}`}
      data-role={role}
    >
      <button
        type="button"
        className="subagent-head"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setOpen((v) => !v)}
      >
        <ChevronRight
          size={14}
          aria-hidden="true"
          className={`subagent-chevron ${open ? "is-open" : ""}`}
        />
        <Icon size={14} aria-hidden="true" />
        {role ? (
          <>
            <strong className="subagent-name">{ROLE_LABELS[role]}</strong>
            <span className="subagent-label">{run.model}</span>
          </>
        ) : (
          <>
            <strong className="subagent-name">@{run.agent}</strong>
            <span className="subagent-label">{label}</span>
          </>
        )}
        <span className="subagent-meta">
          {[
            !role && run.runner === "vendor" ? run.model : "",
            run.mode === "write" ? "worktree" : "read-only",
            run.files.length > 0
              ? `${run.files.length} file${run.files.length === 1 ? "" : "s"}`
              : "",
            run.applied ? "applied" : "",
            cost,
          ]
            .filter(Boolean)
            .join(" · ")}
        </span>
        {verdict && !running && (
          <span className={`role-verdict ${needsChanges ? "warn" : "ok"}`}>
            {verdict}
          </span>
        )}
        <span className={`subagent-status tone-${tone}`} role="status">
          {running ? (
            <LoaderCircle size={13} className="spin" aria-hidden="true" />
          ) : ok ? (
            <Check size={13} aria-hidden="true" />
          ) : (
            <CircleAlert size={13} aria-hidden="true" />
          )}
          {statusLabel(run)}
        </span>
      </button>
      {open && (
        <div className="subagent-body" id={bodyId}>
          {run.prompt && !role && (
            <p className="subagent-prompt">
              <span>Task</span> {run.prompt}
            </p>
          )}
          {run.summary && !running && (
            <div className="subagent-summary">
              <Markdown>{run.summary}</Markdown>
            </div>
          )}
          {run.error && run.error !== run.summary && (
            <p className="subagent-error">{run.error}</p>
          )}
          {run.files.length > 0 && (
            <ul className="subagent-files" aria-label="Changed files">
              {run.files.map((f) => (
                <li key={f.path}>
                  <code>{f.path}</code>
                  <span className="subagent-stat">
                    {f.binary ? (
                      "binary"
                    ) : (
                      <>
                        <span className="add">+{f.additions}</span>{" "}
                        <span className="del">−{f.deletions}</span>
                      </>
                    )}
                  </span>
                </li>
              ))}
              {run.filesTruncated && <li>More files not listed</li>}
            </ul>
          )}
          {run.mode === "write" && !running && (
            <p className="subagent-note">
              {run.applied
                ? role
                  ? "These changes were applied to the project."
                  : "The parent agent applied these changes to the project."
                : run.patch
                  ? role
                    ? "Changes were made in an isolated worktree. They are applied to the project with your usual edit approval after the review."
                    : "Changes were made in an isolated worktree. The parent agent reviews the diff and applies it with your usual edit approval."
                  : "No changes to apply."}
            </p>
          )}
          {run.notes.map((note) => (
            <p key={note} className="subagent-note">
              {note}
            </p>
          ))}
          <div className="subagent-foot">
            <span>
              {[
                run.model,
                run.runner === "vendor"
                  ? "vendor CLI"
                  : run.runner === "shadowcode"
                    ? "ShadowCode's agent"
                    : "",
                run.route === "local"
                  ? "this computer"
                  : run.route === "cloud"
                    ? "cloud"
                    : "",
                run.steps
                  ? `${run.steps} step${run.steps === 1 ? "" : "s"}`
                  : "",
                run.tokens ? `${run.tokens.toLocaleString()} tokens` : "",
                cost,
                run.durationS != null ? `${run.durationS}s` : "",
              ]
                .filter(Boolean)
                .join(" · ")}
            </span>
            {onOpen && run.sessionId && (
              <button
                type="button"
                className="ghost"
                onClick={() => onOpen(run.sessionId)}
              >
                <ExternalLink size={13} aria-hidden="true" /> Open transcript
              </button>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
