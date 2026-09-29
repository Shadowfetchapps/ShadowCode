import { useEffect, useState } from "react";
import { api, type AlwaysAllowed } from "../../api";
import { GitHooksSetting } from "./GitHooksSetting";
import { NotificationFields, notifyPrefs } from "./NotificationFields";
import {
  SandboxSettings,
  sandboxPatch,
  sandboxValues,
} from "./SandboxSettings";
import "../../tools.css";

type Save = (values: Record<string, unknown>) => Promise<void>;

const VENDOR_NAMES: Record<string, string> = {
  codex: "Codex",
  claude: "Claude Code",
  cursor: "Cursor",
  antigravity: "Antigravity",
  grok: "Grok",
};

/** The disclosure label for one `permissions.vendor_notes` entry. The
 * engine keys ShadowCode's own enforcement as `native`; that key is never
 * shown. */
export function noteSummary(id: string) {
  if (id === "native")
    return "How ShadowCode applies this (local, API and OpenRouter models)";
  if (id === "network")
    return "How the network setting applies to subscriptions";
  const name = VENDOR_NAMES[id.replace(/^cli[-:]/, "")];
  return name
    ? `How ${name} applies this`
    : "How this subscription applies this";
}

/** ShadowCode first, the subscriptions, then the network note. */
function noteOrder(notes: Record<string, string>) {
  const rank = (id: string) => (id === "native" ? 0 : id === "network" ? 2 : 1);
  return Object.entries(notes).sort(([a], [b]) => rank(a) - rank(b));
}

/** The selected project's "Always allow in this project" commands, each
 * removable. */
export function AlwaysAllowedList() {
  const [commands, setCommands] = useState<AlwaysAllowed[] | null>(null);
  const [error, setError] = useState("");
  useEffect(() => {
    let live = true;
    Promise.resolve()
      .then(() => api.alwaysAllowed())
      .then((answer) => live && setCommands(answer.commands))
      .catch(() => live && setCommands([]));
    return () => {
      live = false;
    };
  }, []);
  async function remove(command: string) {
    setError("");
    try {
      const answer = await api.removeAlwaysAllowed(command);
      setCommands(answer.commands);
    } catch (e) {
      setError(String(e));
    }
  }
  return (
    <section
      className="always-allowed"
      aria-label="Always allowed in this project"
    >
      <h4>Always allowed in this project</h4>
      {commands === null ? (
        <p className="hint">Reading…</p>
      ) : commands.length === 0 ? (
        <p className="hint">
          Nothing yet. When ShadowCode asks to run a test, build or check
          command, choose “Always allow here” to stop it asking for that exact
          command in this project.
        </p>
      ) : (
        <ul className="always-list">
          {commands.map((rule) => (
            <li key={rule.command}>
              <code>{rule.command}</code>
              <button
                type="button"
                className="ghost"
                aria-label={`Stop always allowing ${rule.command}`}
                onClick={() => void remove(rule.command)}
              >
                Remove
              </button>
            </li>
          ))}
        </ul>
      )}
      {error && <p className="warn-text">{error}</p>}
    </section>
  );
}

/** Settings › Permissions & network. Saves only the permissions, network
 * and sandbox groups. */
export function PermissionsPage({
  cfg,
  onSave,
}: {
  cfg: Record<string, unknown>;
  onSave: Save;
}) {
  const permissions = (cfg.permissions || {}) as Record<string, unknown>;
  const network = (cfg.network || {}) as Record<string, unknown>;
  const notes = (permissions.vendor_notes || {}) as Record<string, string>;
  const originalLevel = String(permissions.level || "workspace");
  const [mode, setMode] = useState(
    permissions.mode === "allow_edits" ? "allow_edits" : "ask",
  );
  const [readOnly, setReadOnly] = useState(originalLevel === "read_only");
  const [netMode, setNetMode] = useState(String(network.mode || "online"));
  const [shell, setShell] = useState(() => sandboxValues(cfg));
  const [dangerous, setDangerous] = useState(
    permissions.require_approval_for_dangerous !== false,
  );
  const [saving, setSaving] = useState(false);
  return (
    <section className="settings-page">
      <h3>Permissions & network</h3>
      <fieldset className="mode-options">
        <legend>When the agent acts in a project</legend>
        <label className="mode-option">
          <input
            type="radio"
            name="permission-mode"
            checked={mode === "ask"}
            onChange={() => setMode("ask")}
          />
          <span>
            <strong>Ask before actions</strong>
            <small>File edits and shell commands wait for your approval.</small>
          </span>
        </label>
        <label className="mode-option">
          <input
            type="radio"
            name="permission-mode"
            checked={mode === "allow_edits"}
            onChange={() => setMode("allow_edits")}
          />
          <span>
            <strong>Allow project edits</strong>
            <small>
              File edits inside the project run without asking. Shell commands,
              changes outside the project and destructive Git commands still
              ask.
            </small>
          </span>
        </label>
      </fieldset>
      {Object.keys(notes).length > 0 && (
        <div className="vendor-notes">
          <p className="hint">
            ShadowCode enforces this for models it runs itself. Subscriptions
            run in their vendor's own sandbox and apply it their way.
          </p>
          {noteOrder(notes).map(([id, note]) => (
            <details key={id} className="vendor-note">
              <summary>{noteSummary(id)}</summary>
              <p>{note}</p>
            </details>
          ))}
        </div>
      )}
      <AlwaysAllowedList />
      <GitHooksSetting />
      <fieldset className="mode-options">
        <legend>Network</legend>
        {[
          [
            "online",
            "Online",
            "Cloud subscriptions and web tools for local models are available.",
          ],
          [
            "web_off",
            "Web tools off",
            "Local models cannot fetch web pages. Subscriptions still work.",
          ],
          [
            "offline",
            "Offline",
            "Only models on this computer run. Account and usage checks stop.",
          ],
        ].map(([id, label, hint]) => (
          <label className="mode-option" key={id}>
            <input
              type="radio"
              name="network-mode"
              checked={netMode === id}
              onChange={() => setNetMode(id)}
            />
            <span>
              <strong>{label}</strong>
              <small>{hint}</small>
            </span>
          </label>
        ))}
      </fieldset>
      <SandboxSettings values={shell} onChange={setShell} />
      <details className="advanced-options">
        <summary>Advanced</summary>
        <label className="check">
          <input
            type="checkbox"
            checked={readOnly}
            onChange={(e) => setReadOnly(e.target.checked)}
          />{" "}
          Read-only projects: the agent can inspect but never edit or run
          commands
        </label>
        <label className="check">
          <input
            type="checkbox"
            checked={dangerous}
            onChange={(e) => setDangerous(e.target.checked)}
          />{" "}
          Ask before destructive commands (rm -rf, git push --force). Privileged
          commands such as sudo are always blocked.
        </label>
      </details>
      <div className="row end settings-foot">
        <button
          type="button"
          className="primary"
          disabled={saving}
          onClick={() => {
            setSaving(true);
            const patch = sandboxPatch(shell);
            void onSave({
              permissions: {
                mode,
                level: readOnly
                  ? "read_only"
                  : originalLevel === "read_only"
                    ? "workspace"
                    : originalLevel,
                ...patch.permissions,
                require_approval_for_dangerous: dangerous,
              },
              network: { mode: netMode, ...patch.network },
              sandbox: patch.sandbox,
            }).finally(() => setSaving(false));
          }}
        >
          {saving ? "Saving…" : "Save"}
        </button>
      </div>
    </section>
  );
}

/** Settings › Appearance. Saves only the ui group. */
export function AppearancePage({
  cfg,
  onSave,
}: {
  cfg: Record<string, unknown>;
  onSave: Save;
}) {
  const ui = (cfg.ui || {}) as Record<string, unknown>;
  const [theme, setTheme] = useState(String(ui.theme || "system"));
  const [notify, setNotify] = useState(() => notifyPrefs(ui));
  const [saving, setSaving] = useState(false);
  return (
    <section className="settings-page">
      <h3>Appearance</h3>
      <fieldset className="seg-field">
        <legend>Theme</legend>
        <div className="seg" role="radiogroup" aria-label="Theme">
          {[
            ["system", "System"],
            ["light", "Light"],
            ["dark", "Dark"],
          ].map(([id, label]) => (
            <button
              type="button"
              role="radio"
              aria-checked={theme === id}
              key={id}
              className={theme === id ? "on" : ""}
              onClick={() => setTheme(id)}
            >
              {label}
            </button>
          ))}
        </div>
      </fieldset>
      <NotificationFields value={notify} onChange={setNotify} />
      <div className="row end settings-foot">
        <button
          type="button"
          className="primary"
          disabled={saving}
          onClick={() => {
            setSaving(true);
            void onSave({ ui: { theme, ...notify } }).finally(() =>
              setSaving(false),
            );
          }}
        >
          {saving ? "Saving…" : "Save"}
        </button>
      </div>
    </section>
  );
}
