/** Compare (docs/COMPARE.md): rules the UI checks before the engine does, and
 * how a lane's state reads. The engine stays authoritative; these only explain
 * why a control is unavailable. */
import type { Approval, CompareLane, CompareRecord } from "../api";
import {
  availabilityLabel,
  isApiKey,
  isLocal,
  isReady,
  type PickerTarget,
} from "./picker";

export const MIN_LANES = 2;
export const MAX_LANES = 3;

export const DUPLICATE_REASON = "Already chosen for another model slot.";

const LOCAL_ID = /^local:gguf:/;
/** Picker ids of local GGUF models (the engine's own test). */
export const isLocalId = (id: string) => LOCAL_ID.test(id);

export type Badge = { label: "Local" | "Cloud" | "API key"; kind: string };

/** The Local / Cloud / API key badge for a lane or slot. */
export function badgeFor(id: string, target?: PickerTarget): Badge {
  if (target ? isLocal(target) : isLocalId(id))
    return { label: "Local", kind: "local" };
  if (target ? isApiKey(target) : id.startsWith("api:"))
    return { label: "API key", kind: "cloud api" };
  return { label: "Cloud", kind: "cloud" };
}

/** Why the Compare button is unavailable, or null when it can open. */
export function compareBlocked(options: {
  task: string;
  repo: boolean;
  inLane: boolean;
  images: number;
}): string | null {
  if (options.inLane)
    return "This conversation is one model's copy in a comparison. Go back to the project to start another comparison.";
  if (!options.repo)
    return "Compare needs a Git repository: each model works in its own copy of the project. Open the repository's root folder.";
  const text = options.task.trim();
  if (!text) return "Type a task to compare models on it.";
  if (text.startsWith("/")) return "Commands cannot be compared. Type a task.";
  if (options.images)
    return "Compare sends text only. Remove the attached images first.";
  return null;
}

/** Rows for one slot's picker: rows that cannot join this lineup stay
 * visible with the reason (for example, a row already chosen). */
export function slotTargets(
  targets: PickerTarget[],
  chosen: string[],
  slot: number,
): PickerTarget[] {
  const others = chosen.filter((id, index) => index !== slot && id);
  return targets.map((target) => {
    if (others.includes(target.id))
      return {
        ...target,
        availability: "unavailable",
        availability_label: "Already chosen",
        reason: DUPLICATE_REASON,
      };
    return target;
  });
}

export type LineupCheck = {
  /** First problem, or null when the lineup can start. */
  error: string | null;
  /** Per-slot problem (index-aligned with the models). */
  slots: (string | null)[];
};

/** The lineup rules the engine enforces (2–3 distinct ready models; local models run sequentially). */
export function checkLineup(
  models: string[],
  targets: PickerTarget[],
): LineupCheck {
  const slots: (string | null)[] = models.map(() => null);
  models.forEach((id, index) => {
    if (!id) {
      slots[index] = "Choose a model.";
      return;
    }
    if (models.indexOf(id) !== index) {
      slots[index] = "Choose a different model for each slot.";
      return;
    }
    const target = targets.find((t) => t.id === id);
    if (!target) {
      slots[index] = "This model is no longer available. Choose another.";
      return;
    }
    if (!isReady(target)) {
      slots[index] =
        `${target.name}: ${availabilityLabel(target)}${target.reason ? ` · ${target.reason}` : ""}`;
      return;
    }
  });
  const chosen = models.filter(Boolean).length;
  let error: string | null = slots.find(Boolean) || null;
  if (models.length < MIN_LANES || models.length > MAX_LANES)
    error = "Compare needs 2 or 3 models.";
  else if (chosen < MIN_LANES && !error) error = "Choose at least 2 models.";
  return { error, slots };
}

export const ACTIVE = ["", "queued", "running", "paused", "cancelling"];
export const isActive = (status: string) => ACTIVE.includes(status);

export type LaneState = {
  /** waiting = an approval is pending for the lane's conversation. */
  kind:
    | "queued"
    | "running"
    | "waiting"
    | "stopping"
    | "completed"
    | "failed"
    | "cancelled"
    | "limit"
    | "interrupted"
    | "removed";
  label: string;
  tone: "busy" | "ok" | "warn" | "bad" | "muted";
};

export function laneState(
  lane: CompareLane,
  approvals: Approval[] = [],
): LaneState {
  if (
    isActive(lane.status) &&
    approvals.some((a) => a.session_id === lane.session_id)
  )
    return { kind: "waiting", label: "Waiting for approval", tone: "warn" };
  switch (lane.status) {
    case "":
    case "queued":
      return { kind: "queued", label: "Starting…", tone: "busy" };
    case "running":
      return { kind: "running", label: "Working…", tone: "busy" };
    case "paused":
      return { kind: "running", label: "Paused", tone: "busy" };
    case "cancelling":
      return { kind: "stopping", label: "Stopping…", tone: "busy" };
    case "completed":
      return { kind: "completed", label: "Finished", tone: "ok" };
    case "cancelled":
      return { kind: "cancelled", label: "Stopped", tone: "muted" };
    case "limit_reached":
      return { kind: "limit", label: "Plan limit reached", tone: "warn" };
    case "interrupted":
      return { kind: "interrupted", label: "Interrupted", tone: "warn" };
    default:
      return { kind: "failed", label: "Failed", tone: "bad" };
  }
}

/** Keep is offered once a lane has finished while the comparison is open;
 * a lane that stopped early only when it changed something. */
export function keepBlocked(
  record: CompareRecord,
  lane: CompareLane,
): string | null {
  if (!["running", "done"].includes(record.state))
    return record.state === "applied"
      ? "A result was already kept."
      : "This comparison was discarded.";
  if (lane.removed) return "This copy was removed.";
  if (isActive(lane.status)) return "Wait for this model to finish.";
  if (lane.status !== "completed" && !lane.changed_files.length)
    return "This model stopped without changes; there is nothing to keep.";
  return null;
}

/** Files named by a Keep that no longer applies ("…started in a, b. Nothing
 * was changed…"). The engine only reports them in its message. */
export function conflictFiles(message: string): string[] | null {
  if (!/no longer apply/.test(message)) return null;
  const match = message.match(
    /comparison started in (.+?)\. Nothing was changed/s,
  );
  if (!match) return [];
  return match[1]
    .split(", ")
    .map((path) => path.trim())
    .filter(Boolean);
}

export function formatSeconds(seconds: number): string {
  const total = Math.max(0, Math.round(seconds));
  if (total < 60) return `${total}s`;
  const minutes = Math.floor(total / 60);
  if (minutes < 60) return `${minutes}m ${total % 60}s`;
  return `${Math.floor(minutes / 60)}h ${minutes % 60}m`;
}

export function formatTokenCount(n: number): string {
  return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : String(n);
}

/** "Error: message" from the transport reads as the message only. */
export const errorText = (error: unknown) =>
  error instanceof Error ? error.message : String(error);
