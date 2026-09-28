/** Wording and arithmetic for the free-model downloads
 * (GET /api/local-models/downloads). Nothing here starts a download. */
import type {
  DownloadCatalog,
  DownloadFit,
  DownloadModel,
  DownloadState,
} from "../api";

/** Sizes as the rest of ShadowCode shows them (binary units: 16 GB of RAM,
 * a 4.8 GB file). */
export function formatSize(bytes: number | null | undefined): string {
  if (!bytes || bytes <= 0) return "0 MB";
  const gib = bytes / 1024 ** 3;
  if (gib >= 1) return `${gib >= 10 ? Math.round(gib) : gib.toFixed(1)} GB`;
  return `${Math.max(1, Math.round(bytes / 1024 ** 2))} MB`;
}

/** "less than a minute", "about 6 minutes", "about 1 hour 10 minutes". */
export function formatDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 60) return "less than a minute";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `about ${minutes} minute${minutes === 1 ? "" : "s"}`;
  const hours = Math.floor(minutes / 60);
  const rest = minutes % 60;
  return `about ${hours} hour${hours === 1 ? "" : "s"}${rest ? ` ${rest} minute${rest === 1 ? "" : "s"}` : ""}`;
}

/** The speed the before-you-start estimate assumes (50 Mbit/s). */
export const TYPICAL_BYTES_PER_SECOND = 50e6 / 8;

export function downloadEstimate(bytes: number): string {
  return `${formatDuration(bytes / TYPICAL_BYTES_PER_SECOND)} on a 50 Mbit/s connection`;
}

export const FIT_LABEL: Record<DownloadFit, string> = {
  gpu: "Runs on your graphics card",
  cpu: "Runs on the processor (slower than a graphics card)",
  tight: "Just fits. Close other apps while you use it",
  no: "Needs more memory than this computer has",
};

export const RUNNING: ReadonlySet<DownloadState> = new Set([
  "downloading",
  "checking",
]);

export const isRunning = (model: DownloadModel) => RUNNING.has(model.state);

/** A download the user started and hasn't finished or cancelled. */
export const inProgress = (model: DownloadModel) =>
  isRunning(model) || model.state === "paused" || model.state === "failed";

export function percent(model: DownloadModel): number {
  if (!model.total) return 0;
  return Math.min(100, Math.floor((model.done / model.total) * 100));
}

/** One line under the progress bar. */
export function progressText(model: DownloadModel): string {
  const amount = `${formatSize(model.done)} of ${formatSize(model.total)}`;
  switch (model.state) {
    case "checking":
      return `Checking the ${formatSize(model.done)} already downloaded…`;
    case "downloading": {
      if (model.bytes_per_second > 0) {
        const left = (model.total - model.done) / model.bytes_per_second;
        return `${amount} · ${formatDuration(left)} left`;
      }
      return `${amount} · starting…`;
    }
    case "paused":
      return `Paused at ${amount}`;
    case "failed":
      return model.done > 0 ? `Stopped at ${amount}` : "Download stopped";
    default:
      return amount;
  }
}

/** Why Download can't be pressed right now, or "" when it can. */
export function blockedReason(
  model: DownloadModel,
  catalog: Pick<DownloadCatalog, "offline" | "busy">,
): string {
  if (!model.supported)
    return (
      model.unsupported_reason || "The bundled llama.cpp can't run this model"
    );
  if (model.fit === "no") return FIT_LABEL.no;
  if (catalog.offline)
    return "Offline mode is on. Switch to Online in Settings › Permissions & network to download.";
  if (catalog.busy && !isRunning(model))
    return "Another model is downloading. Pause it or wait until it finishes.";
  return "";
}

export function recommendedModel(
  catalog: DownloadCatalog | null,
): DownloadModel | undefined {
  if (!catalog) return undefined;
  return catalog.models.find((m) => m.id === catalog.recommended);
}

/** Models worth offering on this computer: loadable and not too big. */
export const offered = (catalog: DownloadCatalog | null) =>
  (catalog?.models || []).filter((m) => m.supported && m.fit !== "no");

/** "NVIDIA GeForce RTX 5060 Ti (16 GB) · 62 GB memory" */
export function hardwareSummary(catalog: DownloadCatalog): string {
  const { gpu, vram_bytes, ram_bytes } = catalog.hardware;
  return [
    gpu ? `${gpu}${vram_bytes ? ` (${formatSize(vram_bytes)})` : ""}` : "",
    ram_bytes ? `${formatSize(ram_bytes)} memory` : "",
  ]
    .filter(Boolean)
    .join(" · ");
}

/** Why nothing is recommended (too little memory), for the first-run card. */
export function noRecommendationReason(catalog: DownloadCatalog): string {
  const smallest = catalog.models
    .filter((m) => m.supported)
    .sort((a, b) => a.min_memory_bytes - b.min_memory_bytes)[0];
  if (!smallest)
    return "The bundled llama.cpp can't run any of the free models.";
  return `This computer has ${formatSize(catalog.hardware.ram_bytes)} of memory; the smallest free model needs about ${formatSize(smallest.min_memory_bytes + 2 * 1024 ** 3)}. Use an OpenRouter key or a subscription instead.`;
}
