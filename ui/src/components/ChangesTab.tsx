/** The drawer's Changes tab: git status, per-hunk stage / discard, and a
 * commit message draft that survives switching tabs. */
import { useCallback, useEffect, useState } from "react";
import { api, type DiffHunk } from "../api";
import { Empty } from "./cards";
import { ConfirmDialog } from "./ConfirmDialog";
import {
  remembered,
  type DrawerMemory,
  type DrawerMemoryUpdate,
} from "../hooks/useDrawerMemory";

type Toast = (text: string, kind?: "ok" | "err" | "info") => void;

/** Git porcelain codes in words (tooltip of the short label). */
const FILE_STATES: Record<string, string> = {
  "??": "New file, not tracked by git yet",
  M: "Modified",
  A: "Added",
  D: "Deleted",
  R: "Renamed",
  C: "Copied",
  U: "Conflict",
};

export function ChangesTab({
  path,
  busy,
  toast,
  onAskAgent,
  memory,
  onMemory,
}: {
  path: string;
  busy: boolean;
  toast: Toast;
  onAskAgent?: (prompt: string) => void;
  memory: DrawerMemory;
  onMemory: DrawerMemoryUpdate;
}) {
  const [git, setGit] = useState<{
    status: string;
    log: string;
    files: { path: string; label: string }[];
    repo: boolean;
    loaded: boolean;
  }>({ status: "", log: "", files: [], repo: true, loaded: false });
  const [selected, setSelected] = useState(path);
  const [hunks, setHunks] = useState<DiffHunk[]>([]);
  const [msg, setMsg] = remembered(memory, onMemory, "commitMessage");
  const [review, setReview] = useState<Awaited<
    ReturnType<typeof api.gitDiff>
  > | null>(null);
  const [view, setView] = useState<"unstaged" | "staged">("unstaged");
  const [discard, setDiscard] = useState<DiffHunk | null>(null);

  const load = useCallback(async () => {
    try {
      const g = await api.git();
      setGit({
        status: g.status,
        log: g.log,
        files: g.files || [],
        repo: Boolean(g.repo),
        loaded: true,
      });
    } catch {
      setGit({ status: "", log: "", files: [], repo: false, loaded: true });
    }
  }, []);

  const loadHunks = useCallback(async (p: string) => {
    if (!p) {
      setHunks([]);
      setReview(null);
      return;
    }
    try {
      const result = await api.gitDiff(p);
      setReview(result);
      setHunks(result.hunks);
    } catch {
      // Clear both views; a stale staged list next to an empty unstaged list
      // would misrepresent the file.
      setHunks([]);
      setReview(null);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load, busy]);
  useEffect(() => {
    setSelected(path);
  }, [path]);
  // Opening the tab without a file shows the first change right away.
  useEffect(() => {
    if (!selected && git.files.length) setSelected(git.files[0].path);
  }, [selected, git.files]);
  useEffect(() => {
    void loadHunks(selected);
  }, [selected, loadHunks, busy]);

  async function act(h: DiffHunk, action: "accept" | "reject") {
    try {
      await api.hunkAction(selected, h, action);
      toast(action === "accept" ? "Hunk staged" : "Hunk discarded", "ok");
      await loadHunks(selected);
      await load();
    } catch (err) {
      toast(String(err), "err");
    }
  }

  if (!git.loaded)
    return (
      <div className="skel-rows">
        <span className="skel" />
        <span className="skel short" />
      </div>
    );
  if (!git.repo)
    return (
      <Empty
        title="Not a git repository"
        body="Initialize git in this folder to review and stage changes."
      />
    );
  return (
    <>
      <div className="list">
        {git.files.length === 0 && <Empty title="Working tree clean" />}
        {git.files.map((f) => (
          <button
            type="button"
            key={f.path}
            className={`file ${selected === f.path ? "active" : ""}`}
            onClick={() => setSelected(f.path)}
          >
            <span
              className="file-label"
              title={FILE_STATES[f.label] || f.label}
            >
              {f.label === "??" ? "New" : f.label}
            </span>
            {f.path}
          </button>
        ))}
      </div>
      {selected && (
        <div className="hunks">
          <div className="crumb">
            <span>{selected}</span>
          </div>
          <div className="seg">
            <button
              type="button"
              className={view === "unstaged" ? "on" : ""}
              onClick={() => setView("unstaged")}
            >
              Unstaged
            </button>
            <button
              type="button"
              className={view === "staged" ? "on" : ""}
              onClick={() => setView("staged")}
            >
              Staged
            </button>
          </div>
          {review?.binary && (
            <p className="hint">Binary file. Text preview is unavailable.</p>
          )}
          {review?.truncated && (
            <p className="hint">
              Preview truncated. Review the full file before staging.
            </p>
          )}
          {review?.untracked && (
            <div className="row">
              <p className="hint">New file</p>
              <button
                type="button"
                className="mini"
                disabled={busy}
                onClick={() =>
                  void api
                    .gitAdd([selected])
                    .then(async () => {
                      await load();
                      await loadHunks(selected);
                      setView("staged");
                    })
                    .catch((e) => toast(String(e), "err"))
                }
              >
                Stage file
              </button>
            </div>
          )}
          {(view === "unstaged" ? hunks : review?.staged_hunks || []).length ===
            0 && <p className="hint">No {view} text changes for this file.</p>}
          {(view === "unstaged" ? hunks : review?.staged_hunks || []).map(
            (h, i) => (
              <div key={i} className="hunk">
                <header>
                  <span>{h.header}</span>
                  {view === "unstaged" &&
                    !review?.untracked &&
                    !review?.truncated && (
                      <span className="row">
                        <button
                          type="button"
                          className="mini"
                          disabled={busy}
                          onClick={() => void act(h, "accept")}
                        >
                          Stage
                        </button>
                        <button
                          type="button"
                          className="mini danger-text"
                          disabled={busy}
                          onClick={() => setDiscard(h)}
                        >
                          Discard
                        </button>
                        {onAskAgent && (
                          <button
                            type="button"
                            className="mini"
                            onClick={() => {
                              const body = h.lines
                                .map(
                                  (line) =>
                                    `${line.kind === "add" ? "+" : line.kind === "del" ? "-" : " "}${line.text}`,
                                )
                                .join("");
                              onAskAgent(
                                `Review this hunk in ${selected}:\n${h.header}\n${body}\nAdjust or explain as needed.`,
                              );
                              toast("Hunk queued in composer", "info");
                            }}
                          >
                            Ask agent
                          </button>
                        )}
                      </span>
                    )}
                </header>
                {h.lines.map((line, j) => (
                  <div
                    key={j}
                    className={`diff-line ${line.kind === "add" ? "diff-add" : line.kind === "del" ? "diff-del" : "diff-ctx"}`}
                  >
                    {(line.kind === "add"
                      ? "+"
                      : line.kind === "del"
                        ? "-"
                        : " ") + line.text}
                  </div>
                ))}
              </div>
            ),
          )}
        </div>
      )}
      <div className="commit">
        <input
          aria-label="Commit message"
          value={msg}
          onChange={(e) => setMsg(e.target.value)}
          placeholder="Commit message"
        />
        <button
          type="button"
          className="mini"
          disabled={busy}
          onClick={() =>
            void api
              .gitAdd(["."])
              .then(async () => {
                await load();
                await loadHunks(selected);
              })
              .catch((e) => toast(String(e), "err"))
          }
        >
          Stage all
        </button>
        <button
          type="button"
          className="mini primary-mini"
          disabled={busy || !msg.trim()}
          onClick={() =>
            void api
              .gitCommit(msg)
              .then(() => {
                setMsg("");
                void load();
                toast("Committed", "ok");
              })
              .catch((err) => toast(String(err), "err"))
          }
        >
          Commit
        </button>
      </div>
      <pre className="plan log">{git.log}</pre>
      {discard && (
        <ConfirmDialog
          title="Discard this change?"
          confirmLabel="Discard change"
          danger
          onCancel={() => setDiscard(null)}
          onConfirm={async () => {
            await act(discard, "reject");
            setDiscard(null);
          }}
        >
          <p>
            The lines in <code>{discard.header}</code> of {selected} go back to
            the last staged or committed version. This cannot be undone.
          </p>
        </ConfirmDialog>
      )}
    </>
  );
}
