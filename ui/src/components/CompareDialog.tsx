import { useId, useState } from "react";
import { LoaderCircle, Plus, X } from "lucide-react";
import { Dialog } from "./Dialog";
import { UnifiedPicker } from "./UnifiedPicker";
import {
  availabilityLabel,
  shortName,
  usageLabel,
  type PickerTarget,
} from "../lib/picker";
import {
  MAX_LANES,
  MIN_LANES,
  badgeFor,
  checkLineup,
  errorText,
  slotTargets,
} from "../lib/compare";

/** Compare: send the composer's task to 2–3 models, each in its own copy of
 * the project. The lineup is chosen from the composer's picker rows. */
export function CompareDialog({
  task,
  targets,
  loading,
  models,
  onModels,
  uncommitted,
  unsavedFiles = [],
  web,
  onStart,
  onClose,
  onOpenAllowance,
  onConnect,
  onSetup,
  onAddLocal,
}: {
  task: string;
  targets: PickerTarget[];
  loading?: boolean;
  /** Chosen picker ids, one per slot ("" = not chosen yet). */
  models: string[];
  onModels: (models: string[]) => void;
  /** Uncommitted files in the project (they are copied into every lane). */
  uncommitted: number;
  /** Open editor drafts do not exist in the filesystem snapshot yet. */
  unsavedFiles?: string[];
  /** Web lookups are on for models that run on ShadowCode's own loop. */
  web?: boolean;
  /** Starts the comparison; a rejection is shown in the dialog. */
  onStart: (models: string[]) => Promise<void>;
  onClose: () => void;
  onOpenAllowance: () => void;
  onConnect: (vendor?: string) => void;
  onSetup: (target: PickerTarget) => void;
  onAddLocal: () => void;
}) {
  const uid = useId().replace(/:/g, "");
  const [open, setOpen] = useState(-1);
  const [starting, setStarting] = useState(false);
  const [error, setError] = useState("");
  const [tried, setTried] = useState(false);
  const check = checkLineup(models, targets);
  const chosen = models
    .map((id) => targets.find((t) => t.id === id))
    .filter((t): t is PickerTarget => Boolean(t));

  function setSlot(index: number, id: string) {
    const next = [...models];
    next[index] = id;
    onModels(next);
    setError("");
  }
  async function start() {
    setTried(true);
    if (check.error || unsavedFiles.length || starting) return;
    setStarting(true);
    setError("");
    try {
      await onStart(models);
    } catch (e) {
      setError(errorText(e));
    } finally {
      setStarting(false);
    }
  }

  return (
    <Dialog
      label="Compare models"
      className="modal compare-dialog"
      onClose={onClose}
    >
      <div
        className="compare-dialog-body"
        onKeyDown={(e) => {
          // An open picker closes itself first (it stops the event).
          if (e.key === "Escape") {
            e.stopPropagation();
            onClose();
          }
        }}
      >
        <header className="compare-dialog-head">
          <div>
            <h2 id={`${uid}-title`}>Compare models</h2>
            <p className="hint">
              Each model works on the same task in its own copy of the project.
              You review the results side by side and keep one.
            </p>
          </div>
          <button
            type="button"
            className="icon-btn"
            aria-label="Close Compare"
            onClick={onClose}
          >
            <X size={16} aria-hidden="true" />
          </button>
        </header>
        <section aria-labelledby={`${uid}-task`}>
          <h3 id={`${uid}-task`} className="compare-label">
            Task
          </h3>
          <blockquote className="compare-task">{task}</blockquote>
        </section>
        <fieldset className="compare-slots">
          <legend className="compare-label">
            Models ({MIN_LANES} or {MAX_LANES})
          </legend>
          {models.map((id, index) => {
            const target = targets.find((t) => t.id === id);
            const badge = target ? badgeFor(id, target) : null;
            const problem = check.slots[index];
            const showProblem = problem && (id || tried);
            return (
              <div className="compare-slot" key={index}>
                <div className="compare-slot-row">
                  <span className="compare-slot-number" aria-hidden="true">
                    {index + 1}
                  </span>
                  <UnifiedPicker
                    label={`Model ${index + 1}`}
                    targets={slotTargets(targets, models, index)}
                    value={id}
                    open={open === index}
                    loading={loading}
                    onOpenChange={(next) => setOpen(next ? index : -1)}
                    onSelect={(value) => setSlot(index, value)}
                    onConnect={onConnect}
                    onSetup={onSetup}
                    onAddLocal={onAddLocal}
                  />
                  {models.length > MIN_LANES && (
                    <button
                      type="button"
                      className="icon-btn"
                      aria-label={`Remove model ${index + 1}`}
                      onClick={() => {
                        onModels(models.filter((_, i) => i !== index));
                        setOpen(-1);
                      }}
                    >
                      <X size={14} aria-hidden="true" />
                    </button>
                  )}
                </div>
                {target && badge && (
                  <p className="compare-slot-meta">
                    <span className="sr-only">{badge.label} · </span>
                    <span className={`avail avail-${target.availability}`}>
                      {availabilityLabel(target)}
                    </span>
                    {" · "}
                    {usageLabel(target.usage, target.inference)}
                  </p>
                )}
                {showProblem && (
                  <p className="compare-slot-error" role="status">
                    {problem}
                  </p>
                )}
              </div>
            );
          })}
          {models.length < MAX_LANES && (
            <button
              type="button"
              className="ghost compare-add"
              onClick={() => onModels([...models, ""])}
            >
              <Plus size={14} aria-hidden="true" /> Add a third model
            </button>
          )}
          <p className="hint">
            Local models run one at a time in selection order. Automatic CPU
            fallback is disabled for comparisons.
          </p>
        </fieldset>
        <section className="compare-cost" aria-label="Cost">
          <p>
            <strong>
              Each model uses its own plan allowance or OpenRouter credit.
            </strong>{" "}
            Every model runs the full task, so {models.length} models cost about{" "}
            {models.length === 3 ? "three" : "two"} times as much as one.{" "}
            <button type="button" className="link" onClick={onOpenAllowance}>
              Open Allowance
            </button>
          </p>
          {chosen.length > 0 && (
            <ul>
              {chosen.map((target) => (
                <li key={target.id}>
                  <span>{shortName(target)}</span>
                  <span className="dim">
                    {usageLabel(target.usage, target.inference)}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </section>
        <p className="hint compare-base">
          {uncommitted > 0
            ? `Your ${uncommitted} uncommitted ${uncommitted === 1 ? "change is" : "changes are"} included: every model starts from the project as it is now.`
            : "Every model starts from your latest commit."}{" "}
          Your project is not changed until you keep a result.
          {web ? " Web lookups are on for models that support them." : ""}
        </p>
        {unsavedFiles.length > 0 && (
          <p className="health-bad compare-error" role="alert">
            Save or discard these open drafts before comparing:{" "}
            {unsavedFiles.join(", ")}. Compare snapshots files from disk, so
            unsaved editor text would otherwise be missing.
          </p>
        )}
        {error && (
          <p className="health-bad compare-error" role="alert">
            {error}
          </p>
        )}
        {tried && check.error && !error && (
          <p className="health-bad compare-error" role="alert">
            {check.error}
          </p>
        )}
        <div className="row compare-dialog-actions">
          <button type="button" className="ghost" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="primary"
            disabled={unsavedFiles.length > 0 || starting}
            aria-disabled={
              Boolean(check.error) || unsavedFiles.length > 0 || starting
            }
            title={
              unsavedFiles.length
                ? "Save or discard open drafts first"
                : check.error || undefined
            }
            onClick={() => void start()}
          >
            {starting && (
              <LoaderCircle size={14} className="spin" aria-hidden="true" />
            )}
            {starting ? "Starting…" : "Start comparison"}
          </button>
        </div>
      </div>
    </Dialog>
  );
}
