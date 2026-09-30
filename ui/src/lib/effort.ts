import { readStore, writeStore } from "./storage";
import type { PickerTarget } from "./picker";

/** Reasoning effort for the next message, remembered per model. "default"
 * sends nothing and the model uses its own setting. Rows that have no
 * effort control (`reasoning` false in GET /api/picker) do not show it. */
export type Effort = "default" | "low" | "medium" | "high";
export const EFFORTS: { id: Effort; label: string; hint: string }[] = [
  { id: "default", label: "Default", hint: "The model's own setting" },
  { id: "low", label: "Low", hint: "Fastest answers, least thinking" },
  { id: "medium", label: "Medium", hint: "Balanced" },
  { id: "high", label: "High", hint: "Thinks longest; slower and uses more" },
];

export const effortKey = (targetId: string) => `shadow:effort:${targetId}`;

export const supportsEffort = (target: PickerTarget | undefined) =>
  target?.reasoning === true;

export function readEffort(targetId: string): Effort {
  const value = readStore(effortKey(targetId));
  return EFFORTS.some((e) => e.id === value) ? (value as Effort) : "default";
}

export function writeEffort(targetId: string, effort: Effort) {
  writeStore(effortKey(targetId), effort === "default" ? null : effort);
}

/** The composer's task mode and the purpose POST /api/jobs takes: Plan and
 * Ask are read-only (the engine's plan and review modes). */
export type TaskMode = "code" | "plan" | "ask";
export const MODES: {
  id: TaskMode;
  label: string;
  purpose: string;
  hint: string;
}[] = [
  {
    id: "code",
    label: "Code",
    purpose: "coder",
    hint: "Change files and run commands (with your approval)",
  },
  {
    id: "plan",
    label: "Plan",
    purpose: "planner",
    hint: "Read the project and write a plan; nothing is changed",
  },
  {
    id: "ask",
    label: "Ask",
    purpose: "reviewer",
    hint: "Answer questions about the project; nothing is changed",
  },
];
export const purposeFor = (mode: TaskMode) =>
  MODES.find((m) => m.id === mode)?.purpose || "coder";
/** The composer mode of an engine job's `mode` (`plan`, `review`, `code`). */
export const modeOfJob = (mode: unknown): TaskMode =>
  mode === "plan" ? "plan" : mode === "review" ? "ask" : "code";
