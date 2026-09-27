import { useEffect, useState } from "react";
import { api, type SandboxStatus } from "../../api";

export type ShellNetwork = "off" | "allowlist" | "on";
export type SandboxValues = {
  require: boolean;
  network: ShellNetwork;
  /** One allowed host per line. */
  allow: string;
};

/** Current values from the saved configuration. Shell network is off
 * whenever `permissions.network` is off. */
export function sandboxValues(cfg: Record<string, unknown>): SandboxValues {
  const permissions = (cfg.permissions || {}) as Record<string, unknown>;
  const network = (cfg.network || {}) as Record<string, unknown>;
  const sandbox = (cfg.sandbox || {}) as Record<string, unknown>;
  const shell = String(network.shell || "on");
  return {
    require: sandbox.require === true,
    network: !permissions.network
      ? "off"
      : shell === "allowlist" || shell === "off"
        ? shell
        : "on",
    allow: Array.isArray(network.allow) ? network.allow.join("\n") : "",
  };
}

/** The configuration groups these values change. */
export function sandboxPatch(values: SandboxValues) {
  return {
    permissions: { network: values.network !== "off" },
    network: {
      shell: values.network,
      allow: values.allow
        .split(/[\n,]/)
        .map((line) => line.trim())
        .filter(Boolean),
    },
    sandbox: { require: values.require },
  };
}

function statusLine(status: SandboxStatus): string {
  switch (status.effective) {
    case "bubblewrap":
      return "Sandbox active (bubblewrap): commands see an empty home folder with read-only toolchains, and only this project is writable.";
    case "landlock":
      return "bubblewrap is unavailable. Commands can fall back to Landlock file limits; network and process isolation are incomplete. Turn on Require sandbox to refuse fallback.";
    case "blocked":
      return status.bubblewrap.works
        ? "The host allow-list needs private network namespaces, which this system does not allow. Shell commands will be refused."
        : "bubblewrap is not available, so shell commands will be refused. Install bubblewrap, or turn off Require sandbox and the host allow-list.";
    default:
      return "No sandbox is available: commands run with your user's full access. Install bubblewrap to isolate them.";
  }
}

const NETWORK_OPTIONS: [ShellNetwork, string, string][] = [
  [
    "off",
    "No network",
    "Request no network access. Full isolation needs working bubblewrap.",
  ],
  [
    "allowlist",
    "Only allowed hosts",
    "Web requests go through a filter that lets only the hosts below through. Needs bubblewrap.",
  ],
  ["on", "Full network", "Approved shell commands can reach any host."],
];

/** Settings › Permissions & network: the shell sandbox and its network. */
export function SandboxSettings({
  values,
  onChange,
}: {
  values: SandboxValues;
  onChange: (values: SandboxValues) => void;
}) {
  const [status, setStatus] = useState<SandboxStatus | null>(null);
  useEffect(() => {
    let live = true;
    api
      .sandboxStatus()
      .then((s) => live && setStatus(s))
      .catch(() => live && setStatus(null));
    return () => {
      live = false;
    };
  }, []);
  return (
    <fieldset className="mode-options sandbox-settings">
      <legend>Agent command sandbox</legend>
      <p className="hint">
        These settings apply to ShadowCode's built-in command tool. Background
        processes, lifecycle hooks and terminal sessions run outside this
        sandbox. Subscription tools use their vendor's permissions and sandbox.
      </p>
      {status && (
        <p className="hint" role="status">
          {statusLine(status)}
        </p>
      )}
      <label className="check">
        <input
          type="checkbox"
          checked={values.require}
          onChange={(e) => onChange({ ...values, require: e.target.checked })}
        />{" "}
        Require sandbox: refuse shell commands when bubblewrap is unavailable
      </label>
      {NETWORK_OPTIONS.map(([id, label, hint]) => (
        <label className="mode-option" key={id}>
          <input
            type="radio"
            name="shell-network"
            checked={values.network === id}
            onChange={() => onChange({ ...values, network: id })}
          />
          <span>
            <strong>{label}</strong>
            <small>{hint}</small>
          </span>
        </label>
      ))}
      {values.network === "allowlist" && (
        <div className="field">
          <label htmlFor="shell-allow">Allowed hosts</label>
          <textarea
            id="shell-allow"
            rows={4}
            value={values.allow}
            onChange={(e) => onChange({ ...values, allow: e.target.value })}
            placeholder={
              "crates.io\nstatic.crates.io\nregistry.npmjs.org\n*.githubusercontent.com"
            }
          />
          <p className="hint">
            One per line. <code>*.example.com</code> includes subdomains; add{" "}
            <code>:port</code> for ports other than 80 and 443.
          </p>
        </div>
      )}
    </fieldset>
  );
}
