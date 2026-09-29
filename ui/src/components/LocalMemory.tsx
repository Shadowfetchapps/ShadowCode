import { useState } from "react";
import type { ChatItem } from "./cards";
import { readableError } from "../lib/transport";

type MemoryItem = Extract<ChatItem, { kind: "memory" }>;

const tokens = (value: number) => value.toLocaleString("en-US");

/** A local model ran out of memory while loading. The conversation already
 * says why; this card offers the ways on: a smaller context (saved as the
 * local context size, then the message is sent again), another model, or the
 * Local models page. */
export function LocalMemoryItem({
  item,
  disabled,
  onUseContext,
  onChoose,
  onOpenLocal,
}: {
  item: MemoryItem;
  disabled?: boolean;
  /** Save the smaller context size and send the request again. */
  onUseContext: (item: MemoryItem, contextTokens: number) => Promise<void>;
  onChoose: () => void;
  onOpenLocal: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const smaller = item.smallerContext;
  if (item.resolved)
    return (
      <div className="msg-note limit-note">
        {item.model} did not fit in memory for this message.
      </div>
    );
  return (
    <section className="limit-card" aria-label="Not enough memory">
      <p className="dim">
        {smaller
          ? `Load ${item.model} with a ${tokens(smaller)}-token context and send the message again. This lowers the context size for local models; you can change it again later.`
          : "Choose a smaller model, or free memory and send the message again."}
      </p>
      {error ? (
        <p className="warning" role="alert">
          {error}
        </p>
      ) : null}
      <div className="row">
        {smaller && item.request ? (
          <button
            type="button"
            className="mini primary-mini"
            disabled={disabled || busy}
            onClick={async () => {
              setBusy(true);
              setError("");
              try {
                await onUseContext(item, smaller);
              } catch (e) {
                setError(readableError(e));
              } finally {
                setBusy(false);
              }
            }}
          >
            Use a {tokens(smaller)}-token context and retry
          </button>
        ) : null}
        <button type="button" className="mini" onClick={onChoose}>
          Choose another model
        </button>
        <button type="button" className="mini" onClick={onOpenLocal}>
          Open Local models
        </button>
      </div>
    </section>
  );
}
