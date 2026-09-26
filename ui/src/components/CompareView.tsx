import { useCallback, useEffect, useId, useRef, useState } from "react";
import {
  ArrowLeft,
  Check,
  CircleAlert,
  LoaderCircle,
  MessageSquare,
  Square,
  Trash2,
  Trophy,
} from "lucide-react";
import {
  api,
  type Approval,
  type CompareLane,
  type CompareRecord,
  type CompareScore,
} from "../api";
import { Dialog } from "./Dialog";
import { relativeTime, type PickerTarget } from "../lib/picker";
import {
  badgeFor,
  conflictFiles,
  errorText,
  formatSeconds,
  formatTokenCount,
  isActive,
  keepBlocked,
  laneState,
} from "../lib/compare";

type Confirm = { kind: "keep"; lane: CompareLane } | { kind: "discard" };
type Conflict = { model: string; message: string; files: string[] };

const STATE_LABELS: Record<string, string> = {
  running: "Running",
  done: "Finished · choose a result to keep",
  applied: "Result kept",
  discarded: "Discarded",
  needs_review: "Interrupted Keep · recovery review required",
};

/** The Compare view: every lane of one comparison side by side, with Keep,
 * Stop and Discard, and the project's scoreboard. */
export function CompareView({
  workspace,
  compareId,
  targets,
  pollMs = 1500,
  onSelect,
  onClose,
  onOpenLane,
  onOpenDiff,
  onOpenChanges,
  onApplied,
  onRecords,
}: {
  /** The project the comparisons belong to. */
  workspace: string;
  /** The comparison to show; "" shows the newest. */
  compareId: string;
  targets: PickerTarget[];
  pollMs?: number;
  onSelect: (id: string) => void;
  onClose: () => void;
  onOpenLane: (record: CompareRecord, lane: CompareLane) => void;
  /** One changed file of a lane that still has its copy. */
  onOpenDiff: (record: CompareRecord, lane: CompareLane, path: string) => void;
  /** The project's own Changes (after Keep), optionally at one file. */
  onOpenChanges: (path?: string) => void;
  onApplied?: (record: CompareRecord) => void;
  /** Every record loaded (the app learns which folders are lane copies). */
  onRecords?: (records: CompareRecord[]) => void;
}) {
  const uid = useId().replace(/:/g, "");
  const [list, setList] = useState<CompareRecord[] | null>(null);
  const [record, setRecord] = useState<CompareRecord | null>(null);
  const [scores, setScores] = useState<CompareScore[] | null>(null);
  const [approvals, setApprovals] = useState<Approval[]>([]);
  const [error, setError] = useState("");
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [confirm, setConfirm] = useState<Confirm | null>(null);
  const [acting, setActing] = useState("");
  const selected = compareId || list?.[0]?.id || "";
  const selectedRef = useRef(selected);
  selectedRef.current = selected;
  const lastState = useRef("");
  /** Bumped by every action, so a poll that started before it cannot
   * overwrite its result. */
  const epoch = useRef(0);

  const loadList = useCallback(async () => {
    try {
      const [compares, board] = await Promise.all([
        api.compares(workspace),
        api.compareScoreboard(workspace),
      ]);
      setList(compares.compares);
      setScores(board.rows);
      onRecords?.(compares.compares);
    } catch (e) {
      setList((prev) => prev || []);
      setError(errorText(e));
    }
  }, [workspace, onRecords]);

  useEffect(() => {
    void loadList();
  }, [loadList]);

  // The selected record, polled while its lanes run.
  useEffect(() => {
    if (!selected) {
      setRecord(null);
      return;
    }
    let live = true;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const poll = async () => {
      const started = epoch.current;
      try {
        const [next, pending] = await Promise.all([
          api.compare(selected),
          api.approvals().catch(() => ({ approvals: [] as Approval[] })),
        ]);
        if (!live) return;
        if (started === epoch.current) {
          setRecord(next);
          setApprovals(pending.approvals);
          if (lastState.current === "running" && next.state !== "running")
            void loadList();
          lastState.current = next.state;
        }
        // Follow-up turns in a lane conversation run after the comparison
        // finished; keep polling while any lane works.
        const working =
          next.state === "running" ||
          next.lanes.some((lane) => !lane.removed && isActive(lane.status));
        if (working) timer = setTimeout(poll, pollMs);
      } catch (e) {
        if (!live) return;
        setError(errorText(e));
        timer = setTimeout(poll, pollMs * 2);
      }
    };
    setConflict(null);
    setError("");
    lastState.current = "";
    void poll();
    return () => {
      live = false;
      if (timer) clearTimeout(timer);
    };
  }, [selected, pollMs, loadList]);

  async function act(
    name: string,
    run: () => Promise<CompareRecord>,
  ): Promise<void> {
    if (acting) return;
    setActing(name);
    setError("");
    setConflict(null);
    epoch.current += 1;
    try {
      const next = await run();
      epoch.current += 1;
      setRecord(next);
      lastState.current = next.state;
      if (next.state === "applied") onApplied?.(next);
      await loadList();
    } catch (e) {
      const message = errorText(e);
      const files = conflictFiles(message);
      if (files && confirm?.kind === "keep")
        setConflict({ model: confirm.lane.name, message, files });
      else if (files) setConflict({ model: "", message, files });
      else setError(message);
      // Whatever happened, show the lanes as they are now.
      epoch.current += 1;
      const id = selectedRef.current;
      if (id)
        await api
          .compare(id)
          .then((next) => {
            if (selectedRef.current === id) setRecord(next);
          })
          .catch(() => undefined);
    } finally {
      setActing("");
    }
  }

  function doConfirm() {
    if (!confirm || !record) return;
    const current = confirm;
    const id = record.id;
    void act(current.kind, () =>
      current.kind === "keep"
        ? api.keepCompare(
            id,
            current.lane.model,
            Boolean(current.lane.checks.incomplete),
          )
        : api.discardCompare(id),
    ).finally(() => setConfirm(null));
  }

  const running = record?.lanes.some(
    (lane) => !lane.removed && isActive(lane.status),
  );
  const open = record && ["running", "done"].includes(record.state);
  const winner = record?.lanes.find((lane) => lane.model === record.winner);

  return (
    <section className="compare-view" aria-labelledby={`${uid}-title`}>
      <header className="compare-head">
        <button type="button" className="ghost" onClick={onClose}>
          <ArrowLeft size={14} aria-hidden="true" /> Back to conversation
        </button>
        <h2 id={`${uid}-title`}>Comparisons</h2>
        {list && list.length > 1 && (
          <label className="compare-pick">
            <span className="sr-only">Comparison</span>
            <select value={selected} onChange={(e) => onSelect(e.target.value)}>
              {list.map((item) => (
                <option key={item.id} value={item.id}>
                  {`${item.task.slice(0, 60)}${item.task.length > 60 ? "…" : ""} · ${relativeTime(item.created_at)}`}
                </option>
              ))}
            </select>
          </label>
        )}
      </header>
      <div className="compare-scroll">
        {list === null ? (
          <p className="compare-loading" role="status">
            <LoaderCircle size={16} className="spin" aria-hidden="true" />{" "}
            Loading comparisons…
          </p>
        ) : !selected ? (
          <div className="compare-empty">
            <p>
              <strong>No comparisons in this project yet.</strong>
            </p>
            <p className="hint">
              Type a task and choose Compare next to Send to run it on two or
              three models at once.
            </p>
          </div>
        ) : !record ? (
          <p className="compare-loading" role="status">
            <LoaderCircle size={16} className="spin" aria-hidden="true" />{" "}
            Loading comparison…
          </p>
        ) : (
          <>
            <div className="compare-summary">
              <blockquote className="compare-task" title={record.task}>
                {record.task}
              </blockquote>
              <p className="compare-meta">
                <span className={`compare-state state-${record.state}`}>
                  {STATE_LABELS[record.state] || record.state}
                </span>
                <span>Started {relativeTime(record.created_at)}</span>
                {record.base.included_uncommitted && (
                  <span>Includes your uncommitted work</span>
                )}
                {record.mode && record.mode !== "code" && (
                  <span>
                    {record.mode === "plan" ? "Plan only" : "Answer only"}
                  </span>
                )}
              </p>
              {open && (
                <div className="row compare-actions">
                  {running && (
                    <button
                      type="button"
                      className="ghost"
                      disabled={Boolean(acting)}
                      onClick={() =>
                        void act("cancel", () => api.cancelCompare(record.id))
                      }
                    >
                      <Square size={12} aria-hidden="true" />
                      {acting === "cancel" ? "Stopping…" : "Stop"}
                    </button>
                  )}
                  <button
                    type="button"
                    className="ghost danger-text"
                    disabled={Boolean(acting)}
                    onClick={() => setConfirm({ kind: "discard" })}
                  >
                    <Trash2 size={13} aria-hidden="true" /> Discard all
                  </button>
                </div>
              )}
            </div>
            {record.state === "applied" && (
              <div className="notice compare-applied" role="status">
                <Check size={15} aria-hidden="true" />
                <span>
                  Applied {record.applied_files.length}{" "}
                  {record.applied_files.length === 1 ? "file" : "files"}
                  {winner ? ` from ${winner.name}` : ""} — review them in
                  Changes.{" "}
                  {record.cleanup_pending
                    ? "Some copies are retained until cleanup completes."
                    : "The copies were removed."}
                </span>
                <button
                  type="button"
                  className="mini primary-mini"
                  onClick={() => onOpenChanges()}
                >
                  Open Changes
                </button>
              </div>
            )}
            {record.state === "discarded" && (
              <p className="notice" role="status">
                This comparison was discarded. Your project was not changed.
              </p>
            )}
            {record.state === "needs_review" && (
              <div className="notice bad" role="alert">
                Keep was interrupted or could not confirm its outcome. Review
                project changes before further action. Recovery material and
                lane copies are retained; automatic Keep and cleanup are
                blocked.
                <button
                  type="button"
                  className="mini"
                  disabled={Boolean(acting)}
                  onClick={() =>
                    void act("recover", () => api.recoverCompare(record.id))
                  }
                >
                  Recheck recovery
                </button>
                <button
                  type="button"
                  className="mini"
                  onClick={() => onOpenChanges()}
                >
                  Open Changes
                </button>
              </div>
            )}
            {record.cleanup_pending && (
              <div className="notice" role="status">
                <span>Cleanup pending. Your recorded result is preserved.</span>
                <button
                  type="button"
                  className="mini"
                  disabled={Boolean(acting)}
                  onClick={() =>
                    void act("cleanup", () => api.discardCompare(record.id))
                  }
                >
                  Retry cleanup
                </button>
              </div>
            )}
            {conflict && (
              <div className="notice bad compare-conflict" role="alert">
                <CircleAlert size={15} aria-hidden="true" />
                <div>
                  <strong>
                    {conflict.model
                      ? `${conflict.model}'s changes no longer apply.`
                      : "These changes no longer apply."}
                  </strong>{" "}
                  Your project changed since the comparison started. Nothing was
                  changed and every copy is kept: update or revert these files,
                  then keep again.
                  {conflict.files.length > 0 ? (
                    <ul aria-label="Conflicting files">
                      {conflict.files.map((file) => (
                        <li key={file}>
                          <code>{file}</code>
                        </li>
                      ))}
                    </ul>
                  ) : (
                    <p>{conflict.message}</p>
                  )}
                </div>
              </div>
            )}
            {error && (
              <p className="notice bad" role="alert">
                {error}
              </p>
            )}
            <ol
              className="compare-lanes"
              aria-label="Models in this comparison"
            >
              {record.lanes.map((lane) => (
                <li key={lane.model}>
                  <LaneCard
                    record={record}
                    lane={lane}
                    target={targets.find((t) => t.id === lane.model)}
                    approvals={approvals}
                    busy={Boolean(acting)}
                    onOpen={() => onOpenLane(record, lane)}
                    onFile={(path) =>
                      record.state === "applied" && lane.model === record.winner
                        ? onOpenChanges(path)
                        : onOpenDiff(record, lane, path)
                    }
                    onKeep={() => setConfirm({ kind: "keep", lane })}
                  />
                </li>
              ))}
            </ol>
            {record.notes.length > 0 && (
              <ul className="compare-notes" aria-label="Notes">
                {record.notes.map((note) => (
                  <li key={note}>{note}</li>
                ))}
              </ul>
            )}
          </>
        )}
        {scores && <Scoreboard rows={scores} />}
      </div>
      {confirm && record && (
        <Dialog
          label={
            confirm.kind === "keep"
              ? `Keep ${confirm.lane.name}'s result`
              : "Discard this comparison"
          }
          onClose={() => !acting && setConfirm(null)}
        >
          <div
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                e.stopPropagation();
                if (!acting) setConfirm(null);
              }
            }}
          >
            {confirm.kind === "keep" ? (
              <>
                <h2>Keep {confirm.lane.name}’s result?</h2>
                {Boolean(confirm.lane.checks.incomplete) && (
                  <p className="warn-text">
                    Checks are stale or incomplete. Keeping this result accepts
                    it without current verification.
                  </p>
                )}
                <p>
                  Apply {confirm.lane.name}’s changes to your project as
                  uncommitted changes and remove the other copies?
                </p>
                <p className="hint">
                  {confirm.lane.changed_files.length}{" "}
                  {confirm.lane.changed_files.length === 1 ? "file" : "files"} ·
                  nothing is staged or committed; review them in Changes.
                </p>
              </>
            ) : (
              <>
                <h2>Discard this comparison?</h2>
                <p>
                  Stops the models that are still working and removes every
                  copy. Your project is not changed.
                </p>
              </>
            )}
            <div className="row compare-dialog-actions">
              <button
                type="button"
                className="ghost"
                disabled={Boolean(acting)}
                onClick={() => setConfirm(null)}
              >
                Cancel
              </button>
              <button
                type="button"
                className={
                  confirm.kind === "keep" ? "primary" : "primary danger"
                }
                disabled={Boolean(acting)}
                onClick={doConfirm}
              >
                {acting && (
                  <LoaderCircle size={14} className="spin" aria-hidden="true" />
                )}
                {confirm.kind === "keep"
                  ? confirm.lane.checks.incomplete
                    ? "Keep without current checks"
                    : `Keep ${confirm.lane.name}`
                  : "Discard all"}
              </button>
            </div>
          </div>
        </Dialog>
      )}
    </section>
  );
}

/** One model's lane: status, changes, checks, usage and its actions. */
export function LaneCard({
  record,
  lane,
  target,
  approvals,
  busy,
  onOpen,
  onFile,
  onKeep,
}: {
  record: CompareRecord;
  lane: CompareLane;
  target?: PickerTarget;
  approvals: Approval[];
  busy?: boolean;
  onOpen: () => void;
  onFile: (path: string) => void;
  onKeep: () => void;
}) {
  const uid = useId().replace(/:/g, "");
  const badge = badgeFor(lane.model, target);
  const state = laneState(lane, approvals);
  const blocked = keepBlocked(record, lane);
  const won = record.winner === lane.model;
  const active = isActive(lane.status);
  const files = lane.changed_files;
  const filesUsable = !lane.removed || (won && record.state === "applied");
  const tokens = lane.usage?.total_tokens || 0;
  const checks = lane.checks || { passed: 0, failed: 0, commands: [] };
  return (
    <article
      className={`compare-lane tone-${state.tone} ${won ? "is-winner" : ""}`}
      aria-labelledby={`${uid}-name`}
    >
      <header className="compare-lane-head">
        <span className={`inference-badge ${badge.kind}`}>{badge.label}</span>
        <h3 id={`${uid}-name`} title={lane.name}>
          {lane.name}
        </h3>
        {won && (
          <span className="compare-kept">
            <Trophy size={12} aria-hidden="true" /> Kept
          </span>
        )}
      </header>
      <p className={`compare-lane-status tone-${state.tone}`} role="status">
        {active && state.kind !== "waiting" && (
          <LoaderCircle size={13} className="spin" aria-hidden="true" />
        )}
        {state.kind === "waiting" && (
          <CircleAlert size={13} aria-hidden="true" />
        )}
        <span>{state.label}</span>
        {lane.duration_s > 0 && (
          <span className="dim"> · {formatSeconds(lane.duration_s)}</span>
        )}
      </p>
      {active && lane.summary && (
        <p className="compare-lane-activity">{lane.summary}</p>
      )}
      {state.kind === "waiting" && (
        <div className="compare-lane-approval">
          <span>This model is asking for permission.</span>
          <button type="button" className="mini primary-mini" onClick={onOpen}>
            Answer in conversation
          </button>
        </div>
      )}
      {lane.error && (
        <p className="compare-lane-error" role="alert">
          {lane.error}
        </p>
      )}
      <div className="compare-lane-section">
        <h4>
          Changed files
          {files.length > 0 && (
            <span className="dim">
              {" "}
              · {files.length}
              {lane.changed_files_truncated ? "+" : ""}
            </span>
          )}
        </h4>
        {files.length ? (
          <ul className="compare-files">
            {files.map((file) => (
              <li key={file.path}>
                <button
                  type="button"
                  className="compare-file"
                  disabled={!filesUsable}
                  title={
                    filesUsable
                      ? `Show the changes to ${file.path}`
                      : "This copy was removed"
                  }
                  onClick={() => onFile(file.path)}
                >
                  <span className={`file-status status-${file.status}`}>
                    {file.status === "added"
                      ? "A"
                      : file.status === "deleted"
                        ? "D"
                        : "M"}
                    <span className="sr-only"> {file.status}</span>
                  </span>
                  <span className="compare-file-path">{file.path}</span>
                  {file.binary ? (
                    <span className="dim">binary</span>
                  ) : (
                    <span className="compare-file-stat">
                      <span className="add">+{file.additions}</span>{" "}
                      <span className="del">−{file.deletions}</span>
                    </span>
                  )}
                </button>
              </li>
            ))}
          </ul>
        ) : (
          <p className="hint">
            {active ? "No changes yet" : "No file changes"}
          </p>
        )}
      </div>
      <div className="compare-lane-section">
        <h4>{won ? "Checks at acceptance" : "Checks"}</h4>
        {checks.commands.length ? (
          <details className="compare-checks">
            <summary>
              {checks.passed > 0 && (
                <span className="ok">{checks.passed} passed</span>
              )}
              {checks.passed > 0 && checks.failed > 0 && " · "}
              {checks.failed > 0 && (
                <span className="bad">{checks.failed} failed</span>
              )}
              {Boolean(checks.incomplete) && (
                <span className="dim">
                  {" "}
                  · {checks.incomplete} incomplete or stale
                </span>
              )}
            </summary>
            <ul>
              {checks.commands.map((command, index) => (
                <li key={index}>
                  <code>{command.command}</code>{" "}
                  <span className={command.success ? "ok" : "bad"}>
                    {command.state ||
                      (command.success
                        ? "passed"
                        : `failed${command.exit_code != null ? ` (exit ${command.exit_code})` : ""}`)}
                  </span>
                </li>
              ))}
            </ul>
          </details>
        ) : (
          <p className="hint">{active ? "Not run yet" : "No checks ran"}</p>
        )}
      </div>
      {(tokens > 0 || typeof lane.usage?.cost === "number") && (
        <p className="compare-lane-usage dim">
          {tokens > 0 &&
            `${formatTokenCount(tokens)} tokens${lane.usage?.estimated ? " (estimated)" : ""}`}
          {tokens > 0 && typeof lane.usage?.cost === "number" && " · "}
          {typeof lane.usage?.cost === "number" &&
            `$${lane.usage.cost.toFixed(4)}`}
        </p>
      )}
      <div className="compare-lane-actions">
        <button
          type="button"
          className="ghost"
          disabled={!lane.session_id || lane.removed}
          title={
            lane.removed
              ? "This copy was removed, so its conversation cannot be opened"
              : undefined
          }
          onClick={onOpen}
        >
          <MessageSquare size={13} aria-hidden="true" /> Open conversation
        </button>
        {!won && ["running", "done"].includes(record.state) && (
          <button
            type="button"
            className="primary"
            disabled={Boolean(blocked) || busy}
            title={blocked || `Apply ${lane.name}'s changes to your project`}
            aria-describedby={blocked ? `${uid}-keep` : undefined}
            onClick={onKeep}
          >
            Keep this one
          </button>
        )}
      </div>
      {blocked && ["running", "done"].includes(record.state) && (
        <p className="sr-only" id={`${uid}-keep`}>
          {blocked}
        </p>
      )}
    </article>
  );
}

/** "Wins in this project": how often each model was kept. */
export function Scoreboard({ rows }: { rows: CompareScore[] }) {
  const uid = useId().replace(/:/g, "");
  return (
    <section className="compare-scoreboard" aria-labelledby={`${uid}-title`}>
      <h3 id={`${uid}-title`}>Wins in this project</h3>
      {rows.length ? (
        <table>
          <thead>
            <tr>
              <th scope="col">Model</th>
              <th scope="col">Wins</th>
              <th scope="col">Runs</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.model}>
                <th scope="row">{row.name || row.model}</th>
                <td>{row.wins}</td>
                <td>{row.runs}</td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="hint">No finished comparisons yet.</p>
      )}
    </section>
  );
}
