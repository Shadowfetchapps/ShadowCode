import { useEffect, useState } from "react";
import { api } from "../api";
import { Dialog } from "./Dialog";
import { canPickFiles, pickDirectory } from "../lib/transport";

/** First run: choose the project folder, trust it and pick how the agent may
 * act. Models are chosen later in the composer's picker, the only place a
 * model is selected. */
export function Onboarding({ onDone }: { onDone: () => void }) {
  const [workspace, setWorkspace] = useState("");
  const [mode, setMode] = useState<"ask" | "allow_edits">("ask");
  const [state, setState] = useState<{ busy: boolean; error: string }>({
    busy: false,
    error: "",
  });

  useEffect(() => {
    api
      .onboarding()
      .then((data) => setWorkspace((w) => w || data.suggested_workspace || ""))
      .catch(() => undefined);
  }, []);

  async function start() {
    setState({ busy: true, error: "" });
    try {
      // No provider or model: the backend keeps its model configuration and
      // the picker becomes the first selection point.
      await api.completeOnboarding({
        workspace: workspace.trim(),
        permission_level: "workspace",
        permission_mode: mode,
        theme: "system",
      });
      await api.saveConfig({ permissions: { mode } }).catch(() => undefined);
      onDone();
    } catch (err) {
      setState({ busy: false, error: String(err) });
    }
  }

  return (
    <Dialog
      label="Welcome to ShadowCode"
      className="wizard"
      onClose={() => undefined}
    >
      <img src="/icon-192.png" alt="" className="welcome-mark" />
      <h2 id="onboarding-title">Welcome to ShadowCode</h2>
      <p className="hint">
        Open a project to start. You choose a model for each conversation in the
        composer.
      </p>
      <div className="field">
        <label htmlFor="onboarding-folder">Project folder</label>
        <div className="input-action">
          <input
            id="onboarding-folder"
            value={workspace}
            onChange={(e) => setWorkspace(e.target.value)}
            placeholder="/path/to/project"
          />
          {canPickFiles() && (
            <button
              type="button"
              className="ghost"
              disabled={state.busy}
              onClick={() =>
                void pickDirectory()
                  .then((path) => {
                    if (path) setWorkspace(path);
                  })
                  .catch((error) =>
                    setState({ busy: false, error: String(error) }),
                  )
              }
            >
              Browse…
            </button>
          )}
        </div>
      </div>
      <fieldset className="mode-options">
        <legend>When the agent wants to change something</legend>
        <label className="mode-option">
          <input
            type="radio"
            name="onboarding-mode"
            checked={mode === "ask"}
            onChange={() => setMode("ask")}
          />
          <span>
            <strong>Ask before actions</strong>
            <small>File edits and commands wait for your approval.</small>
          </span>
        </label>
        <label className="mode-option">
          <input
            type="radio"
            name="onboarding-mode"
            checked={mode === "allow_edits"}
            onChange={() => setMode("allow_edits")}
          />
          <span>
            <strong>Allow project edits</strong>
            <small>
              Edits inside this folder run without asking; commands still ask.
            </small>
          </span>
        </label>
      </fieldset>
      <p className="hint">
        Starting trusts this folder: the agent may read it and, with the choice
        above, change files inside it.
      </p>
      {state.error && (
        <p className="health-bad" role="alert">
          {state.error}
        </p>
      )}
      <div className="row end">
        <button
          type="button"
          className="primary"
          disabled={state.busy || !workspace.trim()}
          onClick={() => void start()}
        >
          {state.busy ? "Opening…" : "Trust and open"}
        </button>
      </div>
    </Dialog>
  );
}
