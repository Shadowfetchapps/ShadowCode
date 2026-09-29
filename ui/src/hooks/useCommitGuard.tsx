import { useState } from "react";
import { api, type GitHook, type SecretFinding } from "../api";
import { Dialog } from "../components/Dialog";
import { SecretFindings } from "../components/SecretFindings";

type Options = { allow_secrets?: boolean; hooks?: "run" | "skip" };
type Pending = {
  message: string;
  options: Options;
  done: () => void | Promise<void>;
};
type Guard =
  | (Pending & { kind: "hooks"; hooks: GitHook[] })
  | (Pending & {
      kind: "secrets";
      findings: SecretFinding[];
      truncated: boolean;
    });

/** Commits from the Git tab: the engine may answer that the staged changes
 * look like they contain a secret, or ask once whether to run the
 * project's own Git hooks. This shows those questions and commits when
 * they are answered. */
export function useCommitGuard(
  toast: (text: string, kind?: "ok" | "err" | "info") => void,
) {
  const [guard, setGuard] = useState<Guard | null>(null);
  const [working, setWorking] = useState(false);

  async function attempt(
    message: string,
    options: Options,
    done: Pending["done"],
  ): Promise<boolean> {
    const result = await api.gitCommit(message, options);
    if (result.ok) {
      setGuard(null);
      await done();
      return true;
    }
    if (result.needs_hooks_choice) {
      setGuard({
        kind: "hooks",
        hooks: result.hooks || [],
        message,
        options,
        done,
      });
      return false;
    }
    if (result.secrets?.length) {
      setGuard({
        kind: "secrets",
        findings: result.secrets,
        truncated: Boolean(result.secrets_truncated),
        message,
        options,
        done,
      });
      return false;
    }
    throw new Error(result.error || "Not committed");
  }

  async function act(work: () => Promise<unknown>) {
    setWorking(true);
    try {
      await work();
    } catch (e) {
      toast(String(e), "err");
    } finally {
      setWorking(false);
    }
  }

  const drop = (path: string) =>
    setGuard((g) =>
      g && g.kind === "secrets"
        ? { ...g, findings: g.findings.filter((f) => f.path !== path) }
        : g,
    );

  const dialog = guard ? (
    guard.kind === "hooks" ? (
      <Dialog
        label="Run this project's Git hooks?"
        onClose={() => setGuard(null)}
      >
        <div className="confirm-dialog">
          <h2>Run this project’s Git hooks?</h2>
          <p>
            This project has its own Git hooks. ShadowCode doesn’t run them
            unless you say so, because they are programs from the repository.
          </p>
          <ul className="hook-list" aria-label="Git hooks">
            {guard.hooks.map((hook) => (
              <li key={hook.name}>
                <strong>{hook.name}</strong> runs <code>{hook.preview}</code>
              </li>
            ))}
          </ul>
          <p className="hint">
            Your answer is kept for this project. Change it in Settings ›
            Permissions & network.
          </p>
          <div className="row confirm-actions">
            <button
              type="button"
              className="ghost"
              onClick={() => setGuard(null)}
            >
              Cancel
            </button>
            <button
              type="button"
              className="ghost"
              disabled={working}
              onClick={() =>
                void act(() =>
                  attempt(
                    guard.message,
                    { ...guard.options, hooks: "skip" },
                    guard.done,
                  ),
                )
              }
            >
              Commit without them
            </button>
            <button
              type="button"
              className="primary"
              disabled={working}
              onClick={() =>
                void act(() =>
                  attempt(
                    guard.message,
                    { ...guard.options, hooks: "run" },
                    guard.done,
                  ),
                )
              }
            >
              Run hooks and commit
            </button>
          </div>
        </div>
      </Dialog>
    ) : (
      <Dialog
        label="This commit may contain a secret"
        onClose={() => setGuard(null)}
      >
        <div className="confirm-dialog">
          <h2>
            {guard.findings.length
              ? "This commit may contain a secret"
              : "Ready to commit"}
          </h2>
          {guard.findings.length > 0 ? (
            <>
              <p>
                Nothing was committed. Once a secret is pushed, anyone with the
                repository can read it.
              </p>
              <SecretFindings
                findings={guard.findings}
                truncated={guard.truncated}
                actions={(path) => (
                  <>
                    <button
                      type="button"
                      className="mini"
                      disabled={working}
                      onClick={() =>
                        void act(async () => {
                          await api.gitUnstage([path]);
                          drop(path);
                        })
                      }
                    >
                      Remove from commit
                    </button>
                    <button
                      type="button"
                      className="mini"
                      disabled={working}
                      onClick={() =>
                        void act(async () => {
                          await api.gitIgnore(path);
                          drop(path);
                        })
                      }
                    >
                      Add to .gitignore
                    </button>
                  </>
                )}
              />
            </>
          ) : (
            <p>The files with possible secrets are out of this commit.</p>
          )}
          <div className="row confirm-actions">
            <button
              type="button"
              className="ghost"
              onClick={() => setGuard(null)}
            >
              Cancel
            </button>
            {guard.findings.length > 0 ? (
              <button
                type="button"
                className="primary danger"
                disabled={working}
                onClick={() =>
                  void act(() =>
                    attempt(
                      guard.message,
                      { ...guard.options, allow_secrets: true },
                      guard.done,
                    ),
                  )
                }
              >
                Commit anyway
              </button>
            ) : (
              <button
                type="button"
                className="primary"
                disabled={working}
                onClick={() =>
                  void act(() =>
                    attempt(guard.message, guard.options, guard.done),
                  )
                }
              >
                Commit
              </button>
            )}
          </div>
        </div>
      </Dialog>
    )
  ) : null;

  return {
    /** Commit `message`; `done` runs once it is committed. Resolves false
     * when a question is shown instead. */
    commit: (message: string, done: Pending["done"]) =>
      attempt(message, {}, done),
    dialog,
  };
}
