export type LocalPhase = "preparing" | "waiting" | "loading";

export const localPhase = (value: unknown): LocalPhase | undefined =>
  value === "preparing" || value === "waiting" || value === "loading"
    ? value
    : undefined;

export const localProgressLabel = (phase: LocalPhase): string =>
  ({
    preparing: "Preparing local model",
    waiting: "Waiting for local runtime",
    loading: "Loading local model",
  })[phase];
