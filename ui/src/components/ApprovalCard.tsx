import { memo, useId, useState } from "react";
import type { Approval, ApprovalAssessment, PreviewFile } from "../api";
import { languageOf, parseUnified } from "../lib/diff";
import { DiffHunk } from "./DiffView";

export type ApprovalDecision = {
  decision: "approve" | "deny";
  /** "task": allow the same kind of action for the rest of the task;
   * "project": allow this exact command from now on in this project. */
  scope?: "once" | "task" | "project";
  /** Sent back to the agent with a denial. */
  note?: string;
};

const FILE_TOOLS = new Set([
  "write_file",
  "edit_file",
  "apply_patch",
  "delete_file",
  "create_directory",
  "move_file",
]);
const BACKGROUND_LABELS = new Map([
  ["background_start", "Start background process"],
  ["background_list", "List background processes"],
  ["background_output", "Read process output"],
  ["background_stop", "Stop background process"],
]);

/** Diff lines the card shows before "Show all". */
export const PREVIEW_LINES = 14;

const STATUS: Record<string, string> = {
  added: "New file",
  deleted: "Deleted",
  modified: "Edited",
};

function FilePreview({ file }: { file: PreviewFile }) {
  const [all, setAll] = useState(false);
  const hunks = parseUnified(file.diff);
  const total = hunks.reduce((n, h) => n + h.lines.length, 0);
  const language = languageOf(file.path);
  let budget = all ? Infinity : PREVIEW_LINES;
  return (
    <div className="approval-file">
      <div className="approval-file-head">
        <span className="approval-file-status">
          {STATUS[file.status] || file.status}
        </span>
        <code>{file.path}</code>
        {!file.binary && (
          <span className="diff-stat">
            <span className="add">+{file.added}</span>{" "}
            <span className="del">−{file.removed}</span>
          </span>
        )}
      </div>
      {file.binary ? (
        <p className="hint">Binary or very large file; no text preview.</p>
      ) : !hunks.length && file.truncated ? (
        <p className="hint">
          This change is too large to preview here; the line counts are
          complete.
        </p>
      ) : (
        hunks.map((hunk, i) => {
          if (budget <= 0) return null;
          const shown = Math.min(budget, hunk.lines.length);
          budget -= shown;
          return (
            <DiffHunk
              key={i}
              hunk={hunk}
              layout="unified"
              language={language}
              maxLines={shown}
            />
          );
        })
      )}
      {!file.binary && total > PREVIEW_LINES && (
        <button
          type="button"
          className="link"
          aria-expanded={all}
          onClick={() => setAll((v) => !v)}
        >
          {all ? "Show less" : `Show all ${total} lines`}
        </button>
      )}
      {all && file.truncated && (
        <p className="hint">
          The preview stops at 2,000 lines; the change itself is complete.
        </p>
      )}
    </div>
  );
}

/** What the action does in plain words, how risky it is, and whether
 * Rewind can undo it, plus any extra checks (for example new packages). */
function Assessment({ assessment }: { assessment: ApprovalAssessment }) {
  return (
    <div className="approval-assessment">
      <p className="approval-explain">{assessment.explanation}</p>
      <div className="approval-tags">
        <span className={`risk-tag risk-${assessment.risk}`}>
          {assessment.risk_label}
        </span>
        <span className={`undo-tag undo-${assessment.undo}`}>
          {assessment.undo_label}
        </span>
      </div>
      {assessment.notes.length > 0 && (
        <ul className="approval-notes">
          {assessment.notes.map((note) => (
            <li key={note}>{note}</li>
          ))}
        </ul>
      )}
      {assessment.checks.map((check) => (
        <section
          key={check.title}
          className={`approval-check check-${check.level || "info"}`}
          aria-label={check.title}
        >
          <h4>{check.title}</h4>
          <ul>
            {check.items.map((item) => (
              <li key={item}>{item}</li>
            ))}
          </ul>
        </section>
      ))}
    </div>
  );
}

/** A pending approval: what the action would do (a diff, the new file, or
 * the full command and folder), and Allow / Allow for this task / Deny /
 * Deny with a note. */
export const ApprovalCard = memo(function ApprovalCard({
  approval,
  onDecide,
}: {
  approval: Approval;
  onDecide: (id: string, answer: ApprovalDecision) => void;
}) {
  const uid = useId();
  const [noting, setNoting] = useState(false);
  const [note, setNote] = useState("");
  const tool = approval.tool || "";
  const preview = approval.preview || null;
  // File tools report their tool name as the "command"; what they do is in
  // the reason ("Write hello.txt").
  const fileChange =
    FILE_TOOLS.has(tool) ||
    preview?.kind === "files" ||
    preview?.kind === "move" ||
    preview?.kind === "folder";
  const command =
    approval.command && approval.command !== tool ? approval.command : "";
  const main = fileChange ? approval.reason || command : command;
  const detail = main === approval.reason ? "" : approval.reason;
  const decide = (answer: ApprovalDecision) => onDecide(approval.id, answer);
  return (
    <div
      className="approval"
      data-approval-id={approval.id}
      role="group"
      aria-labelledby={`${uid}-title`}
    >
      <div className="approval-head">
        <span className="approval-kind">
          {BACKGROUND_LABELS.has(tool)
            ? "Background process"
            : fileChange
              ? "File change"
              : tool === "exec" || preview?.kind === "command"
                ? "Command"
                : tool || "Permission"}
        </span>
        <span className="approval-title" id={`${uid}-title`}>
          {tool === "background_stop"
            ? "Allow ShadowCode to stop this process?"
            : tool === "background_start"
              ? "Allow ShadowCode to start this process?"
              : fileChange
                ? "Allow this change?"
                : "Allow ShadowCode to run this?"}
        </span>
      </div>
      {approval.assessment?.explanation && (
        <Assessment assessment={approval.assessment} />
      )}
      {preview?.kind === "command" ? (
        <div className="approval-command">
          <pre className="code">{preview.command}</pre>
          <p className="hint">
            Runs in <code>{preview.cwd}</code>
          </p>
        </div>
      ) : preview?.kind === "files" ? (
        <>
          {main && <p className="approval-summary">{main}</p>}
          {preview.files.map((file) => (
            <FilePreview key={file.path} file={file} />
          ))}
        </>
      ) : preview?.kind === "move" ? (
        <pre className="code">
          Move {preview.from} → {preview.to}
        </pre>
      ) : preview?.kind === "folder" ? (
        <pre className="code">Create folder {preview.path}</pre>
      ) : (
        main && <pre className="code">{main}</pre>
      )}
      {detail && <p className="hint">{detail}</p>}
      {noting ? (
        <form
          className="approval-note"
          onSubmit={(e) => {
            e.preventDefault();
            decide({ decision: "deny", note: note.trim() || undefined });
          }}
        >
          <label htmlFor={`${uid}-note`}>
            Tell the agent why, or what to do instead
          </label>
          <textarea
            id={`${uid}-note`}
            value={note}
            rows={2}
            autoFocus
            onChange={(e) => setNote(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.stopPropagation();
                setNoting(false);
              }
            }}
          />
          <div className="row approval-actions">
            <button
              type="button"
              className="ghost"
              onClick={() => setNoting(false)}
            >
              Back
            </button>
            <button type="submit" className="primary">
              Deny and send note
            </button>
          </div>
        </form>
      ) : (
        <div className="row approval-actions">
          <button
            type="button"
            className="ghost"
            onClick={() => decide({ decision: "deny" })}
          >
            Deny
          </button>
          {approval.note && (
            <button
              type="button"
              className="ghost"
              onClick={() => setNoting(true)}
            >
              Deny with note…
            </button>
          )}
          {approval.always && (
            <button
              type="button"
              className="ghost"
              title={approval.always.replace(/`/g, "")}
              onClick={() => decide({ decision: "approve", scope: "project" })}
            >
              Always allow here
            </button>
          )}
          {approval.grant && (
            <button
              type="button"
              className="ghost"
              title={`Allow ${approval.grant.replace(/`/g, "")} without asking again until this task ends`}
              onClick={() => decide({ decision: "approve", scope: "task" })}
            >
              Allow for this task
            </button>
          )}
          <button
            type="button"
            className="primary"
            onClick={() => decide({ decision: "approve" })}
          >
            Allow
          </button>
        </div>
      )}
      {approval.grant && !noting && (
        <p className="hint approval-grant">
          “Allow for this task” covers {approval.grant.replace(/`/g, "")} until
          this task ends.
        </p>
      )}
      {approval.always && !noting && (
        <p className="hint approval-grant">
          “Always allow here” covers exactly this command in this project.
          Remove it in Settings › Permissions & network.
        </p>
      )}
    </div>
  );
});
