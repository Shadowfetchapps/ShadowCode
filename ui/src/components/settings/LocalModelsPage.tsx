import { useCallback, useEffect, useState } from "react";
import { Eye, MessageSquareText } from "lucide-react";
import { api, type GgufEntry, type LocalCatalog } from "../../api";
import { pickLocalModel } from "../../lib/transport";

export function formatBytes(bytes: number | null | undefined): string {
  if (!bytes || bytes <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit++;
  }
  return `${value >= 10 || unit === 0 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

const FITS: Record<string, string> = {
  gpu: "fits in GPU memory",
  cpu: "runs on CPU memory (slower)",
  no: "does not fit in this computer's memory",
};

/** Settings › Local models: the managed llama.cpp runtime, this computer's
 * hardware, GGUF files the user added, and weights found in the Ollama store. */
export function LocalModelsPage({
  onChanged,
  onToast,
}: {
  onChanged: () => void;
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const [catalog, setCatalog] = useState<LocalCatalog | null>(null);
  const [error, setError] = useState("");
  const [path, setPath] = useState("");
  const [pending, setPending] = useState("");

  const load = useCallback(async () => {
    setError("");
    try {
      setCatalog(await api.localModels());
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  async function run(
    key: string,
    action: () => Promise<unknown>,
    done?: string,
  ) {
    setPending(key);
    try {
      const result = (await action()) as
        { local_engine?: LocalCatalog } | undefined;
      if (result && typeof result === "object" && result.local_engine)
        setCatalog(result.local_engine);
      else await load();
      if (done) onToast(done, "ok");
      onChanged();
    } catch (e) {
      onToast(String(e), "err");
      await load();
    } finally {
      setPending("");
    }
  }

  async function choose(folder: boolean) {
    try {
      const picked = await pickLocalModel(folder);
      if (picked) setPath(picked);
    } catch (e) {
      onToast(String(e), "err");
    }
  }

  const hardware = catalog?.hardware;
  const runtime = catalog?.runtime;
  const loaded = catalog?.loaded;
  const models = catalog?.models || [];
  const ollama = catalog?.ollama_store;
  const hardwareLine = hardware
    ? [
        hardware.cpu_cores ? `${hardware.cpu_cores} CPU threads` : "",
        hardware.ram_bytes ? `${formatBytes(hardware.ram_bytes)} RAM` : "",
        hardware.gpu
          ? `${hardware.gpu}${hardware.vram_bytes ? ` (${formatBytes(hardware.vram_bytes)})` : ""}`
          : "",
      ]
        .filter(Boolean)
        .join(" · ")
    : "";
  return (
    <section className="settings-page">
      <h3>Local models</h3>
      <p className="hint">
        GGUF models run on this computer through ShadowCode's managed llama.cpp
        runtime, one at a time. Nothing is downloaded; you add weights you
        already have. Removing an entry never deletes the file.
      </p>
      {error && (
        <p className="health-bad" role="alert">
          {error}
        </p>
      )}
      {!catalog && !error && <p role="status">Reading local models…</p>}
      {catalog && (
        <>
          <div className="kv local-runtime">
            <div>
              <span>Runtime</span>
              <code
                className={
                  runtime?.state === "ready" ? "health-ok" : "health-bad"
                }
              >
                {runtime?.state === "ready"
                  ? `Ready · ${runtime.backend === "vulkan" ? "Vulkan" : runtime.backend === "cpu" ? "CPU" : runtime.backend || "unknown backend"}${runtime.version ? ` · ${runtime.version}` : ""}`
                  : runtime?.state === "setup_required"
                    ? "Setup required"
                    : "Unavailable"}
              </code>
            </div>
            {runtime?.detail && (
              <div>
                <span>Detail</span>
                <code>{runtime.detail}</code>
              </div>
            )}
            {hardwareLine && (
              <div>
                <span>Hardware</span>
                <code>{hardwareLine}</code>
              </div>
            )}
            {hardware?.devices && hardware.devices.length > 0 && (
              <div>
                <span>Devices</span>
                <code>{hardware.devices.join(" · ")}</code>
              </div>
            )}
            <div>
              <span>Loaded</span>
              <code>
                {loaded
                  ? `${loaded.name}${loaded.backend ? ` · ${loaded.backend}` : ""}${loaded.context_tokens ? ` · ${loaded.context_tokens.toLocaleString()} tokens context` : ""}`
                  : "No model loaded"}
              </code>
            </div>
          </div>
          {loaded && (
            <button
              type="button"
              className="ghost"
              disabled={Boolean(pending)}
              onClick={() =>
                void run(
                  "unload",
                  () => api.unloadLocalModel(),
                  "Model unloaded",
                )
              }
            >
              {pending === "unload" ? "Unloading…" : `Unload ${loaded.name}`}
            </button>
          )}
          <h4>Your models</h4>
          {models.length === 0 && (
            <p className="hint">No GGUF models added yet.</p>
          )}
          {models.map((model) => (
            <ModelRow
              key={model.id}
              model={model}
              loaded={loaded?.id === model.id}
              pending={pending}
              vram={hardware?.vram_bytes || null}
              ram={hardware?.ram_bytes || null}
              onLoad={() =>
                void run(
                  `load:${model.id}`,
                  () => api.loadLocalModel(model.id),
                  `${model.name} loaded`,
                )
              }
              onUnload={() =>
                void run(
                  "unload",
                  () => api.unloadLocalModel(),
                  "Model unloaded",
                )
              }
              onRemove={() =>
                void run(
                  `remove:${model.id}`,
                  () => api.removeLocalModel(model.path),
                  "Removed from the list. The model file was not deleted.",
                )
              }
            />
          ))}
          <h4>Add a model</h4>
          <div className="field">
            <label htmlFor="local-gguf">GGUF file or folder</label>
            <input
              id="local-gguf"
              value={path}
              onChange={(e) => setPath(e.target.value)}
              placeholder="/path/to/model.gguf or a folder"
            />
          </div>
          <div className="chip-row">
            <button
              type="button"
              className="ghost"
              onClick={() => void choose(false)}
            >
              Choose file…
            </button>
            <button
              type="button"
              className="ghost"
              onClick={() => void choose(true)}
            >
              Choose folder…
            </button>
            <button
              type="button"
              className="primary"
              disabled={!path.trim() || Boolean(pending)}
              onClick={() =>
                void run(
                  "add",
                  () => api.addLocalModel(path.trim()),
                  "Added",
                ).then(() => setPath(""))
              }
            >
              {pending === "add" ? "Adding…" : "Add"}
            </button>
          </div>
          {ollama && (
            <>
              <h4>Found in Ollama</h4>
              {!ollama.available ? (
                <p className="hint">
                  No Ollama model store found
                  {ollama.path ? ` at ${ollama.path}` : ""}.
                </p>
              ) : ollama.models.length === 0 ? (
                <p className="hint">
                  The Ollama store at {ollama.path} has no models.
                </p>
              ) : (
                <>
                  <p className="hint">
                    Importing uses the existing files in {ollama.path}; nothing
                    is copied or changed there.
                  </p>
                  <ul className="ollama-list">
                    {ollama.models.map((m) => (
                      <li key={m.tag}>
                        <span>
                          <strong>{m.tag}</strong>{" "}
                          <span className="dim">
                            {formatBytes(m.bytes)}
                            {m.projector ? " · vision projector" : ""}
                          </span>
                          {!m.compatible && (
                            <span className="health-bad"> · {m.reason}</span>
                          )}
                        </span>
                        {m.already_added ? (
                          <span className="dim">Added</span>
                        ) : m.compatible ? (
                          <button
                            type="button"
                            className="mini"
                            disabled={Boolean(pending)}
                            onClick={() =>
                              void run(
                                `import:${m.tag}`,
                                () => api.importOllama(m.tag),
                                `${m.tag} imported`,
                              )
                            }
                          >
                            {pending === `import:${m.tag}`
                              ? "Importing…"
                              : "Import"}
                          </button>
                        ) : (
                          <span className="dim">Not supported</span>
                        )}
                      </li>
                    ))}
                  </ul>
                </>
              )}
            </>
          )}
        </>
      )}
    </section>
  );
}

function ModelRow({
  model,
  loaded,
  pending,
  vram,
  ram,
  onLoad,
  onUnload,
  onRemove,
}: {
  model: GgufEntry;
  loaded: boolean;
  pending: string;
  vram: number | null;
  ram: number | null;
  onLoad: () => void;
  onUnload: () => void;
  onRemove: () => void;
}) {
  const memory = model.memory;
  const loading = pending === `load:${model.id}`;
  return (
    <article className="local-model" aria-label={model.name}>
      <header>
        <strong>{model.name}</strong>
        {model.vision && (
          <span className="cap-badge">
            <Eye size={11} aria-hidden="true" /> Vision
          </span>
        )}
        {model.tools && model.tools_basis === "template_hint" && (
          <span className="cap-badge" title={model.tools_reason}>
            Tool hint
          </span>
        )}
        {model.tools && model.tools_basis === "known_template_profile" && (
          <span className="cap-badge" title={model.tools_reason}>
            Tool template
          </span>
        )}
        {model.tools === false && (
          <span className="cap-badge" title={model.tools_reason}>
            <MessageSquareText size={11} aria-hidden="true" /> Chat only
          </span>
        )}
        {loaded && <span className="avail avail-ready">Loaded</span>}
      </header>
      <p className="hint path">{model.path}</p>
      <p>
        {model.compatible ? (
          <span className="health-ok">Compatible</span>
        ) : (
          <span className="health-bad">Not compatible</span>
        )}
        {model.reason ? ` · ${model.reason}` : ""}
        {model.architecture && !model.reason?.includes(model.architecture)
          ? ` · ${model.architecture}`
          : ""}
        {` · ${formatBytes(model.bytes)}`}
      </p>
      {memory && (
        <p className="hint">
          Needs about {formatBytes(memory.total_bytes)} (weights{" "}
          {formatBytes(memory.weights_bytes)}, context cache{" "}
          {formatBytes(memory.kv_cache_bytes)}
          {memory.projector_bytes
            ? `, vision projector ${formatBytes(memory.projector_bytes)}`
            : ""}
          ) at{" "}
          {(memory.context_tokens || model.context_tokens).toLocaleString()}{" "}
          tokens
          {" · "}
          {vram ? `GPU ${formatBytes(vram)}` : "no GPU memory reported"}
          {ram ? ` · RAM ${formatBytes(ram)}` : ""}
          {model.fits ? ` · ${FITS[model.fits] || model.fits}` : ""}
        </p>
      )}
      {model.last_error && <p className="health-bad">{model.last_error}</p>}
      <div className="row">
        {loaded ? (
          <button
            type="button"
            className="ghost"
            disabled={Boolean(pending)}
            onClick={onUnload}
          >
            Unload
          </button>
        ) : (
          <button
            type="button"
            className="ghost"
            disabled={
              Boolean(pending) || !model.compatible || model.fits === "no"
            }
            title={
              !model.compatible
                ? model.reason
                : model.fits === "no"
                  ? "Not enough memory on this computer"
                  : undefined
            }
            onClick={onLoad}
          >
            {loading ? "Loading…" : "Load"}
          </button>
        )}
        <button
          type="button"
          className="ghost danger-text"
          disabled={Boolean(pending)}
          title="Removes the entry; the file stays on disk"
          onClick={onRemove}
        >
          Remove
        </button>
      </div>
    </article>
  );
}
