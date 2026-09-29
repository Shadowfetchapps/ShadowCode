import { useCallback, useEffect, useState } from "react";
import { CircleArrowUp, Copy, FolderOpen, RefreshCw } from "lucide-react";
import { api, type AboutInfo, type UpdateStatus } from "../../api";
import { relativeTime } from "../../lib/picker";
import { canPickFiles, isRemote, openLogsFolder } from "../../lib/transport";
import { announceUpdates } from "../../hooks/useUpdates";
import "../../about.css";
import { LoadError } from "../cards";

type Toast = (text: string, kind: "ok" | "err" | "info") => void;
type Save = (values: Record<string, unknown>) => Promise<void>;

/** One sentence about the update check's state. */
export function updateSummary(
  status: UpdateStatus,
  now = Date.now() / 1000,
): string {
  if (!status.allowed)
    return (
      status.policy_message ||
      "Update checks are turned off for this installation."
    );
  if (status.available && status.latest)
    return `ShadowCode ${status.latest.version} is available. You have ${status.current}.`;
  if (status.offline)
    return "Offline mode is on, so ShadowCode doesn't check for updates.";
  if (status.error) return `The last check didn't work: ${status.error}.`;
  if (status.last_checked_at)
    return `You have the latest version (checked ${relativeTime(status.last_checked_at, now)}).`;
  return status.automatic
    ? "Not checked yet. ShadowCode checks once a day."
    : "Not checked yet.";
}

function publishedDate(text?: string | null) {
  if (!text) return "";
  const date = new Date(text);
  return Number.isNaN(date.getTime())
    ? ""
    : date.toLocaleDateString(undefined, {
        year: "numeric",
        month: "short",
        day: "numeric",
      });
}

/** Settings › About: version, commit, install type, license and the
 * update check. */
export function AboutPage({
  onSave,
  onToast,
}: {
  onSave: Save;
  onToast: Toast;
}) {
  const [about, setAbout] = useState<AboutInfo | null>(null);
  const [updates, setUpdates] = useState<UpdateStatus | null>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState("");

  const load = useCallback(async () => {
    try {
      const next = await api.about();
      setAbout(next);
      setUpdates(next.updates);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  async function run(key: string, action: () => Promise<UpdateStatus>) {
    setPending(key);
    try {
      setUpdates(await action());
      announceUpdates();
    } catch (e) {
      onToast(String(e).replace(/^Error: /, ""), "err");
    } finally {
      setPending("");
    }
  }

  if (!about || !updates)
    return (
      <section className="settings-page about-page">
        <h3>About ShadowCode</h3>
        {error ? (
          <LoadError message={error} onRetry={load} />
        ) : (
          <p role="status">Reading version information…</p>
        )}
      </section>
    );

  const remote = isRemote();
  const release = updates.available ? updates.latest : null;
  const step = updates.next_step;
  const copy = async (text: string) => {
    try {
      await navigator.clipboard.writeText(text);
      onToast("Command copied", "ok");
    } catch {
      onToast("Could not copy to the clipboard.", "err");
    }
  };

  return (
    <section className="settings-page about-page">
      <h3>About ShadowCode</h3>
      <div className="kv about-facts">
        <div>
          <span>Version</span>
          <code>{about.version}</code>
        </div>
        <div>
          <span>Commit</span>
          {about.commit ? (
            <code title={about.commit}>{about.commit.slice(0, 12)}</code>
          ) : (
            <span>Not recorded in this build</span>
          )}
        </div>
        <div>
          <span>Installed as</span>
          <code>{about.install.label}</code>
        </div>
        <div>
          <span>License</span>
          <code>{about.license.name}</code>
        </div>
      </div>
      <p className="hint">
        Copyright 2026 {about.license.holder}. ShadowCode was originally created
        by {about.license.holder}. If you share a copy or a changed version,
        include the NOTICE below with the license.
      </p>
      <details className="about-notice">
        <summary>NOTICE</summary>
        <pre>{about.license.notice}</pre>
      </details>
      {about.license.third_party && (
        <p className="hint">
          Licenses of the components ShadowCode includes are in{" "}
          <code>{about.license.third_party}</code>.
        </p>
      )}
      <nav className="about-links" aria-label="ShadowCode links">
        <a href={about.links.release_notes}>Release notes</a>
        <a href={about.links.repository}>Source code</a>
        <a href={about.links.license}>License</a>
        <a href={about.links.user_guide}>User guide</a>
        <a href={about.links.issues}>Report a problem</a>
      </nav>
      {canPickFiles() && (
        <div className="about-logs">
          <p className="hint">
            ShadowCode keeps a log for bug reports: which tasks ran, errors and
            timings. It never holds your prompts, answers or files, and secrets
            are removed. Attach it when you report a problem.
          </p>
          <button
            type="button"
            className="ghost"
            onClick={() =>
              void openLogsFolder().catch((e) => onToast(String(e), "err"))
            }
          >
            <FolderOpen size={14} aria-hidden="true" /> Open logs folder
          </button>
        </div>
      )}

      <h4>Updates</h4>
      {updates.allowed && (
        <label className="check">
          <input
            type="checkbox"
            checked={updates.automatic}
            disabled={Boolean(pending)}
            onChange={(e) => {
              const check = e.target.checked;
              setPending("setting");
              void onSave({ updates: { check } })
                .then(async () => {
                  setUpdates(await api.updates());
                  announceUpdates();
                })
                .catch((err) => onToast(String(err), "err"))
                .finally(() => setPending(""));
            }}
          />{" "}
          Check for updates once a day
        </label>
      )}
      <p className="hint">
        {updates.allowed
          ? "ShadowCode asks GitHub for the newest release. The request carries no version, account or other identifier, and nothing is downloaded or installed for you."
          : "ShadowCode does not contact GitHub for updates."}
      </p>
      <p
        role="status"
        className={release ? "about-status update" : "about-status"}
      >
        {updateSummary(updates)}
      </p>
      {release && updates.error && (
        <p className="hint">The last check didn't work: {updates.error}.</p>
      )}
      {updates.allowed && !remote && (
        <div className="row">
          <button
            type="button"
            className="mini"
            disabled={Boolean(pending) || updates.offline}
            onClick={() => void run("check", api.checkUpdates)}
          >
            <RefreshCw size={13} aria-hidden="true" />
            {pending === "check" ? "Checking…" : "Check now"}
          </button>
        </div>
      )}
      {release && step && (
        <div
          className="update-card"
          role="region"
          aria-label={`ShadowCode ${release.version}`}
        >
          <div className="update-card-head">
            <CircleArrowUp size={16} aria-hidden="true" />
            <strong>ShadowCode {release.version}</strong>
            {publishedDate(release.published_at) && (
              <small>released {publishedDate(release.published_at)}</small>
            )}
            <a href={release.url}>Release notes</a>
          </div>
          <p>{step.text}</p>
          {step.command && (
            <div className="update-command">
              <pre>
                <code>{step.command}</code>
              </pre>
              <button
                type="button"
                className="icon-btn"
                aria-label="Copy command"
                onClick={() => void copy(step.command || "")}
              >
                <Copy size={14} />
              </button>
            </div>
          )}
          <div className="row">
            {step.link && <a href={step.link}>Install instructions</a>}
            {!updates.dismissed && (
              <button
                type="button"
                className="ghost"
                disabled={Boolean(pending)}
                onClick={() =>
                  void run("dismiss", () => api.dismissUpdate(release.version))
                }
              >
                Hide the notice until the next version
              </button>
            )}
          </div>
        </div>
      )}
    </section>
  );
}
