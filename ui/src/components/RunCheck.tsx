import { useRef, useState } from "react";
import type { CheckJobRequest } from "../api";
import { Dialog } from "./Dialog";

export type RunCheckAction = {
  workspace: string;
  sessionId: string;
  disabled?: boolean;
  onRun: (request: CheckJobRequest) => Promise<void>;
};

/** A fresh, explicit check. Historical receipt text may be redacted and is
 * never copied into an executable command. Keep the origin frozen while
 * the dialog is open, including across conversation navigation. */
export function RunCheck({ action }: { action: RunCheckAction }) {
  const [origin, setOrigin] = useState<RunCheckAction | null>(null);
  const [command, setCommand] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const submitting = useRef(false);
  const changed = Boolean(
    origin &&
    (origin.workspace !== action.workspace ||
      origin.sessionId !== action.sessionId),
  );
  const close = () => {
    if (!submitting.current) setOrigin(null);
  };
  const run = async () => {
    if (
      !origin ||
      changed ||
      action.disabled ||
      submitting.current ||
      !command.trim()
    )
      return;
    submitting.current = true;
    setBusy(true);
    setError("");
    try {
      await origin.onRun({
        workspace: origin.workspace,
        session_id: origin.sessionId,
        command: command.trim(),
        timeout: 300,
        queue: false,
      });
      setOrigin(null);
    } catch (reason) {
      setError(String(reason));
    } finally {
      submitting.current = false;
      setBusy(false);
    }
  };
  return (
    <>
      <button
        type="button"
        className="mini"
        disabled={action.disabled || !action.workspace || !action.sessionId}
        onClick={() => {
          setOrigin({ ...action });
          setCommand("");
          setError("");
        }}
      >
        Run a check…
      </button>
      {origin && (
        <Dialog label="Run a check" onClose={close}>
          <h2>Run a check</h2>
          <p>
            Run a check you choose against the current files, without another
            model turn. Your normal command approvals still apply. The result
            appears as a new task; earlier evidence stays in history.
          </p>
          <p>
            Project: <code>{origin.workspace}</code>
          </p>
          <form
            onSubmit={(event) => {
              event.preventDefault();
              void run();
            }}
          >
            <label className="field">
              Check command
              <input
                autoFocus
                value={command}
                onChange={(event) => setCommand(event.target.value)}
                placeholder="For example, npm test"
                disabled={busy}
                maxLength={64000}
              />
            </label>
            <p className="hint">
              Runs from the project folder, with a maximum of five minutes (or
              your shorter configured tool timeout). A passing exit code
              confirms this check only.
            </p>
            {changed && (
              <p role="alert">
                The selected conversation changed. Close this dialog and run the
                check from its original conversation.
              </p>
            )}
            {error && (
              <p role="alert" className="health-bad">
                {error}
              </p>
            )}
            <div className="row end">
              <button
                type="button"
                className="ghost"
                disabled={busy}
                onClick={close}
              >
                Cancel
              </button>
              <button
                type="submit"
                className="primary"
                disabled={busy || changed || action.disabled || !command.trim()}
              >
                {busy ? "Starting check…" : "Run check"}
              </button>
            </div>
          </form>
        </Dialog>
      )}
    </>
  );
}
