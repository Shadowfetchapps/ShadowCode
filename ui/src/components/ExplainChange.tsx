import { useEffect, useRef, useState } from "react";
import { api } from "../api";
import { Markdown } from "./Markdown";

/** "Explain this change": the selected file's change in plain words, from a
 * model, only when asked. */
export function ExplainChange({
  taskId,
  path,
}: {
  taskId: string;
  path: string;
}) {
  const [state, setState] = useState<
    | { kind: "idle" }
    | { kind: "working" }
    | { kind: "done"; text: string; model: string }
    | { kind: "error"; text: string }
  >({ kind: "idle" });
  // The file shown now: an answer for another one is dropped.
  const shown = useRef("");
  const key = `${taskId}\n${path}`;
  // A new file starts fresh.
  useEffect(() => {
    shown.current = key;
    setState({ kind: "idle" });
  }, [key]);
  async function explain() {
    const asked = key;
    setState({ kind: "working" });
    try {
      const answer = await api.explainChange(taskId, path);
      if (shown.current !== asked) return;
      setState(
        answer.ok && answer.text
          ? { kind: "done", text: answer.text, model: answer.model || "" }
          : {
              kind: "error",
              text: answer.error || "No explanation came back.",
            },
      );
    } catch (e) {
      if (shown.current !== asked) return;
      setState({ kind: "error", text: String(e) });
    }
  }
  if (state.kind === "idle" || state.kind === "working")
    return (
      <button
        type="button"
        className="mini"
        disabled={state.kind === "working"}
        title="Ask the conversation's model to describe this change in plain words"
        onClick={() => void explain()}
      >
        {state.kind === "working" ? "Explaining…" : "Explain this change"}
      </button>
    );
  return (
    <section
      className="explain-change"
      aria-label="Explanation"
      aria-live="polite"
    >
      {state.kind === "done" ? (
        <>
          <Markdown>{state.text}</Markdown>
          {state.model && <p className="hint">Explained by {state.model}.</p>}
        </>
      ) : (
        <p className="warn-text">{state.text}</p>
      )}
      <button
        type="button"
        className="link"
        onClick={() => setState({ kind: "idle" })}
      >
        Close
      </button>
    </section>
  );
}
