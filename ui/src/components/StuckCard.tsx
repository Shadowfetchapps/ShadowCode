import { useId, useState } from "react";

export type StuckAction = "resume" | "hint" | "other" | "stop";

/** The agent seems stuck (the same failure again and again, or a file
 * changed back and forth): the task is paused and the user decides. A task
 * that was not paused (a subagent, or one nobody could answer for), or that
 * went on or ended since, shows what happened instead of the choices. */
export function StuckCard({
  text,
  paused = true,
  resolved,
  onAction,
}: {
  text: string;
  paused?: boolean;
  resolved?: "continued" | "ended";
  onAction: (action: StuckAction, hint?: string) => Promise<void>;
}) {
  const uid = useId();
  const [hinting, setHinting] = useState(false);
  const [hint, setHint] = useState("");
  const [done, setDone] = useState<StuckAction | null>(null);
  const [working, setWorking] = useState(false);
  const [error, setError] = useState("");
  async function act(action: StuckAction, value?: string) {
    setWorking(true);
    setError("");
    try {
      await onAction(action, value);
      setDone(action);
    } catch (e) {
      setError(String(e));
    } finally {
      setWorking(false);
    }
  }
  const outcome: Record<StuckAction, string> = {
    resume: "Continued.",
    hint: "Your hint was sent and the task continued.",
    other: "Stopped; choose the model to continue with.",
    stop: "Stopped.",
  };
  return (
    <section
      className="limit-card stuck-card"
      aria-label="The agent seems stuck"
    >
      <p>
        <strong>{text}</strong>
      </p>
      {done ? (
        <p className="hint">{outcome[done]}</p>
      ) : !paused ? (
        <p className="hint">
          The task went on; the agent was asked to try another way.
        </p>
      ) : resolved ? (
        <p className="hint">
          {resolved === "ended" ? "The task has ended." : "The task went on."}
        </p>
      ) : hinting ? (
        <form
          className="approval-note"
          onSubmit={(e) => {
            e.preventDefault();
            if (hint.trim()) void act("hint", hint.trim());
          }}
        >
          <label htmlFor={`${uid}-hint`}>What should it try instead?</label>
          <textarea
            id={`${uid}-hint`}
            rows={2}
            value={hint}
            autoFocus
            onChange={(e) => setHint(e.target.value)}
          />
          <div className="row approval-actions">
            <button
              type="button"
              className="ghost"
              onClick={() => setHinting(false)}
            >
              Back
            </button>
            <button
              type="submit"
              className="primary"
              disabled={working || !hint.trim()}
            >
              Send hint and continue
            </button>
          </div>
        </form>
      ) : (
        <>
          <p className="hint">The task is paused. What next?</p>
          <div className="row approval-actions">
            <button
              type="button"
              className="mini"
              disabled={working}
              onClick={() => void act("stop")}
            >
              Stop
            </button>
            <button
              type="button"
              className="mini"
              disabled={working}
              onClick={() => void act("other")}
            >
              Try another model
            </button>
            <button
              type="button"
              className="mini"
              disabled={working}
              onClick={() => setHinting(true)}
            >
              Give a hint
            </button>
            <button
              type="button"
              className="mini primary-mini"
              disabled={working}
              onClick={() => void act("resume")}
            >
              Keep going
            </button>
          </div>
        </>
      )}
      {error && <p className="warn-text">{error}</p>}
    </section>
  );
}
