import { useEffect, useState } from "react";
import { api, type PickerResponse } from "../api";
import { Dialog } from "./Dialog";
import { ModelSetup } from "./ModelSetup";
import { useModelDownloads } from "../hooks/useModelDownloads";
import { isReady } from "../lib/picker";
import { canPickFiles, pickDirectory } from "../lib/transport";

/** Where the window goes after the first run: straight to work, to
 * Settings › Accounts for an OpenRouter key or a subscription, or to
 * Settings › Local models for the other free models. */
export type OnboardingNext =
  "openrouter" | "subscription" | "local" | undefined;

/** How long the first run waits for the account check before deciding that
 * no model is ready (the local rows and cached accounts answer at once). */
export const READY_CHECK_MS = 6000;

/** Whether any model can run a task right now (a signed-in subscription, an
 * OpenRouter key, or a local model that fits). */
export async function anyModelReady(timeoutMs = READY_CHECK_MS) {
  const ready = (r: PickerResponse) => (r.targets || []).some(isReady);
  const full = api.picker().then(ready);
  const cached = api
    .pickerCached()
    .then(ready)
    .catch(() => false);
  const timeout = new Promise<boolean>((resolve) =>
    setTimeout(() => void cached.then(resolve), timeoutMs),
  );
  // Ready as soon as either answer says so; otherwise the full answer, or
  // the cached one if the full check takes too long or fails.
  return Promise.race([
    cached.then((yes) => (yes ? true : new Promise<boolean>(() => undefined))),
    full.catch(() => cached),
    timeout,
  ]);
}

/** First run: choose the project folder, trust it and pick how the agent may
 * act. If no model is ready yet (no account, no key, no local model), a
 * second step offers a free model for this computer, an OpenRouter key or a
 * subscription. The composer's picker stays the place to switch models. */
export function Onboarding({
  onDone,
}: {
  onDone: (next?: OnboardingNext) => void;
}) {
  const [workspace, setWorkspace] = useState("");
  const [mode, setMode] = useState<"ask" | "allow_edits">("ask");
  const [step, setStep] = useState<"project" | "checking" | "model">("project");
  const [state, setState] = useState<{ busy: boolean; error: string }>({
    busy: false,
    error: "",
  });
  const downloads = useModelDownloads({
    enabled: step === "model",
    onError: (error) => setState({ busy: false, error }),
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
      setStep("checking");
      if (await anyModelReady().catch(() => false)) return onDone();
      setState({ busy: false, error: "" });
      setStep("model");
    } catch (err) {
      setStep("project");
      setState({ busy: false, error: String(err) });
    }
  }

  if (step === "model")
    return (
      <Dialog
        label="Choose a model"
        className="wizard"
        onClose={() => undefined}
      >
        <img src="/icon-192.png" alt="" className="welcome-mark" />
        <h2 id="onboarding-title">Choose how ShadowCode thinks</h2>
        <p className="hint">
          ShadowCode needs an AI model to read and change code. No account is
          needed for the first choice.
        </p>
        <ModelSetup
          downloads={downloads}
          onStarted={() => onDone()}
          onOpenRouter={() => onDone("openrouter")}
          onSubscription={() => onDone("subscription")}
          onBrowse={() => onDone("local")}
          onSkip={() => onDone()}
        />
        {state.error && (
          <p className="health-bad" role="alert">
            {state.error}
          </p>
        )}
      </Dialog>
    );

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
          {step === "checking"
            ? "Checking for models…"
            : state.busy
              ? "Opening…"
              : "Trust and open"}
        </button>
      </div>
    </Dialog>
  );
}
