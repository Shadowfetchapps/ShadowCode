/** The drawer's Git tab: branch, commit (with a suggested message), push,
 * and a pull request with its CI checks. Per-file review and hunk staging
 * live in the Changes tab. Git runs with the user's own sign-in; nothing
 * here stores a credential. */
import { useCallback, useEffect, useMemo, useState } from "react";
import {
  CircleCheck,
  CircleDashed,
  CircleX,
  ExternalLink,
  GitBranch,
  RefreshCw,
} from "lucide-react";
import { api, type SecretFinding } from "../api";
import { useCommitGuard } from "../hooks/useCommitGuard";
import { ConfirmDialog } from "./ConfirmDialog";
import { SecretFindings } from "./SecretFindings";
import {
  branchNameProblem,
  forgeApi,
  signedInText,
  syncText,
  type GitOverview,
  type PrCreated,
  type PrStatus,
  type PullRequest,
  type PushRefusal,
  type Suggestion,
} from "../lib/forge";
import { openExternal } from "../lib/transport";
import { usePrChecks } from "../hooks/usePrChecks";
import {
  remembered,
  type DrawerMemory,
  type DrawerMemoryUpdate,
} from "../hooks/useDrawerMemory";
import { Empty } from "./cards";
import { ConsentDialog } from "./ConsentDialog";
import {
  OpinionHead,
  OpinionNotes,
  ReviewedChanges,
  ReviewerSelect,
  type FindingActions,
} from "./SecondOpinion";
import {
  useOpinionOptions,
  useSecondOpinions,
} from "../hooks/useSecondOpinions";
import type { PickerTarget } from "../lib/picker";
import {
  isActiveOpinion,
  openFindings,
  opinionApi,
  suggestReviewer,
} from "../lib/secondOpinion";
import { Markdown } from "./Markdown";
import "../tools.css";

type Toast = (text: string, kind?: "ok" | "err" | "info") => void;

const FORGE: Record<string, string> = {
  github: "GitHub",
  gitlab: "GitLab",
};

function sourceText(s: Suggestion, action = "commit") {
  if (s.source === "summary") return s.note || "Written from the file list.";
  const who = s.model || "the model";
  return s.source === "local"
    ? `Drafted by the local model ${who}. Edit it before you ${action}.`
    : `Drafted by ${who}. Edit it before you ${action}.`;
}

export function BranchPanel({
  busy,
  toast,
  memory,
  onMemory,
  onOpenTerminal,
  workspace = "",
  sessionId = "",
  targets = [],
}: {
  busy: boolean;
  toast: Toast;
  memory: DrawerMemory;
  onMemory: DrawerMemoryUpdate;
  onOpenTerminal: () => void;
  /** The project; reviews before commit need it. */
  workspace?: string;
  /** The open conversation ("Ask the agent to fix this" queues there
   * when it wrote the change). */
  sessionId?: string;
  /** Picker rows to choose a reviewer from. */
  targets?: PickerTarget[];
}) {
  const [overview, setOverview] = useState<GitOverview | null>(null);
  const [status, setStatus] = useState<PrStatus | null>(null);
  const [message, setMessage] = remembered(memory, onMemory, "commitMessage");
  const [pr, setPr] = remembered(memory, onMemory, "prDraft");
  const [branchName, setBranchName] = useState("");
  const [working, setWorking] = useState("");
  const [note, setNote] = useState("");
  const [prNote, setPrNote] = useState("");
  const [created, setCreated] = useState<PullRequest | null>(null);
  const guard = useCommitGuard(toast);
  /** A push or pull request refused because its commits may contain a
   * secret (or could not all be checked): what was found, and how to go
   * on anyway with exactly the commits that were checked. */
  const [pushSecrets, setPushSecrets] = useState<{
    findings: SecretFinding[];
    truncated: boolean;
    unchecked: string | null;
    retry: () => Promise<void>;
  } | null>(null);
  const refused = (
    answer: PushRefusal,
    retry: (scanned: string) => Promise<void>,
  ) => {
    if (!answer.secrets?.length && !answer.secrets_unchecked) return false;
    setPushSecrets({
      findings: answer.secrets || [],
      truncated: Boolean(answer.secrets_truncated),
      unchecked: answer.secrets_unchecked || null,
      retry: () => retry(answer.scanned ?? ""),
    });
    return true;
  };
  const review = useStagedReview({
    workspace,
    sessionId,
    targets,
    repo: Boolean(overview?.repo),
    staged: overview?.staged || 0,
    changed: overview?.changed || 0,
    toast,
  });

  const load = useCallback(async () => {
    try {
      const next = await forgeApi.overview();
      setOverview(next);
      return next;
    } catch (e) {
      setOverview({ repo: false });
      toast(String(e), "err");
      return null;
    }
  }, [toast]);
  const loadPr = useCallback(async (base = "") => {
    try {
      setStatus(await forgeApi.prStatus("", base));
    } catch {
      setStatus(null);
    }
  }, []);

  useEffect(() => {
    void load().then((o) => {
      if (o?.repo && o.remote) void loadPr();
    });
  }, [load, loadPr, busy]);

  const base = pr.base || status?.base || overview?.default_base || "main";
  const existing = created || status?.pr || null;
  const checks = usePrChecks(
    status?.provider === "github" ? existing?.number : null,
    overview?.remote || "",
  );

  async function run(label: string, action: () => Promise<void>) {
    setWorking(label);
    try {
      await action();
    } catch (e) {
      toast(String(e), "err");
    } finally {
      setWorking("");
    }
  }

  if (!overview)
    return (
      <div className="skel-rows">
        <span className="skel" />
        <span className="skel short" />
      </div>
    );
  if (!overview.repo)
    return (
      <Empty
        title="Not a git repository"
        body="Run git init in this folder (the Terminal tab works) to commit and open pull requests here."
      />
    );

  const nameProblem = branchName ? branchNameProblem(branchName) : "";
  const others = (overview.branches || []).filter((b) => !b.current);
  const onBase = overview.branch === base;
  const cli = status?.cli;
  const cliName =
    cli?.name === "glab" ? "GitLab CLI (glab)" : "GitHub CLI (gh)";
  const canCreate =
    Boolean(cli?.installed && cli?.authenticated) &&
    Boolean(overview.branch) &&
    !onBase;

  return (
    <div className="git-panel">
      <section className="git-section" aria-label="Branch">
        <h4>
          <GitBranch size={14} aria-hidden="true" /> Branch
        </h4>
        <p className="git-branch">
          <strong>{overview.branch || "No branch"}</strong>
          <span className="hint">{syncText(overview)}</span>
        </p>
        <div className="git-row">
          <input
            aria-label="New branch name"
            placeholder="New branch, e.g. fix/login-timeout"
            value={branchName}
            aria-invalid={Boolean(nameProblem)}
            aria-describedby={nameProblem ? "branch-problem" : undefined}
            onChange={(e) => setBranchName(e.target.value)}
          />
          <button
            type="button"
            className="mini"
            disabled={
              busy ||
              !branchName.trim() ||
              Boolean(nameProblem) ||
              Boolean(working)
            }
            onClick={() =>
              void run("branch", async () => {
                const made = await forgeApi.branch(branchName.trim(), true);
                setBranchName("");
                toast(`Now on ${made.branch}`, "ok");
                await load();
                await loadPr();
              })
            }
          >
            Create branch
          </button>
        </div>
        {nameProblem && (
          <p className="hint error" id="branch-problem">
            {nameProblem}
          </p>
        )}
        {others.length > 0 && (
          <div className="git-row">
            <select
              aria-label="Switch to branch"
              defaultValue=""
              disabled={busy || Boolean(working)}
              onChange={(e) => {
                const name = e.target.value;
                e.target.value = "";
                if (name)
                  void run("switch", async () => {
                    await forgeApi.branch(name, false);
                    toast(`Now on ${name}`, "ok");
                    setCreated(null);
                    await load();
                    await loadPr();
                  });
              }}
            >
              <option value="">Switch to…</option>
              {others.map((b) => (
                <option key={b.name} value={b.name}>
                  {b.name}
                </option>
              ))}
            </select>
          </div>
        )}
        {busy && (
          <p className="hint">
            Switching branches and committing wait until the agent finishes.
            Push and pull requests work now.
          </p>
        )}
      </section>

      <section className="git-section" aria-label="Commit">
        <h4>Commit</h4>
        <p className="hint">
          {overview.staged
            ? `${overview.staged} file${overview.staged === 1 ? "" : "s"} staged`
            : "Nothing staged"}
          {overview.changed ? ` · ${overview.changed} changed in total` : ""}
        </p>
        <textarea
          className="git-message"
          aria-label="Commit message"
          placeholder="Commit message"
          rows={4}
          value={message}
          onChange={(e) => setMessage(e.target.value)}
        />
        {note && <p className="hint git-note">{note}</p>}
        {review.gate && (
          <p className="hint opinion-gate" role="status">
            {review.gate}
          </p>
        )}
        <div className="git-row end">
          <button
            type="button"
            className="mini"
            disabled={busy || !overview.changed || Boolean(working)}
            onClick={() =>
              void run("stage", async () => {
                await api.gitAdd(["."]);
                await load();
              })
            }
          >
            Stage all
          </button>
          <button
            type="button"
            className="mini"
            disabled={!overview.staged || Boolean(working)}
            onClick={() =>
              void run("suggest", async () => {
                const s = await forgeApi.suggest("commit");
                setMessage(s.message || "");
                setNote(sourceText(s));
              })
            }
          >
            {working === "suggest" ? "Drafting…" : "Suggest message"}
          </button>
          <button
            type="button"
            className="mini primary-mini"
            disabled={
              busy ||
              !overview.staged ||
              !message.trim() ||
              Boolean(working) ||
              review.starting
            }
            onClick={() =>
              void run("commit", async () => {
                // "Review before every commit": the commit waits until the
                // findings are on screen, never longer than the user wants.
                if (await review.holdCommit()) return;
                await guard.commit(message, async () => {
                  setMessage("");
                  setNote("");
                  review.committed();
                  toast("Committed", "ok");
                  await load();
                });
              })
            }
          >
            {review.commitLabel}
          </button>
        </div>
      </section>

      {review.enabled && (
        <section
          className="git-section"
          aria-label="Review before commit"
          aria-busy={review.running}
        >
          <h4>Review before commit</h4>
          <div className="opinion-controls">
            <ReviewerSelect
              targets={targets}
              value={review.reviewer}
              onChange={review.choose}
              offline={Boolean(review.options?.offline)}
              writer={review.options?.writer}
            />
            <button
              type="button"
              className="mini"
              disabled={
                !overview.staged ||
                !review.reviewer ||
                review.running ||
                review.starting
              }
              onClick={() => void review.start()}
            >
              {review.starting ? "Starting…" : "Review staged changes"}
            </button>
          </div>
          <label className="check">
            <input
              type="checkbox"
              checked={Boolean(review.options?.prefs.before_commit)}
              onChange={(e) => void review.setBeforeCommit(e.target.checked)}
            />{" "}
            Review before every commit
          </label>
          {review.shown && (
            <div className="opinion-panel">
              <OpinionHead
                opinion={review.shown}
                title={`Review by ${review.shown.reviewer.label}`}
                onCancel={() => void review.opinions.cancel(review.shown!.id)}
              />
              {review.shown.status === "completed" && review.shown.summary && (
                <div className="opinion-summary">
                  <Markdown>{review.shown.summary}</Markdown>
                </div>
              )}
              <OpinionNotes opinion={review.shown} />
              {review.shown.status === "completed" && (
                <ReviewedChanges
                  opinion={review.shown}
                  actions={review.actions}
                />
              )}
            </div>
          )}
          {review.opinions.consent && (
            <ConsentDialog
              request={review.opinions.consent.request}
              destination={targets.find((t) => t.id === review.reviewer)?.name}
              attachments={[]}
              onSend={review.opinions.consent.send}
              onCancel={review.opinions.consent.cancel}
            />
          )}
        </section>
      )}

      <section className="git-section" aria-label="Push">
        <h4>Push</h4>
        {overview.remote ? (
          <div className="git-row">
            <span className="hint grow">
              {overview.upstream
                ? `To ${overview.upstream}`
                : `Publishes ${overview.branch || "this branch"} to ${overview.remote} and tracks it`}
            </span>
            <button
              type="button"
              className="mini"
              disabled={
                !overview.branch ||
                Boolean(working) ||
                (Boolean(overview.upstream) && !overview.ahead)
              }
              onClick={() =>
                void run("push", async () => {
                  const push = async (scanned?: string) => {
                    const done = await forgeApi.push("", scanned);
                    if (!done.ok && refused(done, push)) return;
                    if (!done.ok) throw new Error(done.error || "Not pushed");
                    setPushSecrets(null);
                    toast(`Pushed ${done.branch} to ${done.remote}`, "ok");
                    await load();
                  };
                  await push();
                })
              }
            >
              {working === "push"
                ? "Pushing…"
                : overview.upstream
                  ? "Push"
                  : "Publish branch"}
            </button>
          </div>
        ) : (
          <p className="hint">
            No remote is set up. Add one with git remote add origin &lt;url&gt;
            in the Terminal.
          </p>
        )}
      </section>

      {overview.remote && (
        <section className="git-section" aria-label="Pull request">
          <h4>Pull request</h4>
          {existing ? (
            <div className="pr-created">
              <p>
                <a href={existing.url}>
                  {status?.provider === "gitlab" ? "!" : "#"}
                  {existing.number}
                  {existing.title ? ` ${existing.title}` : ""}
                </a>
                {existing.draft ? <span className="dim"> · draft</span> : null}
                {existing.state && existing.state !== "OPEN" ? (
                  <span className="dim"> · {existing.state.toLowerCase()}</span>
                ) : null}
              </p>
              {status?.provider === "github" ? (
                <div className="pr-checks" aria-label="Checks">
                  <div className="git-row">
                    <span className="grow">
                      {checks.checks
                        ? checks.checks.overall === "none"
                          ? "No checks reported yet"
                          : checks.checks.overall === "pass"
                            ? "All checks passed"
                            : checks.checks.overall === "fail"
                              ? "Some checks failed"
                              : "Checks are running"
                        : checks.loading
                          ? "Reading checks…"
                          : ""}
                    </span>
                    <button
                      type="button"
                      className="icon-btn"
                      aria-label="Refresh checks"
                      title="Refresh checks (also every minute)"
                      disabled={checks.loading}
                      onClick={() => void checks.refresh()}
                    >
                      <RefreshCw size={13} aria-hidden="true" />
                    </button>
                  </div>
                  {checks.error && <p className="hint error">{checks.error}</p>}
                  <ul>
                    {checks.checks?.checks.map((c) => (
                      <li
                        key={`${c.workflow}/${c.name}`}
                        className={`check-${c.bucket}`}
                      >
                        {c.bucket === "pass" ? (
                          <CircleCheck size={13} aria-label="Passed" />
                        ) : c.bucket === "fail" ? (
                          <CircleX size={13} aria-label="Failed" />
                        ) : (
                          <CircleDashed
                            size={13}
                            aria-label={
                              c.bucket === "skipping" ? "Skipped" : "Running"
                            }
                          />
                        )}
                        {c.link ? <a href={c.link}>{c.name}</a> : c.name}
                        {c.workflow ? (
                          <span className="dim"> · {c.workflow}</span>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                </div>
              ) : (
                <p className="hint">
                  Pipeline status is on the merge request page.
                </p>
              )}
              <button
                type="button"
                className="mini"
                onClick={() => {
                  setCreated(null);
                  setStatus((s) => (s ? { ...s, pr: null } : s));
                }}
              >
                Start another
              </button>
            </div>
          ) : (
            <>
              <input
                aria-label="Pull request title"
                placeholder="Title"
                value={pr.title}
                onChange={(e) => setPr({ ...pr, title: e.target.value })}
              />
              <textarea
                aria-label="Pull request description"
                placeholder="Description (Markdown)"
                rows={6}
                value={pr.body}
                onChange={(e) => setPr({ ...pr, body: e.target.value })}
              />
              {prNote && <p className="hint git-note">{prNote}</p>}
              <div className="git-row">
                <label className="git-base">
                  Into
                  <select
                    aria-label="Base branch"
                    value={base}
                    onChange={(e) => {
                      setPr({ ...pr, base: e.target.value });
                      void loadPr(e.target.value);
                    }}
                  >
                    {(overview.bases || [base]).map((b) => (
                      <option key={b} value={b}>
                        {b}
                      </option>
                    ))}
                  </select>
                </label>
                <label className="check">
                  <input
                    type="checkbox"
                    checked={pr.draft}
                    onChange={(e) => setPr({ ...pr, draft: e.target.checked })}
                  />{" "}
                  Draft
                </label>
              </div>
              {onBase && (
                <p className="hint">
                  You are on {base}. Create a branch for this work first.
                </p>
              )}
              <div className="git-row end">
                <button
                  type="button"
                  className="mini"
                  disabled={!overview.branch || onBase || Boolean(working)}
                  onClick={() =>
                    void run("suggest-pr", async () => {
                      const s = await forgeApi.suggest("pr", base);
                      setPr({
                        ...pr,
                        title: s.title || "",
                        body: s.body || "",
                      });
                      setPrNote(
                        s.source === "summary"
                          ? s.note || "Written from the commit list."
                          : sourceText(s, "open the pull request"),
                      );
                    })
                  }
                >
                  {working === "suggest-pr"
                    ? "Drafting…"
                    : "Suggest title and description"}
                </button>
                {canCreate && (
                  <button
                    type="button"
                    className="mini primary-mini"
                    disabled={!pr.title.trim() || Boolean(working)}
                    onClick={() =>
                      void run("create-pr", async () => {
                        const finish = async (made: PrCreated) => {
                          setCreated({
                            number: made.number || 0,
                            url: made.url,
                            draft: made.draft,
                            title: pr.title.trim(),
                            state: "OPEN",
                          });
                          setPr({
                            title: "",
                            body: "",
                            base: pr.base,
                            draft: pr.draft,
                          });
                          setPrNote("");
                          toast(
                            made.pushed
                              ? "Pushed the branch and opened the pull request"
                              : "Pull request opened",
                            "ok",
                          );
                          await load();
                        };
                        const open = async (
                          scanned?: string,
                        ): Promise<void> => {
                          const made = await forgeApi.createPr({
                            title: pr.title.trim(),
                            body: pr.body,
                            base,
                            draft: pr.draft,
                            ...(scanned !== undefined
                              ? { allow_secrets: true, scanned }
                              : {}),
                          });
                          if (!made.ok && refused(made, open)) return;
                          if (!made.ok)
                            throw new Error(made.error || "Not opened");
                          setPushSecrets(null);
                          await finish(made);
                        };
                        await open();
                      })
                    }
                  >
                    {working === "create-pr"
                      ? "Creating…"
                      : "Create pull request"}
                  </button>
                )}
              </div>
              {status && !canCreate && status.provider && cli?.name && (
                <div className="pr-help">
                  {!cli.installed ? (
                    <p className="hint">
                      To open pull requests from here, install the {cliName}{" "}
                      from <a href={cli.install_url}>{cli.install_url}</a>, then
                      sign in with <code>{cli.login_command}</code>.
                    </p>
                  ) : !cli.authenticated ? (
                    <p className="hint">
                      The {cliName} is installed but not signed in. Run{" "}
                      <code>{cli.login_command}</code> in the Terminal, then
                      come back here.{" "}
                      <button
                        type="button"
                        className="link-btn"
                        onClick={onOpenTerminal}
                      >
                        Open the Terminal
                      </button>
                    </p>
                  ) : null}
                </div>
              )}
              {status?.provider === "other" && (
                <p className="hint">
                  Pull requests can be opened here for GitHub and GitLab
                  remotes.
                </p>
              )}
              {status?.compare_url && (
                <button
                  type="button"
                  className="mini"
                  onClick={() =>
                    void openExternal(status.compare_url!).catch((e) =>
                      toast(String(e), "err"),
                    )
                  }
                >
                  <ExternalLink size={12} aria-hidden="true" /> Open compare
                  page in browser
                </button>
              )}
              {status?.provider &&
                FORGE[status.provider] &&
                cli?.detail &&
                cli.authenticated && (
                  <p className="hint dim">
                    {FORGE[status.provider]}: {signedInText(cli.detail)}
                  </p>
                )}
            </>
          )}
        </section>
      )}
      {guard.dialog}
      {pushSecrets && (
        <ConfirmDialog
          title={
            pushSecrets.findings.length
              ? "These commits may contain a secret"
              : "These commits weren’t all checked"
          }
          confirmLabel="Push anyway"
          danger
          onCancel={() => setPushSecrets(null)}
          onConfirm={() =>
            run("push", async () => {
              await pushSecrets.retry();
            })
          }
        >
          {pushSecrets.findings.length ? (
            <>
              <p>
                Nothing was pushed. Once a secret is on the remote, anyone with
                the repository can read it, so remove it from these commits
                first if it is real.
              </p>
              <SecretFindings
                findings={pushSecrets.findings}
                truncated={pushSecrets.truncated}
                unchecked={pushSecrets.unchecked}
              />
            </>
          ) : (
            <p>
              Nothing was pushed. {pushSecrets.unchecked} Push anyway only if
              you know these commits hold no secrets.
            </p>
          )}
          <p className="hint">
            Push anyway sends the commits checked here. Commits made since then
            stay on this computer until you push again.
          </p>
        </ConfirmDialog>
      )}
    </div>
  );
}

/** The Git tab's review of the staged changes: the reviewer (suggested,
 * remembered per project), the latest review of the staged changes as they
 * are now, and "Review before every commit", which holds a commit until
 * the findings are on screen (never forced: the user can commit anyway). */
function useStagedReview({
  workspace,
  sessionId,
  targets,
  repo,
  staged,
  changed,
  toast,
}: {
  workspace: string;
  sessionId: string;
  targets: PickerTarget[];
  repo: boolean;
  staged: number;
  changed: number;
  toast: Toast;
}) {
  const enabled = Boolean(workspace && repo);
  const scope = useMemo(
    () =>
      enabled
        ? {
            workspace,
            source: "staged" as const,
            // The latest review, with the diff its findings point into.
            limit: "3",
            diff: "1" as const,
          }
        : null,
    [enabled, workspace],
  );
  const optionScope = useMemo(
    () => (enabled ? { workspace, session_id: sessionId || undefined } : null),
    [enabled, workspace, sessionId],
  );
  const opinions = useSecondOpinions(scope, toast);
  const { options, setOptions } = useOpinionOptions(optionScope);
  const [picked, setPicked] = useState("");
  const [current, setCurrent] = useState("");
  const [waiting, setWaiting] = useState(false);
  const [starting, setStarting] = useState(false);

  const suggested = options
    ? suggestReviewer(targets, {
        writer: options.writer?.model,
        remembered: options.prefs.model,
        offline: options.offline,
        localOnly: options.local_only,
      })
    : "";
  const reviewer = picked || suggested;

  // The staged changes as they are now, to tell a fresh review from one of
  // earlier changes.
  useEffect(() => {
    if (!enabled || !staged) {
      setCurrent("");
      return;
    }
    let live = true;
    opinionApi
      .current({ workspace, source: "staged" })
      .then((now) => {
        if (live) setCurrent(now.hash);
      })
      .catch(() => {
        if (live) setCurrent("");
      });
    return () => {
      live = false;
    };
  }, [enabled, workspace, staged, changed]);

  const latest = opinions.items[0];
  const running = Boolean(latest && isActiveOpinion(latest));
  const fresh = Boolean(
    latest && current && latest.diff_hash === current && staged,
  );
  const shown = latest && (running || fresh) ? latest : undefined;
  const open = shown && fresh ? openFindings(shown) : [];
  const beforeCommit = Boolean(options?.prefs.before_commit);

  async function start() {
    if (!reviewer || starting) return null;
    setStarting(true);
    try {
      return await opinions.start({
        kind: "review",
        source: "staged",
        workspace,
        session_id: sessionId || undefined,
        model: reviewer,
      });
    } finally {
      setStarting(false);
    }
  }

  /** Before a commit: start the review, or stop one that is running (a
   * review holds the project, so the commit could not run beside it).
   * True when the commit should wait. */
  async function holdCommit() {
    if (!enabled) return false;
    if (running && latest) {
      await opinions.cancel(latest.id);
      setWaiting(false);
      return false;
    }
    if (beforeCommit) {
      // The staged changes as they are at this click, not at the last read.
      const now = await opinionApi
        .current({ workspace, source: "staged" })
        .catch(() => null);
      if (now) setCurrent(now.hash);
      if (latest && now && latest.diff_hash === now.hash) return false;
      if (!reviewer) {
        toast(
          "Choose a reviewer model, or turn off Review before every commit.",
          "info",
        );
        return true;
      }
      const record = await start();
      if (record) setWaiting(true);
      return true;
    }
    return false;
  }

  const gate = !enabled
    ? ""
    : running && waiting
      ? "Reviewing the staged changes before this commit. The commit waits until you have seen the findings; Commit without waiting stops the review."
      : running
        ? "A review of the staged changes is running. Committing now stops it."
        : waiting && fresh && shown
          ? shown.status === "completed"
            ? open.length
              ? `The review found ${open.length} open finding${open.length === 1 ? "" : "s"} below. Fix or dismiss them, or commit anyway.`
              : "The review is done. Commit when you are ready."
            : "The review did not finish. You can commit anyway."
          : "";
  const commitLabel = !enabled
    ? "Commit"
    : running
      ? waiting
        ? "Commit without waiting"
        : "Stop review and commit"
      : fresh && open.length
        ? "Commit anyway"
        : beforeCommit && !fresh
          ? "Review and commit"
          : "Commit";

  const actions: FindingActions = {
    working: opinions.working,
    fix: (opinion, finding) =>
      void opinions.fix(opinion.id, finding.id).then((job) => {
        if (job)
          toast(
            job.session_id === sessionId
              ? "Fix queued in this conversation."
              : "Fix queued in the conversation that made the change.",
            "ok",
          );
      }),
    setFinding: (opinion, finding, status) =>
      void opinions.setFinding(opinion.id, finding.id, status),
  };

  return {
    enabled,
    options,
    opinions,
    reviewer,
    running,
    starting,
    shown,
    gate,
    commitLabel,
    actions,
    start,
    holdCommit,
    committed: () => setWaiting(false),
    choose: (id: string) => {
      setPicked(id);
      if (id)
        void opinionApi
          .savePrefs(workspace, { model: id })
          .then((prefs) => setOptions((o) => (o ? { ...o, prefs } : o)))
          .catch(() => undefined);
    },
    setBeforeCommit: async (on: boolean) => {
      try {
        const prefs = await opinionApi.savePrefs(workspace, {
          before_commit: on,
        });
        setOptions((o) => (o ? { ...o, prefs } : o));
      } catch (error) {
        toast(String(error), "err");
      }
    },
  };
}
