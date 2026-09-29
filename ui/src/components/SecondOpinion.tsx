/** Second opinions in the window: the reviewer picker, findings with "Ask
 * the agent to fix this" and "Dismiss", the reviewed diff of the Git tab
 * with findings next to its hunks, the transcript card, and the "Ask
 * another model" form under an answer. */
import { useId, useState, type ReactNode } from "react";
import {
  ArrowRightLeft,
  Check,
  CircleAlert,
  LoaderCircle,
  MessagesSquare,
  Undo2,
  Wrench,
  X,
} from "lucide-react";
import { DiffHunk } from "./DiffView";
import { Markdown } from "./Markdown";
import { languageOf } from "../lib/diff";
import { isApiKey, isLocal, type PickerTarget } from "../lib/picker";
import {
  bySeverity,
  findingsForHunk,
  isActiveOpinion,
  opinionStatus,
  reviewedHunks,
  reviewerChoices,
  SEVERITY_LABEL,
  unplaced,
  usageText,
  type Finding,
  type OpinionRoute,
  type SecondOpinion,
} from "../lib/secondOpinion";
import "../secondOpinion.css";

/** A model to review with, grouped like the composer's picker. */
export function ReviewerSelect({
  targets,
  value,
  onChange,
  offline,
  writer,
  disabled,
  label = "Reviewer model",
}: {
  targets: PickerTarget[];
  value: string;
  onChange: (id: string) => void;
  offline: boolean;
  writer?: OpinionRoute | null;
  disabled?: boolean;
  label?: string;
}) {
  const hintId = useId();
  const choices = reviewerChoices(targets, { offline, keep: value });
  const groups: [string, PickerTarget[]][] = [
    ["On this computer", choices.filter(isLocal)],
    ["Subscriptions", choices.filter((t) => !isLocal(t) && !isApiKey(t))],
    ["API keys", choices.filter(isApiKey)],
  ];
  const chosen = targets.find((t) => t.id === value);
  const same = Boolean(writer && value && writer.model === value);
  return (
    <div className="opinion-reviewer">
      <select
        aria-label={label}
        aria-describedby={hintId}
        value={choices.some((t) => t.id === value) ? value : ""}
        disabled={disabled || !choices.length}
        onChange={(e) => onChange(e.target.value)}
      >
        <option value="" disabled>
          {choices.length ? "Choose a model…" : "No model is ready"}
        </option>
        {groups
          .filter(([, rows]) => rows.length)
          .map(([name, rows]) => (
            <optgroup key={name} label={name}>
              {rows.map((t) => (
                <option key={t.id} value={t.id}>
                  {t.name}
                  {writer?.model === t.id ? " (wrote this)" : ""}
                </option>
              ))}
            </optgroup>
          ))}
      </select>
      <span className="hint" id={hintId}>
        {same
          ? "This is the model that wrote the change; another model gives a more independent review."
          : offline
            ? "Offline: only models on this computer can review."
            : writer
              ? `Written by ${writer.label}.${chosen && !isLocal(chosen) && writer.local ? " Reviewing on a cloud model asks first." : ""}`
              : "The reviewer only reads; it cannot edit files or run commands."}
      </span>
    </div>
  );
}

function SeverityBadge({ severity }: { severity: string }) {
  return (
    <span className={`opinion-severity is-${severity}`}>
      {SEVERITY_LABEL[severity] || "Medium"}
    </span>
  );
}

/** One finding: where, how bad, why, what to change, and its actions. */
export function FindingItem({
  finding,
  onFix,
  onDismiss,
  onRestore,
  working,
  showFile = true,
}: {
  finding: Finding;
  onFix?: () => void;
  onDismiss?: () => void;
  onRestore?: () => void;
  working?: boolean;
  showFile?: boolean;
}) {
  const where = [
    showFile && finding.file ? finding.file : "",
    finding.line
      ? `line ${finding.line}${finding.end_line ? `–${finding.end_line}` : ""}`
      : "",
  ]
    .filter(Boolean)
    .join(", ");
  const dismissed = finding.status === "dismissed";
  return (
    <article
      className={`opinion-finding is-${finding.severity}${dismissed ? " is-dismissed" : ""}`}
      aria-label={`${SEVERITY_LABEL[finding.severity] || "Medium"}: ${finding.title}`}
    >
      <header>
        <SeverityBadge severity={finding.severity} />
        <strong>{finding.title}</strong>
        {where && <span className="dim">{where}</span>}
      </header>
      {!dismissed && (
        <>
          {finding.explanation && finding.explanation !== finding.title && (
            <p>{finding.explanation}</p>
          )}
          {finding.suggested_fix && (
            <div className="opinion-fix">
              <span>Suggested fix</span>
              <Markdown>{finding.suggested_fix}</Markdown>
            </div>
          )}
        </>
      )}
      <div className="row opinion-finding-actions">
        {finding.status === "fixing" ? (
          <span className="opinion-fixing" role="status">
            <Check size={12} aria-hidden="true" /> Fix queued in the
            conversation
          </span>
        ) : dismissed ? (
          onRestore && (
            <button type="button" className="mini" onClick={onRestore}>
              <Undo2 size={11} aria-hidden="true" /> Show again
            </button>
          )
        ) : (
          <>
            {onFix && (
              <button
                type="button"
                className="mini"
                disabled={working}
                onClick={onFix}
              >
                <Wrench size={11} aria-hidden="true" /> Ask the agent to fix
                this
              </button>
            )}
            {onDismiss && (
              <button type="button" className="mini" onClick={onDismiss}>
                <X size={11} aria-hidden="true" /> Dismiss
              </button>
            )}
          </>
        )}
      </div>
    </article>
  );
}

export type FindingActions = {
  fix: (opinion: SecondOpinion, finding: Finding) => void;
  setFinding: (
    opinion: SecondOpinion,
    finding: Finding,
    status: "open" | "dismissed",
  ) => void;
  working?: string;
};

export function FindingItems({
  opinion,
  findings,
  actions,
  showFile,
}: {
  opinion: SecondOpinion;
  findings: Finding[];
  actions: FindingActions;
  showFile?: boolean;
}) {
  if (!findings.length) return null;
  return (
    <div className="opinion-findings">
      {[...findings].sort(bySeverity).map((finding) => (
        <FindingItem
          key={finding.id}
          finding={finding}
          showFile={showFile}
          working={actions.working === `${opinion.id}:${finding.id}`}
          onFix={() => actions.fix(opinion, finding)}
          onDismiss={() => actions.setFinding(opinion, finding, "dismissed")}
          onRestore={() => actions.setFinding(opinion, finding, "open")}
        />
      ))}
    </div>
  );
}

/** Model, status, usage and cost of a second opinion, with Stop while it
 * runs. */
export function OpinionHead({
  opinion,
  title,
  onCancel,
  outdated,
  extra,
}: {
  opinion: SecondOpinion;
  title: string;
  onCancel?: () => void;
  outdated?: boolean;
  extra?: ReactNode;
}) {
  const running = isActiveOpinion(opinion);
  const ok = opinion.status === "completed";
  const usage = usageText(opinion);
  return (
    <header className="opinion-head">
      <div className="opinion-title">
        <MessagesSquare size={14} aria-hidden="true" />
        <strong>{title}</strong>
        <span
          className={`opinion-status ${running ? "is-running" : ok ? "is-ok" : "is-warn"}`}
          role="status"
        >
          {running ? (
            <LoaderCircle size={12} className="spin" aria-hidden="true" />
          ) : ok ? (
            <Check size={12} aria-hidden="true" />
          ) : (
            <CircleAlert size={12} aria-hidden="true" />
          )}
          {opinionStatus(opinion)}
        </span>
        <span className="grow" />
        {extra}
        {running && onCancel && (
          <button type="button" className="mini" onClick={onCancel}>
            Stop
          </button>
        )}
      </div>
      <p className="opinion-meta">
        {[
          opinion.model_name && opinion.model_name !== opinion.reviewer.label
            ? `${opinion.reviewer.label} (${opinion.model_name})`
            : opinion.reviewer.label,
          opinion.reviewer.local ? "on this computer" : "cloud",
          usage,
          "read-only",
        ]
          .filter(Boolean)
          .join(" · ")}
      </p>
      {opinion.same_model && (
        <p className="hint">
          The same model wrote this; its review is less independent.
        </p>
      )}
      {outdated && (
        <p className="hint warn-text">
          The changes are different now. Review again to check the current
          version.
        </p>
      )}
    </header>
  );
}

/** Notes every result may carry: a reply in another form, a cut diff,
 * files the reviewer changed anyway, why it stopped. */
export function OpinionNotes({ opinion }: { opinion: SecondOpinion }) {
  return (
    <>
      {opinion.error && opinion.status !== "completed" && (
        <p className="opinion-error">{opinion.error}</p>
      )}
      {opinion.format_note && <p className="hint">{opinion.format_note}</p>}
      {opinion.truncated && (
        <p className="hint">
          Only part of the changes fit in the request; the reviewer could read
          the rest from the files.
        </p>
      )}
      {Boolean(opinion.redacted) && (
        <p className="hint">
          {opinion.redacted === 1
            ? "One value that looked like a password or key was hidden from the reviewer."
            : `${opinion.redacted} values that looked like passwords or keys were hidden from the reviewer.`}
        </p>
      )}
      {opinion.omitted.length > 0 && (
        <p className="hint">
          Not sent: {opinion.omitted.join(", ")} (secret files).
        </p>
      )}
      {opinion.reviewer_changed.length > 0 && (
        <p className="warn-text">
          The reviewer reported changing {opinion.reviewer_changed.join(", ")}{" "}
          although it was asked only to read. Check the Changes tab.
        </p>
      )}
    </>
  );
}

/** A finished review of the staged changes: the reviewed hunks with their
 * findings next to them, then findings no hunk shows. */
export function ReviewedChanges({
  opinion,
  actions,
}: {
  opinion: SecondOpinion;
  actions: FindingActions;
}) {
  const files = reviewedHunks(opinion);
  const paths = files.map((f) => f.path);
  const general = opinion.findings.filter(
    (finding) => !finding.file || !paths.includes(finding.file),
  );
  return (
    <div className="opinion-reviewed">
      {general.length > 0 && (
        <section aria-label="General findings">
          <FindingItems
            opinion={opinion}
            findings={general}
            actions={actions}
          />
        </section>
      )}
      {files
        .filter((file) =>
          opinion.findings.some((finding) => finding.file === file.path),
        )
        .map((file) => {
          const headers = file.hunks.map((h) => h.header);
          const language = languageOf(file.path);
          const loose = unplaced(opinion.findings, file.path, headers);
          return (
            <section
              key={file.path}
              className="opinion-file"
              aria-label={`Findings in ${file.path}`}
            >
              <h5>
                <code>{file.path}</code>
              </h5>
              <FindingItems
                opinion={opinion}
                findings={loose}
                actions={actions}
                showFile={false}
              />
              {file.hunks.map((hunk) => {
                const here = findingsForHunk(
                  opinion.findings,
                  file.path,
                  hunk.header,
                );
                if (!here.length) return null;
                return (
                  <div key={hunk.header} className="opinion-hunk">
                    <DiffHunk
                      hunk={hunk}
                      layout="unified"
                      language={language}
                      maxLines={40}
                    />
                    <FindingItems
                      opinion={opinion}
                      findings={here}
                      actions={actions}
                      showFile={false}
                    />
                  </div>
                );
              })}
            </section>
          );
        })}
    </div>
  );
}

/** The labelled card a second opinion shows in the transcript, after the
 * answer it is about. */
export function OpinionCard({
  opinion,
  actions,
  onCancel,
  onContinue,
  onShowInReview,
  onOpenWork,
}: {
  opinion: SecondOpinion;
  actions: FindingActions;
  onCancel: () => void;
  onContinue?: () => void;
  onShowInReview?: () => void;
  onOpenWork?: () => void;
}) {
  const done = opinion.status === "completed";
  const title =
    opinion.kind === "ask"
      ? `Second opinion from ${opinion.reviewer.label}`
      : `Review by ${opinion.reviewer.label}`;
  return (
    <section className="opinion-card" aria-label={title}>
      <OpinionHead opinion={opinion} title={title} onCancel={onCancel} />
      {opinion.question && (
        <p className="opinion-question">
          <span>You asked</span> {opinion.question}
        </p>
      )}
      {done && opinion.summary && (
        <div className="opinion-summary">
          <Markdown>{opinion.summary}</Markdown>
        </div>
      )}
      <OpinionNotes opinion={opinion} />
      {done && opinion.kind === "review" && (
        <FindingItems
          opinion={opinion}
          findings={opinion.findings}
          actions={actions}
        />
      )}
      {!isActiveOpinion(opinion) && (
        <div className="row opinion-card-actions">
          {done && onContinue && (
            <button
              type="button"
              className="mini"
              onClick={onContinue}
              title="The next message goes to this model; earlier turns are handed over as a summary."
            >
              <ArrowRightLeft size={11} aria-hidden="true" /> Continue with{" "}
              {opinion.reviewer.label}
            </button>
          )}
          {done &&
            opinion.kind === "review" &&
            opinion.findings.length > 0 &&
            onShowInReview && (
              <button type="button" className="mini" onClick={onShowInReview}>
                Show next to the changes
              </button>
            )}
          {onOpenWork && (
            <button type="button" className="ghost link" onClick={onOpenWork}>
              What the reviewer read
            </button>
          )}
        </div>
      )}
    </section>
  );
}

/** "Ask another model" under an answer: a reviewer and an optional
 * question. */
export function AskAnotherModel({
  targets,
  initial,
  offline,
  writer,
  onAsk,
  onClose,
}: {
  targets: PickerTarget[];
  initial: string;
  offline: boolean;
  writer?: OpinionRoute | null;
  onAsk: (model: string, question: string) => void;
  onClose: () => void;
}) {
  const [model, setModel] = useState(initial);
  const [question, setQuestion] = useState("");
  const id = useId();
  return (
    <form
      className="opinion-ask"
      aria-label="Ask another model"
      onSubmit={(e) => {
        e.preventDefault();
        if (model) onAsk(model, question.trim());
      }}
      onKeyDown={(e) => {
        if (e.key === "Escape") {
          e.stopPropagation();
          onClose();
        }
      }}
    >
      <ReviewerSelect
        targets={targets}
        value={model}
        onChange={setModel}
        offline={offline}
        writer={writer}
        label="Model for the second opinion"
      />
      <label htmlFor={`${id}-q`} className="sr-only">
        What should it look at? (optional)
      </label>
      <textarea
        id={`${id}-q`}
        rows={2}
        maxLength={2000}
        placeholder="What should it look at? (optional)"
        value={question}
        onChange={(e) => setQuestion(e.target.value)}
      />
      <p className="hint">
        It gets the request, this answer and the task's changes, reads the
        project if it needs to, and changes nothing.
      </p>
      <div className="row end">
        <button type="button" className="ghost" onClick={onClose}>
          Cancel
        </button>
        <button type="submit" className="primary" disabled={!model}>
          Ask
        </button>
      </div>
    </form>
  );
}
