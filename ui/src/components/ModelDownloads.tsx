import { useState } from "react";
import type { DownloadCatalog, DownloadModel } from "../api";
import type { ModelDownloads } from "../hooks/useModelDownloads";
import {
  FIT_LABEL,
  blockedReason,
  downloadEstimate,
  formatSize,
  inProgress,
  isRunning,
  percent,
  progressText,
} from "../lib/downloads";

/** One free model: what it is, whether it suits this computer, and its
 * download (Download, Pause, Resume, Cancel, Delete). */
export function DownloadRow({
  model,
  catalog,
  downloads,
  onUse,
}: {
  model: DownloadModel;
  catalog: DownloadCatalog;
  downloads: ModelDownloads;
  /** Offered once the model is downloaded (select it in the composer). */
  onUse?: (model: DownloadModel) => void;
}) {
  const [confirming, setConfirming] = useState(false);
  const busy = Boolean(downloads.pending);
  const pending = (action: string) =>
    downloads.pending === `${action}:${model.id}`;
  const blocked = blockedReason(model, catalog);
  const running = isRunning(model);
  const resumable =
    model.state === "paused" || (model.state === "failed" && model.done > 0);
  return (
    <article className="local-model download-model" aria-label={model.name}>
      <header>
        <strong>{model.name}</strong>
        <span className="dim">{model.publisher}</span>
        {model.recommended && (
          <span className="cap-badge download-recommended">
            Recommended for this computer
          </span>
        )}
        {model.state === "installed" && (
          <span className="avail avail-ready">Downloaded</span>
        )}
      </header>
      <p>{model.summary}</p>
      <p className="hint">
        {formatSize(model.bytes)} download · needs about{" "}
        {formatSize(model.memory_bytes)} of memory · {FIT_LABEL[model.fit]} ·{" "}
        <a href={model.license_url} target="_blank" rel="noreferrer">
          {model.license}
        </a>
      </p>
      {inProgress(model) && (
        <div className="download-progress">
          <progress
            max={100}
            value={percent(model)}
            aria-label={`${model.name} download progress`}
          />
          <span role="status">{progressText(model)}</span>
        </div>
      )}
      {model.error && (
        <p className="health-bad" role="alert">
          {model.error}
        </p>
      )}
      {model.state === "available" &&
        blocked &&
        model.supported &&
        model.fit !== "no" && <p className="hint">{blocked}</p>}
      {!model.supported && (
        <p className="health-bad">{model.unsupported_reason}</p>
      )}
      <div className="row">
        {running ? (
          <>
            <button
              type="button"
              className="ghost"
              disabled={busy}
              onClick={() => void downloads.pause(model.id)}
            >
              {pending("pause") ? "Pausing…" : "Pause"}
            </button>
            <button
              type="button"
              className="ghost danger-text"
              disabled={busy}
              onClick={() => void downloads.cancel(model.id)}
            >
              Cancel download
            </button>
          </>
        ) : model.state === "installed" ? (
          confirming ? (
            <>
              <span className="hint">
                Delete {model.name} ({formatSize(model.bytes)})? You can
                download it again later.
              </span>
              <button
                type="button"
                className="ghost danger-text"
                disabled={busy}
                onClick={() =>
                  void downloads
                    .remove(model.id)
                    .then(() => setConfirming(false))
                }
              >
                {pending("delete") ? "Deleting…" : "Delete"}
              </button>
              <button
                type="button"
                className="ghost"
                disabled={busy}
                onClick={() => setConfirming(false)}
              >
                Keep
              </button>
            </>
          ) : (
            <>
              {onUse && model.model_id && (
                <button
                  type="button"
                  className="primary"
                  onClick={() => onUse(model)}
                >
                  Use this model
                </button>
              )}
              <button
                type="button"
                className="ghost danger-text"
                disabled={busy}
                title="Deletes the downloaded file"
                onClick={() => setConfirming(true)}
              >
                Delete
              </button>
            </>
          )
        ) : resumable ? (
          <>
            <button
              type="button"
              className="primary"
              disabled={busy || Boolean(blocked)}
              title={blocked || undefined}
              onClick={() => void downloads.start(model.id)}
            >
              {pending("start") ? "Resuming…" : "Resume"}
            </button>
            <button
              type="button"
              className="ghost danger-text"
              disabled={busy}
              onClick={() => void downloads.cancel(model.id)}
            >
              Cancel download
            </button>
          </>
        ) : (
          <>
            <button
              type="button"
              className={model.recommended ? "primary" : "ghost"}
              disabled={busy || Boolean(blocked)}
              title={blocked || downloadEstimate(model.bytes)}
              onClick={() => void downloads.start(model.id)}
            >
              {pending("start")
                ? "Starting…"
                : model.state === "failed"
                  ? "Try again"
                  : `Download (${formatSize(model.bytes)})`}
            </button>
            {model.state === "failed" && (
              <button
                type="button"
                className="ghost"
                disabled={busy}
                onClick={() => void downloads.cancel(model.id)}
              >
                Dismiss
              </button>
            )}
          </>
        )}
      </div>
    </article>
  );
}

/** Settings › Local models › Download a free model. */
export function DownloadList({
  downloads,
  onUse,
}: {
  downloads: ModelDownloads;
  onUse?: (model: DownloadModel) => void;
}) {
  const catalog = downloads.catalog;
  if (!catalog)
    return downloads.loadError ? (
      <p className="health-bad" role="alert">
        {downloads.loadError}
      </p>
    ) : (
      <p role="status">Checking this computer…</p>
    );
  return (
    <div className="download-list">
      {catalog.offline && (
        <p className="warn-text">
          Offline mode is on, so nothing can be downloaded. Switch to Online in
          Settings › Permissions &amp; network.
        </p>
      )}
      {catalog.models.map((model) => (
        <DownloadRow
          key={model.id}
          model={model}
          catalog={catalog}
          downloads={downloads}
          onUse={onUse}
        />
      ))}
      <p className="hint">
        Each file is pinned to an exact version on Hugging Face and checked with
        SHA-256 before it is used. Files are saved in{" "}
        <code>{catalog.directory}</code>
        {catalog.free_bytes != null
          ? ` (${formatSize(catalog.free_bytes)} free)`
          : ""}
        .
      </p>
    </div>
  );
}
