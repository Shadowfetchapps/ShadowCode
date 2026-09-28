import { useCallback, useEffect, useState } from "react";
import {
  voiceApi,
  type VoiceModel,
  type VoiceSettings,
  type VoiceStatus,
} from "../../lib/voice";
import { formatBytes } from "./LocalModelsPage";
import { LoadError } from "../cards";

const LENGTHS = [
  { seconds: 30, label: "30 seconds" },
  { seconds: 60, label: "1 minute" },
  { seconds: 120, label: "2 minutes" },
  { seconds: 300, label: "5 minutes" },
];

/** Settings › Voice: where dictation is transcribed (on this computer by
 * default, OpenRouter only if chosen), whisper models to install or remove,
 * language and small options. Nothing downloads unless the user clicks
 * Install. */
export function VoicePage({
  onToast,
}: {
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const [status, setStatus] = useState<VoiceStatus | null>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState("");
  const [cloudModel, setCloudModel] = useState("");

  const load = useCallback(async () => {
    try {
      const next = await voiceApi.status();
      setStatus(next);
      setCloudModel((current) => current || next.config.openrouter_model);
      setError("");
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  const downloading = Boolean(
    status?.models.some((m) => m.progress?.state === "downloading"),
  );
  useEffect(() => {
    if (!downloading) return;
    const timer = window.setInterval(() => void load(), 1000);
    return () => window.clearInterval(timer);
  }, [downloading, load]);

  async function run(key: string, action: () => Promise<unknown>, done = "") {
    setPending(key);
    try {
      await action();
      if (done) onToast(done, "ok");
    } catch (e) {
      onToast(e instanceof Error ? e.message : String(e), "err");
    } finally {
      setPending("");
      await load();
    }
  }
  const save = (values: Partial<VoiceSettings>) =>
    void run("settings", () => voiceApi.save(values));

  if (!status) {
    return (
      <section className="settings-page">
        <h3>Voice</h3>
        {error ? (
          <LoadError message={error} onRetry={load} />
        ) : (
          <p role="status">Reading voice settings…</p>
        )}
      </section>
    );
  }
  const config = status.config;
  const chosen =
    status.models.find((m) => m.active) ||
    status.models.find((m) => m.id === config.model);
  const englishOnly = config.engine === "local" && chosen?.english_only;
  const cloudBlocked = !status.openrouter_key
    ? "Add an OpenRouter API key in Settings › Accounts first."
    : status.offline
      ? "Offline mode is on."
      : null;
  return (
    <section className="settings-page">
      <h3>Voice</h3>
      <p className="hint">
        Dictate into the message box: hold the microphone button (or
        Ctrl+Shift+Space) while you talk, or click it once to start and again to
        stop. The text is inserted where the cursor is and is never sent on its
        own. ShadowCode records from your system&apos;s default microphone;
        choose it in your sound settings.
      </p>
      {status.blocked && (
        <p className="health-bad" role="status">
          {status.blocked}
        </p>
      )}

      <h4>Transcribe with</h4>
      <label className="check">
        <input
          type="radio"
          name="voice-engine"
          checked={config.engine === "local"}
          disabled={Boolean(pending)}
          onChange={() => save({ engine: "local" })}
        />{" "}
        This computer (whisper.cpp) — audio never leaves the machine
      </label>
      {!status.cpu_supported && (
        <p className="hint">
          This processor lacks AVX2, which the built-in whisper.cpp needs.
        </p>
      )}
      <label className="check">
        <input
          type="radio"
          name="voice-engine"
          checked={config.engine === "openrouter"}
          disabled={Boolean(pending) || Boolean(cloudBlocked)}
          onChange={() => save({ engine: "openrouter" })}
        />{" "}
        OpenRouter — each recording is uploaded and billed to your key
      </label>
      <p className="hint">
        {cloudBlocked ||
          "About a tenth of a US cent per minute of speech with the default model. Used only while this option is selected."}
      </p>

      <h4>Models on this computer</h4>
      <p className="hint">
        Downloaded from Hugging Face only when you click Install and checked
        against a pinned SHA-256. whisper.cpp {status.whisper_version} runs on
        the CPU.
      </p>
      {status.models.map((model) => (
        <ModelRow
          key={model.id}
          model={model}
          pending={pending}
          offline={status.offline}
          onInstall={() =>
            void run(`model:${model.id}`, () => voiceApi.install(model.id))
          }
          onRemove={() =>
            void run(
              `model:${model.id}`,
              () => voiceApi.remove(model.id),
              `${model.name} removed`,
            )
          }
          onUse={() => save({ model: model.id })}
        />
      ))}

      <h4>Options</h4>
      <label className="field">
        <span>Language</span>
        <select
          value={englishOnly ? "en" : config.language}
          disabled={Boolean(pending) || englishOnly}
          onChange={(e) => save({ language: e.target.value })}
        >
          {status.languages.map((language) => (
            <option key={language.code} value={language.code}>
              {language.name}
            </option>
          ))}
        </select>
      </label>
      {englishOnly && (
        <p className="hint">
          {chosen?.name} understands English only. Use the multilingual model
          for other languages.
        </p>
      )}
      {config.engine === "openrouter" && (
        <label className="field">
          <span>OpenRouter model</span>
          <input
            value={cloudModel}
            spellCheck={false}
            disabled={Boolean(pending)}
            onChange={(e) => setCloudModel(e.target.value)}
            onBlur={() => {
              const next = cloudModel.trim();
              if (next && next !== config.openrouter_model)
                save({ openrouter_model: next });
            }}
          />
        </label>
      )}
      <label className="field">
        <span>Longest recording</span>
        <select
          value={config.max_seconds}
          disabled={Boolean(pending)}
          onChange={(e) => save({ max_seconds: Number(e.target.value) })}
        >
          {LENGTHS.some((l) => l.seconds === config.max_seconds) ? null : (
            <option value={config.max_seconds}>
              {config.max_seconds} seconds
            </option>
          )}
          {LENGTHS.map((l) => (
            <option key={l.seconds} value={l.seconds}>
              {l.label}
            </option>
          ))}
        </select>
      </label>
      <label className="check">
        <input
          type="checkbox"
          checked={config.voice_commands}
          disabled={Boolean(pending)}
          onChange={(e) => save({ voice_commands: e.target.checked })}
        />{" "}
        Say &ldquo;new line&rdquo; or &ldquo;new paragraph&rdquo; to break lines
      </label>
      <label className="check">
        <input
          type="checkbox"
          checked={config.live_preview}
          disabled={Boolean(pending) || config.engine !== "local"}
          onChange={(e) => save({ live_preview: e.target.checked })}
        />{" "}
        Show words while I talk (uses more CPU)
      </label>
    </section>
  );
}

function ModelRow({
  model,
  pending,
  offline,
  onInstall,
  onRemove,
  onUse,
}: {
  model: VoiceModel;
  pending: string;
  offline: boolean;
  onInstall: () => void;
  onRemove: () => void;
  onUse: () => void;
}) {
  const progress = model.progress;
  const downloading = progress?.state === "downloading";
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
      {downloading && <p role="status">Downloading… {percent}%</p>}
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
