import { CircleHelp } from "lucide-react";
import {
  STEP_LABELS,
  whatWentWrong,
  type NextStep,
} from "../lib/whatWentWrong";

/** Under a failed task's text: what went wrong in plain words, and buttons
 * for the next step. Nothing for failures it doesn't recognise. */
export function FailureHelp({
  text,
  onStep,
  can,
  retryDisabled = false,
}: {
  text: string;
  onStep: (step: NextStep) => void;
  /** Steps this row can take (a row without a request can't retry). */
  can: (step: NextStep) => boolean;
  /** A task is running or being sent: Try again waits, so a second click
   * can't send the request twice. */
  retryDisabled?: boolean;
}) {
  const diagnosis = whatWentWrong(text);
  if (!diagnosis) return null;
  const steps = diagnosis.steps.filter(can);
  return (
    <section className="failure-help" aria-label="What went wrong">
      <p className="failure-help-title">
        <CircleHelp size={14} aria-hidden="true" />
        <strong>{diagnosis.title}</strong>
      </p>
      <p>{diagnosis.plain}</p>
      {steps.length > 0 && (
        <div
          className="failure-help-steps"
          role="group"
          aria-label="Next steps"
        >
          {steps.map((step, index) => (
            <button
              key={step}
              type="button"
              className={index === 0 ? "primary" : "ghost"}
              disabled={step === "retry" && retryDisabled}
              onClick={() => onStep(step)}
            >
              {STEP_LABELS[step]}
            </button>
          ))}
        </div>
      )}
    </section>
  );
}
