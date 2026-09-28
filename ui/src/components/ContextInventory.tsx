import { useEffect, useState } from "react";
import { RefreshCw, X } from "lucide-react";
import { api, type ContextPreview } from "../api";
import type { Mention } from "../lib/mentions";

/** A bounded preview of the app-managed @-attachment layer. Provider-owned
 * workspace discovery is intentionally described as opaque. */
export function ContextInventory({
  mentions,
  onClose,
}: {
  mentions: readonly Mention[];
  onClose: () => void;
}) {
  const requestKey = JSON.stringify(mentions);
  const [preview, setPreview] = useState<ContextPreview | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [updatedAt, setUpdatedAt] = useState<number | null>(null);

  useEffect(() => {
    let current = true;
    setLoading(true);
    setError("");
    void api
      .contextPreview(JSON.parse(requestKey) as Mention[])
      .then((result) => {
        if (!current) return;
        setPreview(result);
        setUpdatedAt(Date.now());
      })
      .catch((reason) => {
        if (current) setError(String(reason));
      })
      .finally(() => {
        if (current) setLoading(false);
      });
    return () => {
      current = false;
    };
  }, [requestKey]);

  const refresh = () => {
    setLoading(true);
    setError("");
    void api
      .contextPreview(JSON.parse(requestKey) as Mention[])
      .then((result) => {
        setPreview(result);
        setUpdatedAt(Date.now());
      })
      .catch((reason) => setError(String(reason)))
      .finally(() => setLoading(false));
  };

  return (
    <aside
      className="context-inventory"
      role="dialog"
      aria-label="Attached context inventory"
    >
      <header>
        <div>
          <strong>Attached context</strong>
          <span>
            {loading
              ? "Checking current files…"
              : updatedAt
                ? `Previewed ${new Date(updatedAt).toLocaleTimeString()}`
                : "Preview unavailable"}
          </span>
        </div>
        <button
          type="button"
          className="icon-btn"
          aria-label="Refresh context preview"
          title="Refresh current file state"
          disabled={loading}
          onClick={refresh}
        >
          <RefreshCw size={14} aria-hidden="true" />
        </button>
        <button
          type="button"
          className="icon-btn"
          aria-label="Close attached context"
          onClick={onClose}
        >
          <X size={14} aria-hidden="true" />
        </button>
      </header>
      {error && <p role="alert">Could not preview attachments: {error}</p>}
      {!loading && preview?.items.length === 0 && (
        <p>No explicit @-attached files or folders.</p>
      )}
      {preview && preview.items.length > 0 && (
        <ul aria-label="Included and excluded sources">
          {preview.items.map((item) => (
            <li key={`${item.kind}:${item.path}`}>
              <div>
                <strong>
                  {item.included ? "Included" : "Not included"} · {item.path}
                </strong>
                <span>{item.reason}</span>
                {item.kind === "file" && item.included && (
                  <span>
                    {item.from_line && item.to_line
                      ? `Lines ${item.from_line}–${item.to_line} · `
                      : ""}
                    {item.bytes.toLocaleString()} of{" "}
                    {item.total_bytes?.toLocaleString() ?? "unknown"} bytes
                    {item.truncated ? " · truncated" : ""}
                  </span>
                )}
                {item.kind === "dir" && item.included && (
                  <details>
                    <summary>
                      {item.entries.length} names attached
                      {item.truncated ? " · list truncated" : ""}
                    </summary>
                    <ul>
                      {item.entries.map((entry) => (
                        <li key={entry}>{entry}</li>
                      ))}
                    </ul>
                  </details>
                )}
              </div>
            </li>
          ))}
        </ul>
      )}
      {preview?.truncated && (
        <p role="status">Some attached context is truncated by size limits.</p>
      )}
      {preview && preview.items.length > 0 && (
        <p className="context-inventory-note" role="status">
          About {preview.estimated_tokens.toLocaleString()} tokens estimated ·{" "}
          {preview.included_bytes.toLocaleString()} attached bytes
        </p>
      )}
      <p className="context-inventory-note">
        This preview covers only explicit @-attachments. Native models may also
        receive a local repository map. Vendor CLIs can discover workspace
        context themselves; ShadowCode cannot enumerate that provider-managed
        context. Files are read again when the task starts.
      </p>
    </aside>
  );
}
