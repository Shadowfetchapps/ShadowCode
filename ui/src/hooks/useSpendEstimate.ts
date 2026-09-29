import { useEffect, useState } from "react";
import { api } from "../api";
import type { PickerTarget } from "../lib/picker";

export type ShownEstimate = { label: string; detail: string };

/** A paid API row: billed per token on a cloud endpoint (not a
 * subscription, not this computer). The engine decides for sure. */
export function mayCost(target: PickerTarget | undefined): boolean {
  return Boolean(
    target &&
    target.inference !== "local" &&
    target.route !== "vendor_cli" &&
    !target.id.startsWith("cli:") &&
    (target.group === "api" || target.id.startsWith("api:")),
  );
}

/** The composer's "about $0.01–$0.05" for the next message on a paid model,
 * from the conversation so far and the model's cached prices. Hidden for
 * subscriptions, local models and models without prices. */
export function useSpendEstimate(
  sessionId: string,
  target: PickerTarget | undefined,
  draftChars: number,
  /** Changes when the conversation grows (a task finished). */
  version: string,
): ShownEstimate | null {
  const [estimate, setEstimate] = useState<ShownEstimate | null>(null);
  const paid = mayCost(target);
  const targetId = target?.id || "";
  // Recompute when the draft grows by a few lines, not on every key.
  const bucket = Math.round(draftChars / 400);
  useEffect(() => {
    if (!paid || !targetId) {
      setEstimate(null);
      return;
    }
    let live = true;
    const timer = setTimeout(() => {
      api
        .spendEstimate(sessionId, targetId, bucket * 400)
        .then((result) => {
          if (!live) return;
          setEstimate(
            result.show ? { label: result.label, detail: result.detail } : null,
          );
        })
        .catch(() => {
          if (live) setEstimate(null);
        });
    }, 350);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [paid, targetId, sessionId, bucket, version]);
  return estimate;
}
