import { useEffect, useState } from "react";
import { api, type GitHook } from "../../api";

/** Settings › Permissions & network: whether commits from the Git tab run
 * the open project's own Git hooks. Shown only when it has some. */
export function GitHooksSetting() {
  const [state, setState] = useState<{
    hooks: GitHook[];
    run: boolean | null;
  } | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    Promise.resolve()
      .then(() => api.gitHooks())
      .then((s) => live && setState(s))
      .catch(() => live && setState(null));
    return () => {
      live = false;
    };
  }, []);
  if (!state || state.hooks.length === 0) return null;
  const value = state.run === null ? "ask" : state.run ? "run" : "skip";
  async function choose(next: string) {
    setError("");
    try {
      setState(await api.setGitHooks(next === "ask" ? null : next === "run"));
    } catch (e) {
      setError(String(e));
    }
  }
  return (
    <fieldset className="mode-options" aria-label="This project's Git hooks">
      <legend>This project’s Git hooks</legend>
      <p className="hint">
        {state.hooks.map((hook) => `${hook.name}: ${hook.preview}`).join(" · ")}
      </p>
      {[
        ["ask", "Ask at the next commit"],
        ["run", "Run them when I commit from ShadowCode"],
        ["skip", "Don’t run them"],
      ].map(([id, label]) => (
        <label className="mode-option" key={id}>
          <input
            type="radio"
            name="git-hooks"
            checked={value === id}
            onChange={() => void choose(id)}
          />
          <span>
            <strong>{label}</strong>
          </span>
        </label>
      ))}
      {error && <p className="warn-text">{error}</p>}
    </fieldset>
  );
}
