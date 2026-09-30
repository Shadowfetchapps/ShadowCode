import { useCallback, useEffect, useState } from "react";
import {
  api,
  type NtfyChange,
  type RemotePairing,
  type RemoteStatus,
} from "../../api";
import { relativeTime } from "../../lib/picker";
import { isRemote } from "../../lib/transport";
import { QrCode } from "../QrCode";

type Toast = (text: string, kind: "ok" | "err" | "info") => void;

const KIND_LABEL: Record<string, string> = {
  loopback: "This computer only",
  tailscale: "Tailscale",
  lan: "Local network",
};

function addressLabel(address: string, kind?: string) {
  if (address === "0.0.0.0" || address === "::")
    return `Every network (${address})`;
  return `${KIND_LABEL[kind || ""] || "Address"} (${address})`;
}

/** A hard-to-guess ntfy topic, made in this browser. */
export function randomTopic() {
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  return `shadowcode-${Array.from(bytes, (b) => b.toString(36).padStart(2, "0"))
    .join("")
    .slice(0, 20)}`;
}

/** Settings › Remote access: the web interface for phones and other
 * computers, paired devices, and phone notifications through ntfy. */
export function RemotePage({ onToast }: { onToast: Toast }) {
  const [status, setStatus] = useState<RemoteStatus | null>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState("");
  const [pairing, setPairing] = useState<RemotePairing | null>(null);
  const [linkHost, setLinkHost] = useState("");
  const [address, setAddress] = useState("");
  const [port, setPort] = useState("");
  const [publicUrl, setPublicUrl] = useState("");
  const [server, setServer] = useState("");
  const [topic, setTopic] = useState("");
  const [token, setToken] = useState("");

  const apply = useCallback((next: RemoteStatus) => {
    setStatus(next);
    setAddress(next.address);
    setPort(String(next.port));
    setPublicUrl(next.public_url);
    setServer(next.ntfy.server);
    setTopic(next.ntfy.topic);
    setError("");
  }, []);

  const load = useCallback(async () => {
    try {
      apply(await api.remoteStatus());
    } catch (e) {
      setError(String(e));
    }
  }, [apply]);
  useEffect(() => {
    if (!isRemote()) void load();
  }, [load]);

  async function run<T>(
    key: string,
    action: () => Promise<T>,
    done = "",
  ): Promise<T | undefined> {
    setPending(key);
    try {
      const result = await action();
      if (done) onToast(done, "ok");
      return result;
    } catch (e) {
      onToast(String(e), "err");
      return undefined;
    } finally {
      setPending("");
    }
  }

  const save = (values: Parameters<typeof api.saveRemote>[0], done = "") =>
    run(
      "remote",
      async () => {
        const next = await api.saveRemote(values);
        apply(next);
        if (!next.running) setPairing(null);
        if (next.error) onToast(next.error, "err");
      },
      done,
    );
  const saveNtfy = (values: NtfyChange, done = "") =>
    run("ntfy", async () => apply(await api.saveNtfy(values)), done);

  if (isRemote()) {
    return (
      <section className="settings-page remote-settings">
        <h3>Remote access</h3>
        <p className="hint">
          You are using ShadowCode from another device. Remote access, paired
          devices and phone notifications are managed on the computer running
          ShadowCode.
        </p>
      </section>
    );
  }
  if (!status) {
    return (
      <section className="settings-page remote-settings">
        <h3>Remote access</h3>
        {error ? (
          <p className="health-bad" role="alert">
            {error}
          </p>
        ) : (
          <p role="status">Reading remote access settings…</p>
        )}
      </section>
    );
  }

  const addresses = status.addresses;
  const choices = [
    ...addresses,
    { address: "0.0.0.0", interface: "", kind: "all" },
  ];
  if (!choices.some((a) => a.address === status.address))
    choices.unshift({ address: status.address, interface: "", kind: "" });
  const everyAddress = status.address === "0.0.0.0" || status.address === "::";
  const linkHosts = addresses.filter((a) => a.kind !== "loopback");
  const https = status.public_url.startsWith("https://");
  // Tailscale traffic is already encrypted between your devices.
  const tailnet =
    addresses.find((a) => a.address === status.address)?.kind === "tailscale";
  const busy = Boolean(pending);
  const addressChanged =
    address !== status.address || port !== String(status.port);

  return (
    <section className="settings-page remote-settings">
      <h3>Remote access</h3>
      <p className="hint">
        Follow and steer tasks from your phone or another computer in a web
        browser: read conversations, answer approvals and send messages. Only
        devices you pair can connect.
      </p>
      <label className="check">
        <input
          type="checkbox"
          checked={status.enabled}
          disabled={busy}
          onChange={(e) =>
            void save(
              { enabled: e.target.checked },
              e.target.checked ? "" : "Remote access turned off",
            )
          }
        />{" "}
        Turn on remote access
      </label>
      {status.running && status.url && (
        <p className="health-ok" role="status">
          Listening at <code>{status.url}</code>
        </p>
      )}
      {status.enabled && !status.running && (
        <p className="health-bad" role="alert">
          Remote access is on but not running
          {status.error ? `: ${status.error}` : "."}
        </p>
      )}

      <h4>Where it listens</h4>
      <div className="remote-address">
        <div className="field">
          <label htmlFor="remote-address">Address</label>
          <select
            id="remote-address"
            value={address}
            disabled={busy}
            onChange={(e) => setAddress(e.target.value)}
          >
            {choices.map((choice) => (
              <option key={choice.address} value={choice.address}>
                {addressLabel(choice.address, choice.kind)}
              </option>
            ))}
          </select>
        </div>
        <div className="field remote-port">
          <label htmlFor="remote-port">Port</label>
          <input
            id="remote-port"
            inputMode="numeric"
            value={port}
            disabled={busy}
            onChange={(e) => setPort(e.target.value.replace(/\D/g, ""))}
          />
        </div>
        <button
          type="button"
          className="mini"
          disabled={busy || !addressChanged || !port}
          onClick={() =>
            void save(
              { address, port: Number(port) },
              "Remote access address saved",
            )
          }
        >
          Apply
        </button>
      </div>
      <p className="hint">
        “This computer only” is the safe default: nothing else on the network
        can connect. To reach it from a phone, use Tailscale: choose the
        Tailscale address, or keep this computer only and run{" "}
        <code>tailscale serve --bg {status.port}</code> for an HTTPS address on
        your tailnet.
      </p>
      {status.exposed && !tailnet && !https && (
        <p className="warn-text remote-warning" role="note">
          Plain HTTP is not encrypted. On a local network, other devices can
          read what you see and send, including the access key. Prefer
          Tailscale, which encrypts the connection, and add its HTTPS address
          below.
        </p>
      )}
      <div className="field">
        <label htmlFor="remote-public-url">Public address (optional)</label>
        <div className="input-action">
          <input
            id="remote-public-url"
            value={publicUrl}
            disabled={busy}
            placeholder="https://your-computer.your-tailnet.ts.net"
            onChange={(e) => setPublicUrl(e.target.value)}
          />
          <button
            type="button"
            className="ghost"
            disabled={busy || publicUrl === status.public_url}
            onClick={() =>
              void save({ public_url: publicUrl }, "Public address saved")
            }
          >
            Save
          </button>
        </div>
        <p className="hint">
          Pairing links and phone notifications use this address, for example
          the HTTPS address from <code>tailscale serve</code>.
        </p>
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={status.allow_terminals}
          disabled={busy}
          onChange={(e) => void save({ allow_terminals: e.target.checked })}
        />{" "}
        Allow terminals over remote access
      </label>
      <p className="hint">
        Off by default. When on, paired devices can open terminals and run any
        command on this computer. Tasks still ask for approval as usual.
      </p>

      <h4>Pair a device</h4>
      {!status.running ? (
        <p className="hint">Turn on remote access to pair a phone.</p>
      ) : (
        <>
          {everyAddress && !status.public_url && linkHosts.length > 1 && (
            <div className="field">
              <label htmlFor="remote-link-host">Link address</label>
              <select
                id="remote-link-host"
                value={linkHost}
                onChange={(e) => setLinkHost(e.target.value)}
              >
                <option value="">Automatic</option>
                {linkHosts.map((a) => (
                  <option key={a.address} value={a.address}>
                    {addressLabel(a.address, a.kind)}
                  </option>
                ))}
              </select>
            </div>
          )}
          <div className="row">
            <button
              type="button"
              className="primary"
              disabled={busy}
              onClick={() =>
                void run("pair", async () =>
                  setPairing(await api.pairRemote(linkHost || undefined)),
                )
              }
            >
              {pairing ? "New pairing link" : "Pair a device"}
            </button>
          </div>
          {pairing && (
            <div className="remote-pairing">
              <QrCode rows={pairing.qr.rows} label="Pairing QR code" />
              <div>
                <p>
                  Scan with the phone’s camera, or open this link on the device.
                  It works once, for {Math.round(pairing.expires_in / 60)}{" "}
                  minutes.
                </p>
                <p className="remote-link">
                  <code>{pairing.link}</code>
                </p>
                <button
                  type="button"
                  className="mini ghost"
                  onClick={() =>
                    void navigator.clipboard
                      .writeText(pairing.link)
                      .then(() => onToast("Pairing link copied", "ok"))
                      .catch((e) => onToast(String(e), "err"))
                  }
                >
                  Copy link
                </button>
                <p className="hint">
                  Anyone who opens this link first can control ShadowCode. Share
                  it only with your own device.
                </p>
              </div>
            </div>
          )}
        </>
      )}

      <h4>Paired devices</h4>
      {status.devices.length === 0 ? (
        <p className="hint">No devices are paired.</p>
      ) : (
        <ul className="remote-devices">
          {status.devices.map((device) => (
            <li key={device.id}>
              <span>
                <strong>{device.name}</strong>
                <small className="dim">
                  {device.last_seen
                    ? `Used ${relativeTime(device.last_seen)}`
                    : `Paired ${relativeTime(device.created_at)}`}
                </small>
              </span>
              <button
                type="button"
                className="mini ghost danger-text"
                disabled={busy}
                onClick={() =>
                  void run(
                    `revoke:${device.id}`,
                    async () => apply(await api.revokeRemote(device.id)),
                    `${device.name} unpaired`,
                  )
                }
              >
                Unpair
              </button>
            </li>
          ))}
        </ul>
      )}
      {status.devices.length > 0 && (
        <button
          type="button"
          className="mini ghost danger-text"
          disabled={busy}
          onClick={() =>
            void run(
              "revoke:all",
              async () => {
                apply(await api.revokeRemote());
                setPairing(null);
              },
              "Every device unpaired and unused links cancelled",
            )
          }
        >
          Unpair all devices
        </button>
      )}

      <h4>Phone notifications</h4>
      <p className="hint">
        Get a notification on your phone through{" "}
        <a href="https://ntfy.sh" target="_blank" rel="noreferrer">
          ntfy
        </a>{" "}
        when a task needs approval, finishes, fails or reaches a plan or
        spending limit. Install the ntfy app, subscribe to the same topic, and
        enter the server here (ntfy.sh or your own). Nothing is sent until you
        do.
      </p>
      <div className="field">
        <label htmlFor="ntfy-server">Server</label>
        <input
          id="ntfy-server"
          value={server}
          disabled={busy}
          placeholder="https://ntfy.sh"
          onChange={(e) => setServer(e.target.value)}
        />
      </div>
      <div className="field">
        <label htmlFor="ntfy-topic">Topic</label>
        <div className="input-action">
          <input
            id="ntfy-topic"
            value={topic}
            disabled={busy}
            spellCheck={false}
            onChange={(e) => setTopic(e.target.value)}
          />
          <button
            type="button"
            className="ghost"
            disabled={busy}
            onClick={() => setTopic(randomTopic())}
          >
            Generate
          </button>
        </div>
        <p className="hint">
          On a public server anyone who knows the topic can read its messages,
          so use a long, random one.
        </p>
      </div>
      <div className="field">
        <label htmlFor="ntfy-token">Access token (optional)</label>
        <input
          id="ntfy-token"
          type="password"
          autoComplete="off"
          value={token}
          disabled={busy}
          placeholder={
            status.ntfy.token_saved
              ? "Saved · type to replace"
              : "For servers that need a login"
          }
          onChange={(e) => setToken(e.target.value)}
        />
      </div>
      <div className="row">
        <button
          type="button"
          className="primary"
          disabled={
            busy ||
            (server === status.ntfy.server &&
              topic === status.ntfy.topic &&
              !token)
          }
          onClick={() =>
            void saveNtfy(
              { server, topic, ...(token ? { token } : {}) },
              server && topic
                ? "Phone notifications saved"
                : "Phone notifications turned off",
            ).then(() => setToken(""))
          }
        >
          Save
        </button>
        <button
          type="button"
          className="ghost"
          disabled={busy || !status.ntfy.configured}
          onClick={() =>
            void run(
              "ntfy-test",
              async () => {
                try {
                  await api.testNtfy();
                } finally {
                  await load();
                }
              },
              "Test notification sent",
            )
          }
        >
          Send a test
        </button>
        {status.ntfy.token_saved && (
          <button
            type="button"
            className="ghost danger-text"
            disabled={busy}
            onClick={() => void saveNtfy({ token: "" }, "Access token removed")}
          >
            Remove token
          </button>
        )}
      </div>
      {status.ntfy.error && (
        <p className="health-bad" role="alert">
          Last notification failed: {status.ntfy.error}
        </p>
      )}
      <fieldset className="remote-events" disabled={busy}>
        <legend>Notify me when</legend>
        {(
          [
            ["approval", "A task needs my approval"],
            ["finished", "A task finishes"],
            ["failed", "A task fails"],
            ["limit", "A task reaches a plan or spending limit"],
          ] as const
        ).map(([key, label]) => (
          <label className="check" key={key}>
            <input
              type="checkbox"
              checked={status.ntfy.events[key]}
              onChange={(e) =>
                void saveNtfy({ events: { [key]: e.target.checked } })
              }
            />{" "}
            {label}
          </label>
        ))}
        <label className="check">
          <input
            type="checkbox"
            checked={status.ntfy.details}
            onChange={(e) => void saveNtfy({ details: e.target.checked })}
          />{" "}
          Include task details (the command waiting for approval, or the task’s
          summary)
        </label>
      </fieldset>
      <p className="hint">
        Messages name the project and link back to the conversation when remote
        access is on.
      </p>
    </section>
  );
}
