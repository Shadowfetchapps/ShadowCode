import { useCallback, useEffect, useState } from "react";
import {
  api,
  type CodeIntelSettings,
  type CodeIntelStatus,
  type EmbeddingModel,
} from "../../api";
import { formatBytes } from "./LocalModelsPage";
import { LoadError } from "../cards";

const BUSY = new Set(["installing", "downloading", "verifying", "running"]);
const MAP_SIZES = [
  { tokens: 0, label: "Off" },
  { tokens: 512, label: "Small (512 tokens)" },
  { tokens: 1024, label: "Normal (1,024 tokens)" },
  { tokens: 2048, label: "Large (2,048 tokens)" },
  { tokens: 4096, label: "Very large (4,096 tokens)" },
];

function sourceLabel(source?: string): string {
  if (source === "managed") return "installed by ShadowCode";
  if (source === "config") return "from your settings";
  return "found on this computer";
}

/** Settings › Code intelligence: language servers (with optional managed
 * installs), the repository map, and code search with an optional embedding
 * model. Nothing downloads unless the user clicks Install. */
export function CodeIntelPage({
  onToast,
}: {
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const [status, setStatus] = useState<CodeIntelStatus | null>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState("");

  const load = useCallback(async () => {
    try {
      setStatus(await api.codeIntelStatus());
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  // Poll while a download, install or background indexing runs.
  const busy = Boolean(
    status &&
    (status.managed.some((m) => BUSY.has(m.progress?.state || "")) ||
      status.embeddings.models.some((m) => BUSY.has(m.progress?.state || "")) ||
      BUSY.has(status.embeddings.backfill?.state || "")),
  );
  useEffect(() => {
    if (!busy) return;
    const timer = window.setInterval(() => void load(), 1500);
    return () => window.clearInterval(timer);
  }, [busy, load]);

  async function run(key: string, action: () => Promise<unknown>, done = "") {
    setPending(key);
    try {
      await action();
      if (done) onToast(done, "ok");
    } catch (e) {
      onToast(String(e), "err");
    } finally {
      setPending("");
      await load();
    }
  }

  function save(values: Partial<CodeIntelSettings>) {
    void run("settings", () => api.saveCodeIntel(values));
  }

  if (!status) {
    return (
      <section className="settings-page">
        <h3>Code intelligence</h3>
        {error ? (
          <LoadError message={error} onRetry={load} />
        ) : (
          <p role="status">Reading code intelligence status…</p>
        )}
      </section>
    );
  }
  const config = status.config;
  const managed = new Map(status.managed.map((m) => [m.id, m]));
  const index = status.index;
  const coverage = status.embeddings.coverage;
  const backfill = status.embeddings.backfill;
  return (
    <section className="settings-page">
      <h3>Code intelligence</h3>
      <p className="hint">
        Helps the agent find its way around a project: language servers report
        errors right after each edit, a ranked map of the code goes into the
        prompt, and search_code finds code by keywords or meaning. Everything
        runs on this computer.
      </p>
      {status.config_error && (
        <p className="health-bad" role="alert">
          Your code_intel settings are invalid, so defaults are in use:{" "}
          {status.config_error}
        </p>
      )}

      <h4>Language servers</h4>
      <label className="check">
        <input
          type="checkbox"
          checked={config.lsp}
          disabled={pending === "settings"}
          onChange={(e) => save({ lsp: e.target.checked })}
        />{" "}
        Start language servers for the files the agent works on
      </label>
      <label className="check">
        <input
          type="checkbox"
          checked={config.diagnostics_on_edit}
          disabled={!config.lsp || pending === "settings"}
          onChange={(e) => save({ diagnostics_on_edit: e.target.checked })}
        />{" "}
        Tell the agent about errors its edits introduce (waits up to{" "}
        {(config.diagnostics_wait_ms / 1000).toLocaleString()} s per edit)
      </label>
      <p className="hint">
        Servers run without building or running project code (no build scripts,
        no downloads). An idle server stops after {config.lsp_idle_minutes}{" "}
        minutes.
      </p>
      <ul className="code-intel-languages">
        {status.languages.map((language) => {
          const pkg = language.managed_package
            ? managed.get(language.managed_package)
            : undefined;
          const progress = pkg?.progress;
          const installing = progress?.state === "installing";
          return (
            <li key={language.language} aria-label={language.label}>
              <div>
                <strong>{language.label}</strong>{" "}
                {language.available ? (
                  <span className="health-ok">
                    {language.server} · {sourceLabel(language.source)}
                  </span>
                ) : (
                  <span className="dim">Not installed</span>
                )}
                {!language.available && language.note && (
                  <p className="hint">{language.note}</p>
                )}
                {progress?.state === "error" && progress.error && (
                  <p className="health-bad">{progress.error}</p>
                )}
              </div>
              {pkg && (
                <div className="row">
                  {pkg.installed ? (
                    <>
                      <span className="dim">
                        {pkg.installed_bytes
                          ? formatBytes(pkg.installed_bytes)
                          : "Installed"}
                      </span>
                      <button
                        type="button"
                        className="mini ghost danger-text"
                        disabled={Boolean(pending) || installing}
                        onClick={() =>
                          void run(
                            `remove:${pkg.id}`,
                            () => api.removeLanguageServer(pkg.id),
                            `${pkg.label} server removed`,
                          )
                        }
                      >
                        Remove
                      </button>
                    </>
                  ) : (
                    <button
                      type="button"
                      className="mini"
                      disabled={
                        Boolean(pending) ||
                        installing ||
                        status.offline ||
                        !status.npm.available
                      }
                      title={
                        status.offline
                          ? "Offline mode is on"
                          : !status.npm.available
                            ? "npm was not found"
                            : pkg.packages.join(", ")
                      }
                      onClick={() =>
                        void run(`install:${pkg.id}`, () =>
                          api.installLanguageServer(pkg.id),
                        )
                      }
                    >
                      {installing
                        ? "Installing…"
                        : `Install (about ${formatBytes(pkg.approx_bytes)})`}
                    </button>
                  )}
                </div>
              )}
            </li>
          );
        })}
      </ul>
      {!status.npm.available && (
        <p className="hint">
          Installing the TypeScript or Python server needs Node.js with npm.
        </p>
      )}
      {status.offline && (
        <p className="hint">
          Offline mode is on, so nothing can be downloaded.
        </p>
      )}
      {status.servers.length > 0 && (
        <div className="row">
          <span className="dim">
            {status.servers.filter((s) => s.state === "ready").length} running
          </span>
          <button
            type="button"
            className="mini ghost"
            disabled={Boolean(pending)}
            onClick={() =>
              void run(
                "stop",
                () => api.stopLanguageServers(),
                "Language servers stopped",
              )
            }
          >
            Stop all
          </button>
        </div>
      )}

      <h4>Repository map</h4>
      <div className="field">
        <label htmlFor="repo-map-size">Map in the agent's prompt</label>
        <select
          id="repo-map-size"
          value={
            MAP_SIZES.some((s) => s.tokens === config.repo_map_tokens)
              ? config.repo_map_tokens
              : 1024
          }
          disabled={pending === "settings"}
          onChange={(e) => save({ repo_map_tokens: Number(e.target.value) })}
        >
          {MAP_SIZES.map((size) => (
            <option key={size.tokens} value={size.tokens}>
              {size.label}
            </option>
          ))}
        </select>
      </div>
      <p className="hint">
        The most-used definitions, ranked by how the code refers to them. It is
        left out for models with small context windows.
      </p>

      <h4>Code search</h4>
      <div className="kv">
        <div>
          <span>Index</span>
          <code>
            {index && index.files > 0
              ? `${index.files.toLocaleString()} files · ${index.symbols.toLocaleString()} definitions · ${index.chunks.toLocaleString()} chunks`
              : "Not built yet (it builds on first use)"}
          </code>
        </div>
        {coverage && (
          <div>
            <span>Vectors</span>
            <code>
              {coverage.embedded.toLocaleString()} of{" "}
              {coverage.chunks.toLocaleString()} chunks
              {backfill?.state === "running" ? " · indexing…" : ""}
            </code>
          </div>
        )}
      </div>
      {backfill?.state === "error" && backfill.error && (
        <p className="health-bad">{backfill.error}</p>
      )}
      <div className="row">
        <button
          type="button"
          className="ghost"
          disabled={Boolean(pending)}
          onClick={() =>
            void run("reindex", () => api.reindexCode(), "Index updated")
          }
        >
          {pending === "reindex" ? "Indexing…" : "Reindex this project"}
        </button>
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={config.semantic_search}
          disabled={pending === "settings"}
          onChange={(e) => save({ semantic_search: e.target.checked })}
        />{" "}
        Also search by meaning with an embedding model (when one is installed)
      </label>
      <p className="hint">
        Embedding models run on the bundled llama.cpp. They are downloaded from
        Hugging Face only when you click Install, and the file is checked
        against a pinned SHA-256.
        {!status.embeddings.runtime &&
          " The llama.cpp runtime was not found, so semantic search is unavailable."}
      </p>
      {status.embeddings.models.map((model) => (
        <EmbeddingRow
          key={model.id}
          model={model}
          pending={pending}
          offline={status.offline}
          onInstall={() =>
            void run(`model:${model.id}`, () =>
              api.installEmbeddingModel(model.id),
            )
          }
          onRemove={() =>
            void run(
              `model:${model.id}`,
              () => api.removeEmbeddingModel(model.id),
              `${model.name} removed`,
            )
          }
          onUse={() => save({ embedding_model: model.id })}
        />
      ))}
    </section>
  );
}

function EmbeddingRow({
  model,
  pending,
  offline,
  onInstall,
  onRemove,
  onUse,
}: {
  model: EmbeddingModel;
  pending: string;
  offline: boolean;
  onInstall: () => void;
  onRemove: () => void;
  onUse: () => void;
}) {
  const progress = model.progress;
  const downloading =
    progress?.state === "downloading" || progress?.state === "verifying";
  const percent =
    progress && progress.total > 0
      ? Math.floor((progress.done / progress.total) * 100)
      : 0;
  return (
    <article className="local-model" aria-label={model.name}>
      <header>
        <strong>{model.name}</strong>
        {model.active && <span className="avail avail-ready">In use</span>}
      </header>
      <p className="hint">
        {model.summary} {formatBytes(model.bytes)} · {model.license}
      </p>
      {downloading && (
        <p role="status">
          {progress?.state === "verifying"
            ? "Checking the download…"
            : `Downloading… ${percent}%`}
        </p>
      )}
      {progress?.state === "error" && progress.error && (
        <p className="health-bad">{progress.error}</p>
      )}
      <div className="row">
        {model.installed ? (
          <>
            {!model.active && (
              <button
                type="button"
                className="ghost"
                disabled={Boolean(pending)}
                onClick={onUse}
              >
                Use this model
              </button>
            )}
            <button
              type="button"
              className="ghost danger-text"
              disabled={Boolean(pending)}
              onClick={onRemove}
            >
              Remove
            </button>
          </>
        ) : (
          <button
            type="button"
            className="ghost"
            disabled={Boolean(pending) || downloading || offline}
            title={offline ? "Offline mode is on" : undefined}
            onClick={onInstall}
          >
            {downloading
              ? "Downloading…"
              : `Install (${formatBytes(model.bytes)})`}
          </button>
        )}
      </div>
    </article>
  );
}
