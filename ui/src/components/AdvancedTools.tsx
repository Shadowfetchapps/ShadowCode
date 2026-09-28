import { useEffect, useState } from "react";
import { api, type ParallelPlan, type GuardianStatus } from "../api";
import { sentenceCase } from "../lib/statusLabels";

export function AdvancedTools({ onOpen }: { onOpen?: (path: string) => void }) {
  const [plan, setPlan] = useState<ParallelPlan | null>(null);
  const [guardian, setGuardian] = useState<GuardianStatus | null>(null);
  const [goal, setGoal] = useState("");
  const [busy, setBusy] = useState(true);
  const [error, setError] = useState("");
  const [result, setResult] = useState("");
  async function refresh() {
    const [parallel, health] = await Promise.all([
      api.parallelPlan(),
      api.guardianStatus(),
    ]);
    setPlan(parallel.plan);
    setGuardian(health);
  }
  useEffect(() => {
    let live = true;
    void Promise.all([api.parallelPlan(), api.guardianStatus()])
      .then(([p, g]) => {
        if (live) {
          setPlan(p.plan);
          setGuardian(g);
        }
      })
      .catch((e) => {
        if (live) setError(String(e));
      })
      .finally(() => {
        if (live) setBusy(false);
      });
    return () => {
      live = false;
    };
  }, []);
  async function perform(action: () => Promise<string>) {
    setBusy(true);
    setError("");
    setResult("");
    try {
      setResult(await action());
      await refresh();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <div className="advanced-tools">
      {error && (
        <p className="error" role="alert">
          {error}
        </p>
      )}
      {result && (
        <p className="hint" role="status">
          {result}
        </p>
      )}
      <section
        className="settings-section advanced-card"
        aria-label="Parallel workspaces"
      >
        <div className="advanced-heading">
          <h3>Parallel workspaces</h3>
          <span className="chip">Up to 2</span>
        </div>
        <p className="hint">
          Prepare separate branches from committed HEAD. Open a workspace to
          start its task, then commit its work before checking the branches
          together.
        </p>
        {!plan ? (
          <>
            <label className="field">
              Work items
              <textarea
                aria-label="Work items"
                rows={3}
                value={goal}
                onChange={(e) => setGoal(e.target.value)}
                placeholder="One work item per line"
                maxLength={64000}
              />
            </label>
            <button
              type="button"
              className="primary"
              disabled={busy || !goal.trim()}
              onClick={() =>
                void perform(async () => {
                  const p = await api.prepareParallel(goal);
                  if (!p.ok)
                    throw new Error(p.error || "Could not prepare workspaces");
                  return "Workspaces prepared. Open each workspace to start a task.";
                })
              }
            >
              Prepare workspaces
            </button>
          </>
        ) : (
          <>
            <p className="hint">{plan.lead_note}</p>
            {plan.workers.map((worker) => (
              <article className="advanced-worker" key={worker.item.id}>
                <div className="advanced-heading">
                  <strong>{worker.item.title}</strong>
                  <span className="dim">{sentenceCase(worker.status)}</span>
                </div>
                <code className="worktree-path">{worker.branch}</code>
                <div className="row">
                  <button
                    type="button"
                    disabled={busy || !onOpen || worker.status === "removed"}
                    onClick={() => onOpen?.(worker.worktree_path)}
                  >
                    Open workspace
                  </button>
                  <button
                    type="button"
                    disabled={
                      busy ||
                      worker.status === "finished" ||
                      worker.status === "removed"
                    }
                    onClick={() =>
                      void perform(async () => {
                        await api.parallelWorkerStatus(
                          worker.item.id,
                          "finished",
                        );
                        return "Marked finished. Commit all changes before checking integration.";
                      })
                    }
                  >
                    Mark finished
                  </button>
                </div>
              </article>
            ))}
            <div className="row">
              <button
                type="button"
                disabled={
                  busy || plan.workers.some((w) => w.status !== "finished")
                }
                onClick={() =>
                  void perform(async () => {
                    const check = await api.verifyParallel();
                    return check.ok
                      ? "Combined merge check passed. Branches are ready for review; nothing has been merged."
                      : check.conflicts
                          ?.map((c) => `${c.worker}: ${c.detail}`)
                          .join("\n") ||
                          "Finish all workspaces before checking.";
                  })
                }
              >
                Check integration
              </button>
              <button
                type="button"
                disabled={busy}
                onClick={() =>
                  void perform(async () => {
                    const removed = await api.cleanupParallel();
                    return `Removed ${removed.cleaned} clean checkouts. All worker branches and commits are retained.`;
                  })
                }
              >
                Remove clean checkouts
              </button>
            </div>
            <p className="hint">
              Last check: {plan.verify_status}. Cleanup preserves every branch
              and refuses checkouts containing changes or ignored files.
            </p>
          </>
        )}
      </section>
      <section
        className="settings-section advanced-card"
        aria-label="Guardian diagnostics"
      >
        <div className="advanced-heading">
          <h3>Guardian diagnostics</h3>
          <span className="chip">{guardian?.enabled ? "Enabled" : "Off"}</span>
        </div>
        <p className="hint">
          Checks local isolation support and identifies a suggested test
          command. It does not run tests or change code. Save schedule changes
          below before running a check.
        </p>
        {guardian?.last_run && (
          <p className="hint">
            Last checked {new Date(guardian.last_run * 1000).toLocaleString()}
          </p>
        )}
        {guardian?.last_result && (
          <p className="hint">
            {guardian.last_result.tests.hint
              ? `Suggested test: ${guardian.last_result.tests.hint}`
              : "No test command detected."}{" "}
            Tests have not been run.
          </p>
        )}
        <div className="row">
          <button
            type="button"
            disabled={busy || !guardian?.enabled}
            onClick={() =>
              void perform(async () => {
                await api.runGuardian();
                return "Local diagnostics completed.";
              })
            }
          >
            Run diagnostics
          </button>
          <button
            type="button"
            disabled={busy}
            onClick={() => void perform(async () => "Status refreshed.")}
          >
            Refresh status
          </button>
        </div>
      </section>
    </div>
  );
}
