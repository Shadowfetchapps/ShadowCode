import { useEffect, useLayoutEffect, useRef, useState } from "react";
import {
  api,
  type ManagedWorktree,
  type WorktreeInspection,
  type WorktreeRecovery,
  type WorktreeReturnReview,
  type WorktreeCopyReview,
  type WorktreeRepairReview,
} from "../api";
import { sentenceCase } from "../lib/statusLabels";
import { WorktreeSetupForm } from "./WorktreeSetupForm";

export function WorktreeSettings({
  onOpen,
  onToast,
}: {
  onOpen?: (path: string) => void;
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const [workspace, setWorkspace] = useState("");
  const [records, setRecords] = useState<ManagedWorktree[]>([]);
  const [reference, setReference] = useState("HEAD");
  const [busy, setBusy] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [review, setReview] = useState<WorktreeInspection | null>(null);
  const [recovery, setRecovery] = useState<WorktreeRecovery | null>(null);
  const [returnReview, setReturnReview] = useState<WorktreeReturnReview | null>(
    null,
  );
  const [copyReview, setCopyReview] = useState<WorktreeCopyReview | null>(null);
  const [repairReview, setRepairReview] = useState<WorktreeRepairReview | null>(
    null,
  );
  const reviewRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    let live = true;
    void api
      .worktrees()
      .then((data) => {
        if (live) {
          setWorkspace(data.workspace);
          setRecords(data.worktrees);
        }
      })
      .catch((e) => {
        if (live) setError(String(e));
      })
      .finally(() => {
        if (live) setLoading(false);
      });
    return () => {
      live = false;
    };
  }, []);
  useLayoutEffect(() => {
    if (review || recovery || returnReview || copyReview || repairReview) {
      reviewRef.current?.scrollIntoView({ block: "nearest" });
      reviewRef.current?.focus({ preventScroll: true });
    }
  }, [review, recovery, returnReview, copyReview, repairReview]);
  async function refresh() {
    const data = await api.worktrees();
    setWorkspace(data.workspace);
    setRecords(data.worktrees);
    setReview(null);
    setRecovery(null);
    setReturnReview(null);
    setCopyReview(null);
    setRepairReview(null);
  }
  async function perform(action: () => Promise<void>) {
    setBusy(true);
    setError("");
    setReview(null);
    setRecovery(null);
    setReturnReview(null);
    setCopyReview(null);
    setRepairReview(null);
    try {
      await action();
    } catch (e) {
      setError(String(e));
      setReview(null);
      setRecovery(null);
    } finally {
      setBusy(false);
    }
  }
  return (
    <section className="settings-section" aria-label="Managed worktrees">
      <h3>Isolated worktrees</h3>
      <p className="hint">
        Work on a separate branch while keeping this checkout intact. New
        worktrees start from a local commit. You can also review and copy your
        current edits into a new worktree while preserving the originals.
      </p>
      <WorktreeSetupForm key={workspace} onToast={onToast} />
      <p className="hint">Source project</p>
      <code className="worktree-path">
        {workspace || (loading ? "Loading project…" : "Project unavailable")}
      </code>
      {error && (
        <p role="alert" className="error">
          {error}
        </p>
      )}
      <div className="row worktree-create">
        <label>
          Starting commit or branch
          <input
            value={reference}
            onChange={(e) => setReference(e.target.value)}
            disabled={busy || loading}
            placeholder="HEAD"
            maxLength={256}
            spellCheck={false}
          />
        </label>
        <button
          type="button"
          className="primary"
          disabled={busy || loading || !workspace || !reference.trim()}
          onClick={() =>
            void perform(async () => {
              const created = await api.createWorktree(
                workspace,
                reference.trim(),
              );
              await refresh();
              onToast(`Created ${created.branch}`, "ok");
            })
          }
        >
          Create worktree
        </button>
        <button
          type="button"
          className="ghost"
          disabled={busy || loading || !workspace}
          onClick={() =>
            void perform(async () =>
              setCopyReview(await api.reviewWorktreeCopy(workspace)),
            )
          }
        >
          Review current edits
        </button>
        <button
          type="button"
          className="ghost"
          disabled={busy}
          onClick={() => void perform(refresh)}
        >
          Refresh
        </button>
      </div>
      {loading && <p role="status">Loading worktrees…</p>}
      {busy && (
        <p role="status">Working on the requested worktree operation…</p>
      )}
      {!loading && !records.length && !error && (
        <p className="hint">No managed worktrees for this project yet.</p>
      )}
      <div className="worktree-list">
        {records.map((record) => (
          <article
            className="worktree-card"
            key={record.id}
            data-worktree-id={record.id}
          >
            <strong>{record.branch}</strong>
            <span className="hint">{sentenceCase(record.state)}</span>
            <code className="worktree-path">{record.path}</code>
            <p className="hint">{record.detail}</p>
            <div className="row">
              <button
                type="button"
                className="ghost"
                disabled={busy || !onOpen}
                onClick={() => onOpen?.(record.path)}
              >
                Open worktree
              </button>
              <button
                type="button"
                className="ghost"
                disabled={busy}
                onClick={() =>
                  void perform(async () =>
                    setReturnReview(
                      await api.reviewWorktreeReturn(workspace, record.id),
                    ),
                  )
                }
              >
                Review return
              </button>
              {(record.state === "merge_pending" ||
                record.state === "needs_attention" ||
                record.state === "returning") && (
                <button
                  type="button"
                  className="ghost"
                  disabled={busy || !onOpen}
                  onClick={() => onOpen?.(workspace)}
                >
                  Open source project
                </button>
              )}
              <button
                type="button"
                className="ghost"
                disabled={busy}
                onClick={() =>
                  void perform(async () => {
                    setRecovery(null);
                    setReview(await api.inspectWorktree(workspace, record.id));
                  })
                }
              >
                Inspect removal
              </button>
              <button
                type="button"
                className="ghost"
                disabled={busy}
                onClick={() =>
                  void perform(async () =>
                    setRepairReview(
                      await api.reviewWorktreeRepair(workspace, record.id),
                    ),
                  )
                }
              >
                Review connection repair
              </button>
              <button
                type="button"
                className="ghost"
                disabled={busy}
                onClick={() =>
                  void perform(async () => {
                    setReview(null);
                    setRecovery(
                      await api.worktreeRecovery(workspace, record.id),
                    );
                  })
                }
              >
                Review missing checkout
              </button>
            </div>
          </article>
        ))}
      </div>
      {repairReview && (
        <div
          className="worktree-review"
          ref={reviewRef}
          tabIndex={-1}
          role="region"
          aria-label="Review connection repair"
        >
          <h4>Restore missing Git connections</h4>
          <code className="worktree-path">{repairReview.record.path}</code>
          <p>
            Branch retained: <strong>{repairReview.record.branch}</strong>
          </p>
          <code className="worktree-path">{repairReview.head}</code>
          <p>
            Checkout connection:{" "}
            {repairReview.checkout_pointer === null
              ? "Missing — will restore"
              : "Intact — preserved"}
          </p>
          <p>
            Repository connection:{" "}
            {repairReview.registration_pointer === null
              ? "Missing — will restore"
              : "Intact — preserved"}
          </p>
          <p>{repairReview.warning}</p>
          <div className="row">
            <button
              type="button"
              className="ghost"
              disabled={busy}
              onClick={() => setRepairReview(null)}
            >
              Cancel repair
            </button>
            <button
              type="button"
              className="primary"
              disabled={busy}
              onClick={() =>
                void perform(async () => {
                  const repaired = await api.repairWorktree(
                    workspace,
                    repairReview.record.id,
                    repairReview.hash,
                  );
                  await refresh();
                  onToast(
                    `Restored Git connection for ${repaired.branch}`,
                    "ok",
                  );
                })
              }
            >
              Restore Git connection
            </button>
          </div>
        </div>
      )}
      {copyReview && (
        <div
          className="worktree-review"
          ref={reviewRef}
          tabIndex={-1}
          role="region"
          aria-label="Review copied changes"
        >
          <h4>Copy current edits into a new worktree</h4>
          <code className="worktree-path">{copyReview.source}</code>
          <p>
            Starting commit:{" "}
            <code className="worktree-path">{copyReview.head}</code>
          </p>
          <h4>Staged changes</h4>
          <pre tabIndex={0} role="region" aria-label="Staged copy diff">
            {copyReview.staged_diff || "No staged changes."}
          </pre>
          <h4>Unstaged changes</h4>
          <pre tabIndex={0} role="region" aria-label="Unstaged copy diff">
            {copyReview.unstaged_diff || "No unstaged changes."}
          </pre>
          <h4>Untracked files</h4>
          {copyReview.untracked.length ? (
            <ul>
              {copyReview.untracked.map((file) => (
                <li key={file.path}>
                  <code className="worktree-path">{file.path}</code>{" "}
                  {file.bytes.toLocaleString()} bytes
                </li>
              ))}
            </ul>
          ) : (
            <p>No untracked files.</p>
          )}
          {copyReview.intent_to_add.length > 0 && (
            <p>
              Intent-to-add entries are preserved:{" "}
              {copyReview.intent_to_add.join(", ")}
            </p>
          )}
          <p>
            The copy starts from this project’s HEAD and preserves staged and
            unstaged edits separately. Original files stay here. Ignored files
            are excluded. If the source changes, review it again before copying.
          </p>
          <div className="row">
            <button
              type="button"
              className="ghost"
              disabled={busy}
              onClick={() => setCopyReview(null)}
            >
              Cancel copy
            </button>
            <button
              type="button"
              className="primary"
              disabled={busy}
              onClick={() =>
                void perform(async () => {
                  const copied = await api.copyWorktreeChanges(
                    workspace,
                    copyReview.hash,
                  );
                  await refresh();
                  onToast(
                    `Copied edits into ${copied.branch}; source preserved`,
                    "ok",
                  );
                })
              }
            >
              Copy into new worktree
            </button>
          </div>
        </div>
      )}
      {returnReview && (
        <div
          className="worktree-review"
          ref={reviewRef}
          tabIndex={-1}
          role="region"
          aria-label="Review returned changes"
        >
          <h4>Return committed changes</h4>
          <p>
            From <strong>{returnReview.worktree_branch}</strong>
          </p>
          <code className="worktree-path">{returnReview.worktree_head}</code>
          <p>
            Into <strong>{returnReview.source_branch}</strong>
          </p>
          <code className="worktree-path">{returnReview.record.source}</code>
          <code className="worktree-path">{returnReview.source_head}</code>
          <p>Incoming changes since the common ancestor:</p>
          <pre tabIndex={0} role="region" aria-label="Incoming worktree diff">
            {returnReview.diff || "No file-content differences."}
          </pre>
          <p>
            This prepares a merge in the source project without committing.
            Review and commit there afterward. Conflicts remain for resolution;
            use Git merge --abort to abandon the merge. The worktree and branch
            are kept.
          </p>
          <div className="row">
            <button
              type="button"
              className="ghost"
              disabled={busy}
              onClick={() => setReturnReview(null)}
            >
              Keep changes isolated
            </button>
            <button
              type="button"
              className="primary"
              disabled={busy}
              onClick={() =>
                void perform(async () => {
                  const result = await api.returnWorktreeChanges(
                    workspace,
                    returnReview.record.id,
                    returnReview.hash,
                  );
                  await refresh();
                  if (result.state === "merge_pending")
                    onToast(
                      "Changes returned; review and commit in the source project",
                      "ok",
                    );
                  else {
                    setError(result.detail);
                    onToast(
                      "Return needs attention in the source project",
                      "err",
                    );
                  }
                })
              }
            >
              Prepare merge in source
            </button>
          </div>
        </div>
      )}
      {recovery && (
        <div
          className="worktree-review"
          ref={reviewRef}
          tabIndex={-1}
          role="region"
          aria-label="Review worktree recovery"
        >
          <h4>Recover committed work</h4>
          <p className="hint">Missing checkout</p>
          <code className="worktree-path">{recovery.record.path}</code>
          <p>
            Retained branch:{" "}
            <strong>{recovery.branch || "Detached HEAD"}</strong>
          </p>
          <p>
            Commit to restore:{" "}
            <code className="worktree-path">{recovery.commit}</code>
          </p>
          <p>{recovery.warning}</p>
          <p className="hint">
            The new checkout will appear in this list. Open and trust it before
            running tasks.
          </p>
          <div className="row">
            <button
              type="button"
              className="ghost"
              disabled={busy}
              onClick={() => setRecovery(null)}
            >
              Cancel recovery
            </button>
            <button
              type="button"
              className="primary"
              disabled={busy}
              onClick={() =>
                void perform(async () => {
                  const restored = await api.restoreWorktree(
                    workspace,
                    recovery.record.id,
                    recovery.hash,
                  );
                  await refresh();
                  onToast(
                    `Recovered committed work in ${restored.branch}`,
                    "ok",
                  );
                })
              }
            >
              Restore in new worktree
            </button>
          </div>
        </div>
      )}
      {review && (
        <div
          className="worktree-review"
          ref={reviewRef}
          tabIndex={-1}
          role="region"
          aria-label="Review worktree removal"
        >
          <h4>Review removal</h4>
          <code className="worktree-path">{review.record.path}</code>
          <p>
            Branch retained:{" "}
            <strong>{review.current_branch || "Detached HEAD"}</strong>
          </p>
          <p>
            Commit retained: <code>{review.head}</code>
          </p>
          <p>{review.reason}</p>
          {review.status && <pre>{review.status}</pre>}
          <p className="hint">
            Removal also checks for active tasks and background processes. Your
            source checkout and committed branch history are preserved.
          </p>
          <div className="row">
            <button
              type="button"
              className="ghost"
              disabled={busy}
              onClick={() => setReview(null)}
            >
              Keep worktree
            </button>
            <button
              type="button"
              className="ghost"
              disabled={busy || !review.can_remove}
              onClick={() =>
                void perform(async () => {
                  await api.removeWorktree(
                    workspace,
                    review.record.id,
                    review.hash,
                  );
                  await refresh();
                  onToast(
                    "Worktree removed; branch and commits retained",
                    "ok",
                  );
                })
              }
            >
              Remove clean worktree
            </button>
          </div>
        </div>
      )}
    </section>
  );
}
