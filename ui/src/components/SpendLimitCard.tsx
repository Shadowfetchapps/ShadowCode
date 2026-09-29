import { useState } from "react";
import type { ChatItem } from "./cards";

type SpendItem = Extract<ChatItem, { kind: "spend" }>;

/** A spending limit reached on a paid model (spend.limit_reached). The task
 * waits between steps; nothing runs until the user answers. */
export function SpendLimitCard({
  item,
  onDecide,
}: {
  item: SpendItem;
  onDecide: (item: SpendItem, action: "continue" | "stop") => Promise<void>;
}) {
  const [busy, setBusy] = useState(false);
  const decide = (action: "continue" | "stop") => {
    setBusy(true);
    void onDecide(item, action).finally(() => setBusy(false));
  };
  const open = !item.resolved;
  return (
    <section
      className={`limit-card spend-card${open ? "" : " is-resolved"}`}
      aria-label="Spending limit reached"
    >
      <p>
        <strong>{item.title}</strong>
      </p>
      <p className={open ? "" : "dim"}>{item.text}</p>
      {open ? (
        <div className="row">
          <button
            type="button"
            className="mini primary-mini"
            disabled={busy}
            onClick={() => decide("continue")}
          >
            {item.continueLabel}
          </button>
          <button
            type="button"
            className="mini"
            disabled={busy}
            onClick={() => decide("stop")}
          >
            Stop
          </button>
        </div>
      ) : (
        <p className="spend-outcome">
          {item.resolved === "ended"
            ? "The task ended before an answer."
            : item.outcome}
        </p>
      )}
    </section>
  );
}
