import {
  useCallback,
  useEffect,
  useId,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import { KeyStorage } from "./KeyStorage";
import { RefreshCw } from "lucide-react";
import {
  api,
  type AgentInstall,
  type LoginProgress,
  type OpenRouterStatus,
  type VendorStatus,
} from "../../api";
import { Dialog } from "../Dialog";
import { listen } from "../../lib/transport";
import {
  billingWarning,
  relativeTime,
  usageDetailLines,
  usageLabel,
} from "../../lib/picker";
import { SpendingLimits } from "./SpendingLimits";

const ORDER = ["codex", "claude", "cursor", "antigravity", "grok"];

export const vendorId = (key: string) => key.replace(/^cli[-:]/, "");

type Login = {
  vendor: string;
  attempt: number;
  state:
    "running" | "stopping" | "done" | "unconfirmed" | "failed" | "unsupported";
  lines: string[];
  detail?: string;
};

/** How often a running Antigravity agent install is checked. */
export const INSTALL_POLL_MS = 1000;

const MB = 1_000_000;
/** "334 MB" */
export const formatMb = (bytes: number) =>
  `${Math.round(Math.max(0, bytes) / MB)} MB`;
/** "1.1 GB", or MB below a gigabyte. */
export const formatSize = (bytes: number) =>
  bytes >= 1000 * MB
    ? `${(bytes / (1000 * MB)).toFixed(1)} GB`
    : formatMb(bytes);
/** "dl.google.com" from the download URL. */
export function sourceHost(source: string): string {
  try {
    return new URL(source).host || source;
  } catch {
    return source;
  }
}

/** Settings › Accounts: the official CLI of each subscription, its sign-in
 * state, models and usage. Sign-in and sign-out run the vendors' own commands.
 * Antigravity's agent server is downloaded here, only when the user chooses
 * Install. API keys (OpenRouter, billed per token) follow in their own card. */
export function AccountsPage({
  focusVendor,
  offline = false,
  installPollMs = INSTALL_POLL_MS,
  onChanged,
  onToast,
  planLimit,
}: {
  focusVendor?: string;
  /** "When a plan runs out" (saved as config limits), shown first. */
  planLimit?: ReactNode;
  /** Offline mode: no downloads (the Antigravity agent install is off). */
  offline?: boolean;
  /** Poll interval while the Antigravity agent installs (tests shorten it). */
  installPollMs?: number;
  /** Called after connect/disconnect/refresh so the picker reloads. */
  onChanged: () => void;
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const [vendors, setVendors] = useState<Record<string, VendorStatus> | null>(
    null,
  );
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [busy, setBusy] = useState<Record<string, boolean>>({});
  const [logins, setLogins] = useState<Record<string, Login>>({});
  const loginSequence = useRef<Record<string, number>>({});
  const connecting = useRef(new Set<string>());
  const [connectingRevision, setConnectingRevision] = useState(0);
  const catalogRefreshPending = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);
  const [confirm, setConfirm] = useState<VendorStatus | null>(null);
  const [agentConfirm, setAgentConfirm] = useState<"install" | "remove" | null>(
    null,
  );
  const [checks, setChecks] = useState(0);
  const rows = useRef<Record<string, HTMLElement | null>>({});
  // Which card's main control gets focus once it is on screen: the vendor the
  // picker sent the user to (always), or a card whose controls were replaced
  // (only when focus was on that card or lost).
  const focusRequest = useRef<{ key: string; always: boolean } | null>(
    focusVendor ? { key: focusVendor, always: true } : null,
  );
  const finishing = useRef(false);

  const accountRead = useRef(0);
  const load = useCallback(async (refresh = false, cached = false) => {
    const read = ++accountRead.current;
    setLoading(true);
    setError("");
    try {
      const result = await (cached
        ? api.accountsCached()
        : api.accounts(refresh));
      if (!mounted.current || read !== accountRead.current) return;
      const next: Record<string, VendorStatus> = {};
      for (const [key, value] of Object.entries(result.vendors || {}))
        next[vendorId(key)] = value;
      setVendors(next);
    } catch (e) {
      if (mounted.current && read === accountRead.current) setError(String(e));
    } finally {
      if (mounted.current && read === accountRead.current) setLoading(false);
    }
  }, []);
  useEffect(() => {
    void load(false, true);
  }, [load]);
  useEffect(() => {
    if (focusVendor) focusRequest.current = { key: focusVendor, always: true };
  }, [focusVendor]);
  useEffect(() => {
    const request = focusRequest.current;
    if (!request || !vendors) return;
    const row = rows.current[request.key];
    const target = row?.querySelector<HTMLElement>("[data-primary]");
    if (!row || !target || (target as HTMLButtonElement).disabled) return;
    focusRequest.current = null;
    const active = document.activeElement;
    if (
      !request.always &&
      active &&
      active !== document.body &&
      !row.contains(active)
    )
      return;
    row.scrollIntoView?.({ block: "nearest" });
    target.focus();
  });

  const setInstall = useCallback((install: AgentInstall) => {
    setVendors((current) =>
      current?.antigravity
        ? { ...current, antigravity: { ...current.antigravity, install } }
        : current,
    );
  }, []);
  // The install finished: probe the agent (version, sign-in state) and let
  // the picker reload.
  async function installFinished(install: AgentInstall) {
    if (finishing.current) return;
    finishing.current = true;
    try {
      if (install.installed) {
        focusRequest.current = { key: "antigravity", always: false };
        await refreshOne("antigravity");
        onChanged();
        onToast("Antigravity agent installed. Connect to sign in.", "ok");
      } else if (install.state === "error") {
        focusRequest.current = { key: "antigravity", always: false };
      }
    } finally {
      finishing.current = false;
    }
  }
  const installing = Boolean(vendors?.antigravity?.install?.busy);
  // Follow a running install (also one started before this page opened).
  useEffect(() => {
    if (!installing) return;
    let live = true;
    let reading = false;
    async function read() {
      if (!live || reading) return;
      reading = true;
      try {
        const next = await api.antigravityInstallStatus();
        if (!live) return;
        setInstall(next);
        if (!next.busy) {
          live = false;
          await installFinished(next);
        }
      } catch {
        // A missed check is retried on the next tick.
      } finally {
        reading = false;
      }
    }
    const timer = setInterval(() => void read(), installPollMs);
    return () => {
      live = false;
      clearInterval(timer);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [installing, installPollMs, setInstall]);

  // Recover engine-owned operations after closing Settings or changing tabs.
  // These reads never launch authentication or infer current account billing.
  useEffect(() => {
    let live = true;
    const reads = ORDER.map((vendor) => {
      const attempt = loginSequence.current[vendor] || 0;
      return api
        .loginProgress(vendor)
        .then((progress) => {
          if (
            !live ||
            (loginSequence.current[vendor] || 0) !== attempt ||
            !progress.running ||
            progress.done
          )
            return progress.running === false ? false : null;
          setLogins((current) =>
            current[vendor] || (loginSequence.current[vendor] || 0) !== attempt
              ? current
              : {
                  ...current,
                  [vendor]: {
                    vendor,
                    attempt,
                    state: progress.cancellation_requested
                      ? "stopping"
                      : "running",
                    lines: progress.lines || [],
                  },
                },
          );
          return true;
        })
        .catch(() => null);
    });
    void Promise.all(reads).then((active) => {
      if (
        live &&
        active.every((running) => running === false) &&
        !Object.values(loginSequence.current).some(Boolean)
      )
        void load();
    });
    return () => {
      live = false;
    };
  }, [load]);

  const runningKey = JSON.stringify(
    Object.values(logins)
      .filter(
        (login) => login.state === "running" || login.state === "stopping",
      )
      .map(({ vendor, attempt }) => ({ vendor, attempt })),
  );
  useEffect(() => {
    // A successful vendor may finish while another is still stopping. Its
    // cached card updates immediately, but a full picker refresh must wait
    // rather than start new probes inside the other operation's cleanup.
    if (
      runningKey === "[]" &&
      connecting.current.size === 0 &&
      catalogRefreshPending.current
    ) {
      catalogRefreshPending.current = false;
      onChanged();
    }
  }, [runningKey, onChanged, connectingRevision]);
  // Each vendor keeps its own in-flight read, attempt identity and completion.
  // A slow provider cannot prevent another card's progress or cancellation.
  useEffect(() => {
    const running = JSON.parse(runningKey) as {
      vendor: string;
      attempt: number;
    }[];
    if (!running.length) return;
    let live = true;
    const reading = new Set<string>();
    const finished = new Set<string>();
    async function read(vendor: string, attempt: number) {
      if (!live || reading.has(vendor) || finished.has(vendor)) return;
      reading.add(vendor);
      try {
        const progress: LoginProgress = await api.loginProgress(vendor);
        if (!live || (loginSequence.current[vendor] || 0) !== attempt) return;
        setLogins((current) => {
          const login = current[vendor];
          if (!login || login.attempt !== attempt) return current;
          return {
            ...current,
            [vendor]: {
              ...login,
              lines: progress.lines?.length ? progress.lines : login.lines,
              state: progress.done
                ? progress.done.ok
                  ? progress.done.availability === "ready"
                    ? "done"
                    : "unconfirmed"
                  : "failed"
                : progress.cancellation_requested
                  ? "stopping"
                  : login.state,
              detail: progress.done?.detail || login.detail,
            },
          };
        });
        if (progress.done) {
          finished.add(vendor);
          // The operation owns its final account observation; cancellation
          // must not launch a fresh, slow discovery before retry is possible.
          void load(false, true);
          if (progress.done.ok && progress.done.availability === "ready")
            catalogRefreshPending.current = true;
        }
      } catch {
        // Keep known instructions and try again at the next event/tick.
      } finally {
        reading.delete(vendor);
      }
    }
    const readAll = () => {
      for (const { vendor, attempt } of running) void read(vendor, attempt);
    };
    let stop: (() => void) | undefined;
    void listen("shadowcode:events", readAll)
      .then((unlisten) => {
        if (live) stop = unlisten;
        else unlisten();
      })
      .catch(() => undefined);
    const timer = setInterval(readAll, 1500);
    readAll();
    return () => {
      live = false;
      clearInterval(timer);
      stop?.();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [runningKey]);

  async function refreshOne(vendor: string) {
    setBusy((b) => ({ ...b, [vendor]: true }));
    try {
      const status = await api.refreshAccount(vendor);
      setVendors((current) => ({
        ...(current || {}),
        // Keep the install state if a refresh answer leaves it out.
        [vendor]: {
          ...status,
          install: status.install ?? current?.[vendor]?.install,
        },
      }));
    } catch (e) {
      onToast(String(e), "err");
    } finally {
      setBusy((b) => ({ ...b, [vendor]: false }));
    }
  }

  async function connect(vendor: string) {
    if (connecting.current.has(vendor)) return;
    connecting.current.add(vendor);
    setConnectingRevision((value) => value + 1);
    const attempt = (loginSequence.current[vendor] || 0) + 1;
    loginSequence.current[vendor] = attempt;
    setBusy((b) => ({ ...b, [vendor]: true }));
    try {
      const result = await api.connectAccount(vendor);
      if (!mounted.current || attempt !== loginSequence.current[vendor]) return;
      setLogins((current) => ({
        ...current,
        [vendor]: {
          vendor,
          attempt,
          state: result.state === "unsupported" ? "unsupported" : "running",
          lines: result.lines || [],
          detail:
            result.state === "unsupported"
              ? result.hint ||
                result.note ||
                "Sign in with the vendor's own app."
              : result.note,
        },
      }));
    } catch (e) {
      if (mounted.current && attempt === loginSequence.current[vendor])
        onToast(String(e), "err");
    } finally {
      connecting.current.delete(vendor);
      if (mounted.current) {
        setConnectingRevision((value) => value + 1);
        setBusy((b) => ({ ...b, [vendor]: false }));
      }
    }
  }

  async function cancelLogin(vendor: string) {
    const login = logins[vendor];
    if (!login || login.state !== "running") return;
    const attempt = login.attempt;
    setLogins((current) =>
      current[vendor]?.attempt === attempt
        ? { ...current, [vendor]: { ...current[vendor], state: "stopping" } }
        : current,
    );
    try {
      await api.cancelLogin(vendor);
      // This acknowledges the request only. Read until owned work stops.
    } catch (e) {
      if (!mounted.current) return;
      setLogins((current) =>
        current[vendor]?.attempt === attempt &&
        current[vendor].state === "stopping"
          ? { ...current, [vendor]: { ...current[vendor], state: "running" } }
          : current,
      );
      if ((loginSequence.current[vendor] || 0) === attempt)
        onToast(String(e), "err");
    }
  }

  async function disconnect(status: VendorStatus, vendor: string) {
    setConfirm(null);
    setBusy((b) => ({ ...b, [vendor]: true }));
    try {
      const result = await api.disconnectAccount(vendor);
      onToast(result.note || `Signed out of ${status.label}.`, "ok");
      await refreshOne(vendor);
      onChanged();
    } catch (e) {
      onToast(String(e), "err");
    } finally {
      setBusy((b) => ({ ...b, [vendor]: false }));
    }
  }

  async function installAgent() {
    setAgentConfirm(null);
    setBusy((b) => ({ ...b, antigravity: true }));
    try {
      const next = await api.installAntigravity();
      setInstall(next);
      if (!next.busy) await installFinished(next);
    } catch (e) {
      onToast(message(e), "err");
    } finally {
      setBusy((b) => ({ ...b, antigravity: false }));
    }
  }

  async function removeAgent() {
    setAgentConfirm(null);
    setBusy((b) => ({ ...b, antigravity: true }));
    try {
      setInstall(await api.removeAntigravity());
      focusRequest.current = { key: "antigravity", always: false };
      onToast("Antigravity agent removed. Your sign-in is kept.", "ok");
      await refreshOne("antigravity");
      onChanged();
    } catch (e) {
      onToast(message(e), "err");
    } finally {
      setBusy((b) => ({ ...b, antigravity: false }));
    }
  }

  const agentInstall = vendors?.antigravity?.install;
  const keys = vendors
    ? [
        ...ORDER.filter((k) => vendors[k]),
        ...Object.keys(vendors).filter((k) => !ORDER.includes(k)),
      ]
    : [];
  return (
    <section className="settings-page">
      <div className="settings-page-head">
        <div>
          <h3>Accounts</h3>
          <p className="hint">
            Subscriptions run through each vendor's official command-line tool
            or agent server. Signing in opens the vendor's own page; ShadowCode
            never asks for a password and never reads vendor tokens. API keys
            are billed per token by the provider and stay on this computer.
          </p>
        </div>
        <button
          type="button"
          className="ghost"
          disabled={loading}
          onClick={() => {
            setChecks((n) => n + 1);
            void load(true).then(onChanged);
          }}
        >
          <RefreshCw size={14} aria-hidden="true" />{" "}
          {loading ? "Checking…" : "Check all"}
        </button>
      </div>
      {planLimit && <div className="plan-limit-card">{planLimit}</div>}
      {error && (
        <p className="health-bad" role="alert">
          {error}
        </p>
      )}
      {!vendors && !error && <p role="status">Checking accounts…</p>}
      {keys.map((key) => {
        const status = vendors![key];
        const ready = status.state === "ready";
        const apiKey =
          status.billing == null
            ? /api.?key/i.test(status.account?.auth_mode || "")
            : status.billing === "api_key";
        // The account line already names the plan; don't repeat it below.
        const usageLines = usageDetailLines(status.usage).filter(
          (line) => !(status.account?.plan && / plan$/.test(line)),
        );
        const models = status.models || [];
        const thisLogin = logins[key];
        const loginActive =
          thisLogin?.state === "running" || thisLogin?.state === "stopping";
        // Antigravity before its agent server is installed: the install
        // controls replace the usual status and sign-in.
        const install = key === "antigravity" ? status.install : null;
        const needsInstall = Boolean(install && !install.installed);
        return (
          <article
            className="account-card"
            key={key}
            ref={(el) => {
              rows.current[key] = el;
            }}
            aria-labelledby={`account-${key}`}
          >
            <header>
              <h4 id={`account-${key}`}>{status.product || status.label}</h4>
              <span className={`avail avail-${status.availability}`}>
                {status.availability_label}
              </span>
              {status.version && <span className="dim">{status.version}</span>}
            </header>
            {install && needsInstall && (
              <AgentInstallPanel
                install={install}
                offline={offline}
                starting={Boolean(busy[key])}
                onInstall={() => setAgentConfirm("install")}
                onRetry={() => void installAgent()}
                onRefresh={() => void refreshOne(key).then(onChanged)}
              />
            )}
            {/* Ready rows: the header already shows state and version; the
                account line says who is signed in. Other states explain why. */}
            {!needsInstall && !ready && status.detail && <p>{status.detail}</p>}
            {ready &&
              status.account &&
              (status.account.email || status.account.plan) && (
                <p>
                  {[
                    status.account.email,
                    status.account.plan &&
                      `${status.account.plan[0].toUpperCase()}${status.account.plan.slice(1)} plan`,
                  ]
                    .filter(Boolean)
                    .join(" · ")}
                </p>
              )}
            {apiKey && (
              <p className="warn-text">API key login · billed per token</p>
            )}
            {status.billing === "unknown" && (
              <p className="warn-text">{billingWarning(status)}</p>
            )}
            {!needsInstall && !ready && status.fix && (
              <p className="hint">{status.fix}</p>
            )}
            {!needsInstall && status.error && (
              <p className="health-bad">{status.error}</p>
            )}
            {ready && (
              <div className="account-usage">
                {/* With reported windows the list below says it all; the
                    one-line summary is for rows without them. */}
                <p>
                  <strong>Usage</strong>
                  {status.usage?.windows?.length
                    ? ""
                    : // The usage link sits right below; drop the label's
                      // "· Open … usage" pointer to it.
                      `: ${usageLabel(status.usage, "cloud")
                        .replace(/ · Open .* usage$/, "")
                        .replace(/^Usage unavailable/, "unavailable")}`}
                </p>
                {usageLines.length > 0 && (
                  <ul>
                    {usageLines.map((line) => (
                      <li key={line}>{line}</li>
                    ))}
                  </ul>
                )}
                {status.usage_note &&
                  !usageLines.includes(status.usage_note) && (
                    <p className="hint">{status.usage_note}</p>
                  )}
                {status.usage?.provider_usage_url && (
                  <a
                    href={status.usage.provider_usage_url}
                    target="_blank"
                    rel="noreferrer"
                  >
                    Open {status.product || status.label} usage
                  </a>
                )}
              </div>
            )}
            {ready && models.length > 0 && (
              <details className="account-models">
                <summary>
                  {models.length} model{models.length === 1 ? "" : "s"}{" "}
                  available
                </summary>
                <ul>
                  {models.map((m) => (
                    <li key={m.id}>
                      {m.label || m.id}
                      {m.is_default && <span className="dim"> · default</span>}
                      {m.vision && <span className="dim"> · vision</span>}
                    </li>
                  ))}
                </ul>
              </details>
            )}
            {thisLogin && (
              <LoginPanel
                login={thisLogin}
                onCancel={() => void cancelLogin(key)}
              />
            )}
            {!needsInstall && (
              <div className="row">
                {!ready && status.state !== "not_installed" && (
                  <button
                    type="button"
                    className="primary"
                    data-primary
                    disabled={busy[key] || loginActive}
                    onClick={() => void connect(key)}
                  >
                    {thisLogin?.state === "stopping"
                      ? "Stopping…"
                      : loginActive
                        ? "Signing in…"
                        : "Connect"}
                  </button>
                )}
                <button
                  type="button"
                  className="ghost"
                  data-primary={
                    ready || status.state === "not_installed" ? true : undefined
                  }
                  disabled={busy[key]}
                  onClick={() => void refreshOne(key).then(onChanged)}
                >
                  {busy[key] ? "Checking…" : "Refresh"}
                </button>
                {ready && (
                  <button
                    type="button"
                    className="ghost danger-text"
                    disabled={busy[key]}
                    onClick={() => setConfirm(status)}
                  >
                    Disconnect
                  </button>
                )}
                {install?.installed && install.managed && (
                  <button
                    type="button"
                    className="ghost mini"
                    disabled={busy[key] || thisLogin?.state === "running"}
                    onClick={() => setAgentConfirm("remove")}
                  >
                    Remove agent
                  </button>
                )}
              </div>
            )}
          </article>
        );
      })}
      <OpenRouterCard
        focus={focusVendor === "openrouter"}
        checks={checks}
        onChanged={onChanged}
        onToast={onToast}
      />
      <SpendingLimits onToast={onToast} />
      {confirm && (
        <Dialog
          label={`Disconnect ${confirm.label}`}
          className="modal modal-sm"
          onClose={() => setConfirm(null)}
        >
          <h2>Disconnect {confirm.label}?</h2>
          <p>
            {confirm.shared_cli_note ||
              `This signs out the ${confirm.binary || confirm.label} command-line tool for your whole user account, not just ShadowCode.`}
          </p>
          {confirm.logout_command?.length ? (
            <p className="hint">
              Runs <code>{confirm.logout_command.join(" ")}</code>
            </p>
          ) : null}
          <div className="row end">
            <button
              type="button"
              className="ghost"
              onClick={() => setConfirm(null)}
            >
              Cancel
            </button>
            <button
              type="button"
              className="primary danger"
              onClick={() => void disconnect(confirm, vendorId(confirm.id))}
            >
              Disconnect
            </button>
          </div>
        </Dialog>
      )}
      {agentConfirm === "install" && agentInstall && (
        <Dialog
          label="Install the Antigravity agent"
          className="modal modal-sm"
          onClose={() => setAgentConfirm(null)}
        >
          <h2>Install the Antigravity agent?</h2>
          <p>
            ShadowCode downloads Google's official Antigravity agent server
            (version {agentInstall.version},{" "}
            {formatMb(agentInstall.download_bytes)}) from{" "}
            <strong>{sourceHost(agentInstall.source)}</strong>, checks it and
            unpacks it (about {formatSize(agentInstall.installed_bytes)}) into:
          </p>
          <p>
            <code className="install-dir">{agentInstall.dir}</code>
          </p>
          <p className="hint">
            Nothing is downloaded until you choose Install. You sign in with
            Google afterwards with Connect.
          </p>
          <div className="row end">
            <button
              type="button"
              className="ghost"
              onClick={() => setAgentConfirm(null)}
            >
              Cancel
            </button>
            <button
              type="button"
              className="primary"
              disabled={offline}
              onClick={() => void installAgent()}
            >
              Install
            </button>
          </div>
        </Dialog>
      )}
      {agentConfirm === "remove" && agentInstall && (
        <Dialog
          label="Remove the Antigravity agent"
          className="modal modal-sm"
          onClose={() => setAgentConfirm(null)}
        >
          <h2>Remove the Antigravity agent?</h2>
          <p>
            Frees about {formatSize(agentInstall.installed_bytes)}. Your
            Antigravity sign-in is kept.
          </p>
          <p className="hint">
            Antigravity stays unavailable until you install the agent again.
          </p>
          <div className="row end">
            <button
              type="button"
              className="ghost"
              onClick={() => setAgentConfirm(null)}
            >
              Cancel
            </button>
            <button
              type="button"
              className="primary danger"
              onClick={() => void removeAgent()}
            >
              Remove agent
            </button>
          </div>
        </Dialog>
      )}
      <KeyStorage />
    </section>
  );
}

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** "$0.0042", "$1.20": small per-token spend stays visible. */
export function formatUsd(value: number): string {
  if (!Number.isFinite(value)) return "$0.00";
  const abs = Math.abs(value);
  return `$${abs > 0 && abs < 0.01 ? value.toFixed(4) : value.toFixed(2)}`;
}

/** Accounts › OpenRouter: a pay-per-token API key saved on this computer.
 * The key is write-only here; the engine never sends it back. */
function OpenRouterCard({
  focus,
  checks,
  onChanged,
  onToast,
}: {
  /** Opened from the picker (connect("openrouter")): scroll here and focus. */
  focus: boolean;
  /** Bumped by "Check all" to reread the status. */
  checks: number;
  onChanged: () => void;
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const [status, setStatus] = useState<OpenRouterStatus | null>(null);
  const [loadError, setLoadError] = useState("");
  const [key, setKey] = useState("");
  const [error, setError] = useState("");
  const [busy, setBusy] = useState<"" | "save" | "refresh" | "remove">("");
  const [confirm, setConfirm] = useState(false);
  const card = useRef<HTMLElement>(null);
  const focusAfter = useRef(focus);
  const uid = useId().replace(/:/g, "");

  useEffect(() => {
    let live = true;
    api
      .openrouterStatus()
      .then((next) => {
        if (!live) return;
        setStatus(next);
        setLoadError("");
      })
      .catch((e) => live && setLoadError(message(e)));
    return () => {
      live = false;
    };
  }, [checks]);

  useEffect(() => {
    if (focus) focusAfter.current = true;
  }, [focus]);
  // Move focus to the card's main control once it is on screen: after the
  // picker sent the user here, and after save/remove replaced the controls.
  useEffect(() => {
    if (!status || !focusAfter.current) return;
    focusAfter.current = false;
    card.current?.scrollIntoView?.({ block: "nearest" });
    card.current?.querySelector<HTMLElement>("[data-primary]")?.focus();
  }, [status]);

  async function save(event: FormEvent) {
    event.preventDefault();
    if (!key.trim() || busy) return;
    setBusy("save");
    setError("");
    try {
      const next = await api.setOpenrouterKey(key);
      setKey("");
      focusAfter.current = true;
      setStatus(next);
      onToast("OpenRouter key saved.", "ok");
      onChanged();
    } catch (e) {
      setError(message(e));
    } finally {
      setBusy("");
    }
  }

  async function refresh() {
    setBusy("refresh");
    try {
      setStatus(await api.refreshOpenrouter());
      onChanged();
    } catch (e) {
      onToast(message(e), "err");
    } finally {
      setBusy("");
    }
  }

  async function remove() {
    setConfirm(false);
    setBusy("remove");
    try {
      const next = await api.removeOpenrouterKey();
      focusAfter.current = true;
      setStatus(next);
      onToast("OpenRouter key removed from this computer.", "ok");
      onChanged();
    } catch (e) {
      onToast(message(e), "err");
    } finally {
      setBusy("");
    }
  }

  const offline = Boolean(status?.offline);
  const keysUrl = status?.keys_url || "https://openrouter.ai/keys";
  const info = status?.key;
  const [availability, availabilityText] = !status
    ? ["", ""]
    : offline
      ? ["unavailable", "Offline"]
      : !status.key_set
        ? ["sign_in", "Add API key"]
        : status.key_error
          ? ["sign_in", "Check key"]
          : ["ready", "Ready"];
  return (
    <article
      className="account-card"
      ref={card}
      aria-labelledby={`${uid}-title`}
    >
      <header>
        <h4 id={`${uid}-title`}>OpenRouter</h4>
        <span className="cap-badge">API key</span>
        {availabilityText && (
          <span className={`avail avail-${availability}`}>
            {availabilityText}
          </span>
        )}
      </header>
      {loadError && <p className="health-bad">{loadError}</p>}
      {!status && !loadError && <p role="status">Checking OpenRouter…</p>}
      {offline && <p className="warn-text">Offline mode: OpenRouter is off.</p>}
      {status && !status.key_set && (
        <form className="openrouter-form" onSubmit={(e) => void save(e)}>
          <p>
            Pay per token for hundreds of models. Create a key at{" "}
            <a href={keysUrl} target="_blank" rel="noreferrer">
              openrouter.ai/keys
            </a>
            ; ShadowCode stores it only on this computer.
          </p>
          <div className="field">
            <label htmlFor={`${uid}-key`}>OpenRouter API key</label>
            <input
              id={`${uid}-key`}
              type="password"
              autoComplete="off"
              spellCheck={false}
              data-primary
              value={key}
              disabled={offline || busy === "save"}
              aria-invalid={error ? true : undefined}
              aria-describedby={error ? `${uid}-error` : undefined}
              placeholder="sk-or-…"
              onChange={(e) => {
                setKey(e.target.value);
                if (error) setError("");
              }}
            />
          </div>
          {error && (
            <p className="health-bad" id={`${uid}-error`} role="alert">
              {error}
            </p>
          )}
          <div className="row">
            <button
              type="submit"
              className="primary"
              disabled={offline || !key.trim() || Boolean(busy)}
            >
              {busy === "save" ? "Checking key…" : "Save"}
            </button>
          </div>
        </form>
      )}
      {status?.key_set && (
        <>
          <p className="warn-text">Billed per token by OpenRouter</p>
          {info && (
            <>
              <p>Key: {info.label}</p>
              <p>
                Credits used: {formatUsd(info.usage)}
                {info.limit != null &&
                  ` of ${formatUsd(info.limit)} limit${
                    info.limit_remaining != null
                      ? ` (${formatUsd(info.limit_remaining)} left)`
                      : ""
                  }`}
              </p>
              {info.credits_remaining != null &&
                (info.credits_remaining > 0 ? (
                  <p>Account balance: {formatUsd(info.credits_remaining)}</p>
                ) : (
                  <p className="health-bad">
                    Out of credits: add credits on OpenRouter to use paid
                    models.
                  </p>
                ))}
              {info.is_free_tier && (
                <p className="hint">
                  Free tier: add credits on OpenRouter to use paid models.
                </p>
              )}
            </>
          )}
          {status.key_error && <p className="health-bad">{status.key_error}</p>}
          <p>
            {status.models} model{status.models === 1 ? "" : "s"} (
            {status.tool_models} support tools)
            {status.fetched_at ? (
              <span className="dim">
                {" "}
                · updated {relativeTime(status.fetched_at)}
              </span>
            ) : null}
          </p>
          {status.activity_url && (
            <a href={status.activity_url} target="_blank" rel="noreferrer">
              Open OpenRouter activity
            </a>
          )}
          <div className="row">
            <button
              type="button"
              className="ghost"
              data-primary
              disabled={offline || Boolean(busy)}
              onClick={() => void refresh()}
            >
              {busy === "refresh" ? "Refreshing…" : "Refresh models"}
            </button>
            <button
              type="button"
              className="ghost danger-text"
              disabled={offline || Boolean(busy)}
              onClick={() => setConfirm(true)}
            >
              Remove key
            </button>
          </div>
        </>
      )}
      {confirm && (
        <Dialog
          label="Remove OpenRouter key"
          className="modal modal-sm"
          onClose={() => setConfirm(false)}
        >
          <h2>Remove OpenRouter key?</h2>
          <p>
            ShadowCode deletes the key from this computer and OpenRouter models
            stop working until you add a key again. The key stays valid on
            OpenRouter; revoke it there if you no longer need it.
          </p>
          <div className="row end">
            <button
              type="button"
              className="ghost"
              onClick={() => setConfirm(false)}
            >
              Cancel
            </button>
            <button
              type="button"
              className="primary danger"
              onClick={() => void remove()}
            >
              Remove key
            </button>
          </div>
        </Dialog>
      )}
    </article>
  );
}

const STAGES: Record<string, string> = {
  verifying: "Checking the download…",
  unpacking: "Unpacking…",
};
/** Spoken once per stage (the visible byte counts change every second). */
const ANNOUNCE: Record<string, string> = {
  downloading: "Antigravity agent: downloading",
  verifying: "Antigravity agent: checking the download",
  unpacking: "Antigravity agent: unpacking",
};

/** Accounts › Antigravity before the agent server is installed: what it is,
 * Install (after a confirmation with the download size), progress, errors. */
function AgentInstallPanel({
  install,
  offline,
  starting,
  onInstall,
  onRetry,
  onRefresh,
}: {
  install: AgentInstall;
  offline: boolean;
  /** An install or refresh request is on its way. */
  starting: boolean;
  onInstall: () => void;
  onRetry: () => void;
  onRefresh: () => void;
}) {
  const uid = useId().replace(/:/g, "");
  if (install.busy) {
    const total = install.total || install.download_bytes;
    const downloading = install.state === "downloading";
    const percent = downloading
      ? Math.min(100, Math.round((install.done / Math.max(1, total)) * 100))
      : 100;
    const text = downloading
      ? `Downloading ${formatMb(install.done)} of ${formatMb(total)}`
      : STAGES[install.state] || "Installing…";
    return (
      <div className="agent-install">
        <p>{text}</p>
        <div
          className="bar install-bar"
          role="progressbar"
          aria-label="Installing the Antigravity agent"
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={percent}
          aria-valuetext={text}
        >
          <i style={{ width: `${percent}%` }} />
        </div>
        {/* Stage changes are announced; byte counts are not. */}
        <p className="sr-only" role="status">
          {ANNOUNCE[install.state] || "Installing the Antigravity agent"}
        </p>
      </div>
    );
  }
  const failed = install.state === "error";
  return (
    <div className="agent-install">
      <p>
        Antigravity runs through Google's official agent server, which asks
        ShadowCode before it runs commands or edits files.
      </p>
      {failed ? (
        <p className="health-bad" role="alert">
          {install.error || "The install did not finish."}
        </p>
      ) : (
        <p className="hint">
          {formatMb(install.download_bytes)} download from{" "}
          {sourceHost(install.source)} · about{" "}
          {formatSize(install.installed_bytes)} on disk
        </p>
      )}
      {offline && (
        <p className="warn-text" id={`${uid}-offline`}>
          Offline mode: downloads are off. Switch to Online in Permissions &amp;
          network to install.
        </p>
      )}
      <div className="row">
        <button
          type="button"
          className="primary"
          data-primary
          disabled={offline || starting}
          aria-describedby={offline ? `${uid}-offline` : undefined}
          onClick={failed ? onRetry : onInstall}
        >
          {failed ? "Try again" : "Install Antigravity agent"}
        </button>
        <button
          type="button"
          className="ghost"
          disabled={starting}
          onClick={onRefresh}
        >
          Refresh
        </button>
      </div>
    </div>
  );
}

const URL_RE = /(https?:\/\/[^\s<>"')]+)/g;
const CODE_RE = /\b([A-Z0-9]{4,5}-[A-Z0-9]{4,5})\b/g;

/** A login output line with links and selectable device codes. */
export function LoginLine({ line }: { line: string }) {
  const parts = line.split(URL_RE);
  return (
    <>
      {parts.map((part, i) =>
        i % 2 === 1 ? (
          <a key={i} href={part} target="_blank" rel="noreferrer">
            {part}
          </a>
        ) : (
          part.split(CODE_RE).map((piece, j) =>
            j % 2 === 1 ? (
              <code key={`${i}-${j}`} className="device-code">
                {piece}
              </code>
            ) : (
              <span key={`${i}-${j}`}>{piece}</span>
            ),
          )
        ),
      )}
    </>
  );
}

function LoginPanel({
  login,
  onCancel,
}: {
  login: Login;
  onCancel: () => void;
}) {
  return (
    <div className="login-panel" role="status" aria-live="polite">
      {login.state === "running" && (
        <p>
          Finish signing in on the page the vendor opened. Links and codes
          appear here as the command prints them.
        </p>
      )}
      {login.state === "done" && <p className="health-ok">Signed in.</p>}
      {login.state === "stopping" && <p>Stopping sign-in…</p>}
      {login.state === "unconfirmed" && (
        <p>Sign-in command finished. Account status is not confirmed.</p>
      )}
      {login.state === "failed" && (
        <p className="health-bad">
          {login.detail || "Sign-in did not finish."}
        </p>
      )}
      {login.state === "unsupported" && <p>{login.detail}</p>}
      {login.lines.length > 0 && (
        <pre className="login-lines">
          {login.lines.map((line, i) => (
            <div key={i}>
              <LoginLine line={line} />
            </div>
          ))}
        </pre>
      )}
      {(login.state === "running" || login.state === "stopping") && (
        <button
          type="button"
          className="mini ghost"
          disabled={login.state === "stopping"}
          onClick={onCancel}
        >
          Cancel sign-in
        </button>
      )}
    </div>
  );
}
