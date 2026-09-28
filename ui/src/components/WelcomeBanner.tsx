import type { ReactNode } from "react";

export const SUGGESTIONS: { label: string; prompt: string }[] = [
  {
    label: "Explain this project",
    prompt:
      "Explore this project and explain its structure, entry points, and how to run it.",
  },
  { label: "Fix a bug", prompt: "Find and fix the bug where " },
  { label: "Add tests", prompt: "Add tests for " },
];

/** Quiet empty state: the mark, one line and three optional suggestions.
 * Without a model it also shows how to get one (`modelSetup`), or a
 * "Choose a model" button. */
export function WelcomeBanner({
  onSelect,
  needsModel = false,
  onChooseModel,
  modelSetup,
}: {
  onSelect: (prompt: string) => void;
  needsModel?: boolean;
  onChooseModel?: () => void;
  modelSetup?: ReactNode;
}) {
  return (
    <div className="welcome">
      <img src="/icon-192.png" alt="" className="welcome-mark" />
      <h1>What should we work on?</h1>
      {needsModel && modelSetup ? (
        <div className="welcome-model-setup is-wide">{modelSetup}</div>
      ) : (
        needsModel &&
        onChooseModel && (
          <div className="welcome-model-setup">
            <p>Choose a coding agent or local model to start.</p>
            <button type="button" className="primary" onClick={onChooseModel}>
              Choose a model
            </button>
          </div>
        )
      )}
      <div className="welcome-suggestions" aria-label="Suggestions">
        {SUGGESTIONS.map((s) => (
          <button
            type="button"
            className="welcome-chip"
            key={s.label}
            onClick={() => onSelect(s.prompt)}
          >
            {s.label}
          </button>
        ))}
      </div>
    </div>
  );
}
