import { useCallback, useEffect, useState } from "react";
import { Archive, RotateCcw, ShieldCheck, Wrench } from "lucide-react";
import {
  dataApi,
  describeSummary,
  formatBytes,
  hasRemotePairing,
  pickDataFolder,
  type DataOverview,
  type Inspection,
  type LastOperation,
  type RepairReport,
} from "../../lib/data";
import { canPickFiles, invoke, isRemote } from "../../lib/transport";
import { LoadError } from "../cards";
import { ConfirmDialog } from "../ConfirmDialog";
import "../../data.css";

type Toast = (text: string, kind: "ok" | "err" | "info") => void;

const clean = (e: unknown) => String(e).replace(/^(Api)?Error: /, "");

function when(seconds: number) {
  return new Date(seconds * 1000).toLocaleString(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
  });
}

const REASONS: Record<string, string> = {
  manual: "",
  "before-restore": "made before a restore",
  "before-repair": "made before a repair",
  "upgrade-copy": "made before an upgrade",
};

function lastText(last: LastOperation): string {
  const at = when(last.finished_at);
  if (!last.ok && last.kind !== "repair")
    return `The ${last.kind} on ${at} did not finish: ${last.error ?? "unknown error"}. Nothing was changed.`;
  if (last.kind === "restore")
    return `Restored on ${at}. The data you had before is in ${last.backup_of_previous_data}.${last.remote_restored ? " Remote access is off: check the paired devices in Settings › Remote access before turning it on." : ""}`;
  if (last.kind === "reset")
    return `Reset on ${at}. Your previous data was moved to ${(last.moved_to ?? []).join(", ") || "folders next to the profile"}.`;
  return last.ok
    ? `Last check and repair on ${at}: no problems found.`
    : `Last check and repair on ${at} found damage. Restore a backup below.`;
}

/** Text with `command` spans shown as code. */
function withCode(text: string) {
  return text
    .split("`")
    .map((part, i) => (i % 2 ? <code key={i}>{part}</code> : part));
}

/** What to close so a scheduled restore or reset runs. The engine names
 * the process holding the profile; a window attached to it is not enough. */
function restartText(overview: DataOverview): string {
  const mode = overview.engine?.mode;
  if (overview.desktop_attached && mode === "desktop")
    return "Another ShadowCode window holds your data: quit it and this one, then open ShadowCode again.";
  return (
    overview.engine?.restart ??
    "Quit ShadowCode (and any `shadowcode serve`) and open it again."
  );
}

/** Settings › Your data: backups, restore, check and repair, reset, and
 * where the profile lives. Local only: remote access refuses these routes. */
export function DataPage({ onToast }: { onToast: Toast }) {
  const [overview, setOverview] = useState<DataOverview | null>(null);
  const [error, setError] = useState("");
  const [busy, setBusy] = useState("");
  const [includeSecrets, setIncludeSecrets] = useState(false);
  const [restoring, setRestoring] = useState<Inspection | null>(null);
  const [restoreSecrets, setRestoreSecrets] = useState(false);
  const [restoreRemote, setRestoreRemote] = useState(false);
  const [resetting, setResetting] = useState(false);
  const [report, setReport] = useState<RepairReport | null>(null);
  const remote = isRemote();
  const pickers = canPickFiles();

  const load = useCallback(async () => {
    try {
      setOverview(await dataApi.overview());
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    if (!remote) void load();
  }, [load, remote]);

  async function act<T>(key: string, action: () => Promise<T>) {
    setBusy(key);
    try {
      return await action();
    } catch (e) {
      onToast(clean(e), "err");
      return undefined;
    } finally {
      setBusy("");
    }
  }

  async function backUp(folder = "") {
    const made = await act("backup", () =>
      dataApi.createBackup(includeSecrets, folder),
    );
    if (made) {
      const left = made.manifest.left_out ?? [];
      onToast(
        `Backup saved to ${made.path}${left.length ? `. Not included: ${left.join("; ")}` : ""}`,
        "ok",
      );
      await load();
    }
  }

  async function chooseRestore(path: string) {
    const inspection = await act("inspect", () => dataApi.inspect(path));
    if (inspection) {
      setRestoreSecrets(false);
      setRestoreRemote(false);
      setRestoring(inspection);
    }
  }

  if (remote)
    return (
      <section className="settings-page data-page">
        <h3>Your data</h3>
        <p className="hint">
          Backups, restore, repair and reset are available only in Settings ›
          Your data on the computer running ShadowCode.
        </p>
      </section>
    );

  if (!overview)
    return (
      <section className="settings-page data-page">
        <h3>Your data</h3>
        {error ? (
          <LoadError message={error} onRetry={load} />
        ) : (
          <p role="status">Reading your data…</p>
        )}
      </section>
    );

  const pending = overview.pending;
  const db = overview.database;
  const included = [
    pending?.include_secrets && "API keys",
    pending?.include_remote && "remote access",
  ].filter(Boolean);
  // Quitting this window helps only when it holds the profile itself.
  const quitHelps =
    !overview.desktop_attached &&
    (!overview.engine?.mode || overview.engine.mode === "desktop");
  return (
    <section className="settings-page data-page">
      <h3>Your data</h3>
      {pending && (
        <div className="data-pending" role="status">
          <p>
            {pending.kind === "restore"
              ? `A restore from ${pending.source} is scheduled${included.length ? `, ${included.join(" and ")} included` : ""}.`
              : "A reset is scheduled."}{" "}
            It happens the next time ShadowCode starts.{" "}
            {withCode(restartText(overview))}
          </p>
          <div className="row">
            {quitHelps && (
              <button
                type="button"
                className="mini"
                onClick={() => void invoke("desktop_quit")}
              >
                Quit ShadowCode now
              </button>
            )}
            <button
              type="button"
              className="ghost"
              disabled={Boolean(busy)}
              onClick={async () => {
                if (await act("cancel", dataApi.cancelPending)) {
                  onToast(`The ${pending.kind} was cancelled.`, "info");
                  await load();
                }
              }}
            >
              Cancel the {pending.kind}
            </button>
          </div>
        </div>
      )}
      {overview.last_operation && !report && (
        <p
          className={overview.last_operation.ok ? "hint" : "hint data-problem"}
        >
          {lastText(overview.last_operation)}
        </p>
      )}

      <h4>Backups</h4>
      <p className="hint">
        A backup is a folder with a copy of your conversations, tasks, goals,
        automations and settings. Only your account can open it. Downloaded
        models and project files are not included.
      </p>
      <label className="check">
        <input
          type="checkbox"
          checked={includeSecrets}
          onChange={(e) => setIncludeSecrets(e.target.checked)}
        />{" "}
        Include API keys and remote-access pairing
      </label>
      {includeSecrets && (
        <p className="hint data-warning" role="note">
          Anyone who gets this backup can use your API keys and your paired
          devices. Keep it somewhere private, and never share it.
        </p>
      )}
      <div className="row">
        <button
          type="button"
          className="mini"
          disabled={Boolean(busy)}
          onClick={() => void backUp()}
        >
          <Archive size={13} aria-hidden="true" />
          {busy === "backup" ? "Backing up…" : "Back up now"}
        </button>
        {pickers && (
          <button
            type="button"
            className="ghost"
            disabled={Boolean(busy)}
            onClick={async () => {
              const folder = await pickDataFolder("backup");
              if (folder) await backUp(folder);
            }}
          >
            Back up to another folder…
          </button>
        )}
      </div>
      {overview.backups.length ? (
        <ul className="data-list" aria-label="Backups">
          {overview.backups.map((b) => (
            <li key={b.path}>
              <div>
                <strong>{when(b.created_at)}</strong>
                <small>
                  ShadowCode {b.app_version} · {formatBytes(b.bytes)}
                  {REASONS[b.reason] ? ` · ${REASONS[b.reason]}` : ""}
                  {b.includes_secrets ? " · includes API keys" : ""}
                </small>
              </div>
              <button
                type="button"
                className="ghost"
                aria-label={`Restore the backup from ${when(b.created_at)}`}
                disabled={Boolean(busy)}
                onClick={() => void chooseRestore(b.path)}
              >
                <RotateCcw size={13} aria-hidden="true" />
                Restore…
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="hint">No backups in {overview.backups_folder} yet.</p>
      )}
      {pickers && (
        <button
          type="button"
          className="ghost"
          disabled={Boolean(busy)}
          onClick={async () => {
            const folder = await pickDataFolder("restore");
            if (folder) await chooseRestore(folder);
          }}
        >
          Restore from another folder…
        </button>
      )}

      {overview.upgrade_copies.length > 0 && (
        <>
          <h4>Copies made before upgrades</h4>
          <p className="hint">
            Before each upgrade changes the database, ShadowCode keeps a copy of
            it. Restoring one brings back your history as it was then; your
            settings stay as they are.
          </p>
          <ul className="data-list" aria-label="Copies made before upgrades">
            {overview.upgrade_copies.map((c) => (
              <li key={c.path}>
                <div>
                  <strong>{when(c.created_at)}</strong>
                  <small>
                    {c.name} · {formatBytes(c.bytes)}
                  </small>
                </div>
                <button
                  type="button"
                  className="ghost"
                  aria-label={`Restore the copy from ${when(c.created_at)}`}
                  disabled={Boolean(busy)}
                  onClick={() => void chooseRestore(c.path)}
                >
                  <RotateCcw size={13} aria-hidden="true" />
                  Restore…
                </button>
              </li>
            ))}
          </ul>
        </>
      )}

      <h4>Check and repair</h4>
      <p className="hint">
        Backs up the database first, checks it for damage, rebuilds its search
        indexes and clears caches ShadowCode can download again. Nothing you
        made is removed.
      </p>
      <button
        type="button"
        className="mini"
        disabled={Boolean(busy)}
        onClick={async () => {
          const result = await act("repair", dataApi.repair);
          if (result) {
            setReport(result);
            onToast(
              result.ok
                ? "No problems found."
                : "The database is damaged. Restore a backup.",
              result.ok ? "ok" : "err",
            );
            await load();
          }
        }}
      >
        <Wrench size={13} aria-hidden="true" />
        {busy === "repair" ? "Checking…" : "Check and repair"}
      </button>
      {report && (
        <ul className="data-checks" aria-label="Check and repair results">
          {report.checks.map((c) => (
            <li key={c.id} className={`data-check ${c.status}`}>
              <strong>{c.label}</strong>
              <span>{c.detail}</span>
            </li>
          ))}
        </ul>
      )}

      <h4>Start over</h4>
      <p className="hint">
        Reset moves your settings, API keys, conversations and history into
        folders next to the ones below (named <code>….reset-</code> and the
        date), so ShadowCode starts as if it was just installed. Nothing is
        deleted. Backups, worktrees and downloaded models stay where they are.
      </p>
      <button
        type="button"
        className="mini danger-text"
        disabled={Boolean(busy)}
        onClick={() => setResetting(true)}
      >
        Reset ShadowCode…
      </button>
      {overview.reset_folders.length > 0 && (
        <p className="hint">
          Folders from earlier resets: {overview.reset_folders.join(", ")}
        </p>
      )}

      <h4>Where your data is</h4>
      <div className="kv data-folders">
        <div>
          <span>Settings and API keys</span>
          <code>{overview.folders.config}</code>
        </div>
        <div>
          <span>Backups, worktrees and downloads</span>
          <code>{overview.folders.data}</code>
        </div>
        <div>
          <span>History</span>
          <code>{overview.folders.state}</code>
        </div>
        <div>
          <span>Database</span>
          <span>
            {formatBytes(db.bytes + db.wal_bytes)} · format{" "}
            {db.schema_version ?? "unknown"}
          </span>
        </div>
      </div>
      <p className="hint">
        The folders keep the name <code>shadow-agent</code> from earlier
        versions so every upgrade finds your data. The names stay the same in
        all 1.x versions.
      </p>

      {restoring && (
        <ConfirmDialog
          title={
            restoring.restorable
              ? "Restore this backup?"
              : "This backup can't be restored"
          }
          confirmLabel={
            restoring.restorable ? "Restore at next start" : "Close"
          }
          onCancel={() => setRestoring(null)}
          onConfirm={async () => {
            if (!restoring.restorable) {
              setRestoring(null);
              return;
            }
            const scheduled = await act("restore", () =>
              dataApi.restore(restoring.path, restoreSecrets, restoreRemote),
            );
            if (scheduled) {
              setRestoring(null);
              onToast(scheduled.message, "info");
              await load();
            }
          }}
        >
          <p>
            {restoring.kind === "database"
              ? `A copy of the database from ${when(restoring.manifest.created_at)}.`
              : `A backup from ${when(restoring.manifest.created_at)}, made by ShadowCode ${restoring.manifest.app_version}.`}{" "}
            {describeSummary(restoring.summary)}
          </p>
          {restoring.problems.length > 0 && (
            <ul className="data-problems" aria-label="Problems">
              {restoring.problems.map((p) => (
                <li key={p}>{p}</li>
              ))}
            </ul>
          )}
          {restoring.restorable && (
            <>
              <p>
                <ShieldCheck size={13} aria-hidden="true" /> Your current data
                is backed up first. The restore finishes the next time
                ShadowCode starts.
              </p>
              {restoring.manifest.includes_secrets && (
                <label className="check">
                  <input
                    type="checkbox"
                    checked={restoreSecrets}
                    onChange={(e) => setRestoreSecrets(e.target.checked)}
                  />{" "}
                  Also restore the API keys in this backup
                </label>
              )}
              {hasRemotePairing(restoring.manifest) && (
                <label className="check">
                  <input
                    type="checkbox"
                    checked={restoreRemote}
                    onChange={(e) => setRestoreRemote(e.target.checked)}
                  />{" "}
                  Also restore remote access and paired devices
                </label>
              )}
              {restoreRemote && (
                <p className="hint data-warning" role="note">
                  Devices you removed since this backup can connect again.
                  Remote access stays off until you turn it on in Settings ›
                  Remote access, where you can check the devices first.
                </p>
              )}
              {restoring.ignored.length > 0 && (
                <p className="hint">
                  Not restored (made by a newer version):{" "}
                  {restoring.ignored.join(", ")}
                </p>
              )}
            </>
          )}
        </ConfirmDialog>
      )}
      {resetting && (
        <ConfirmDialog
          title="Reset ShadowCode?"
          confirmLabel="Reset at next start"
          danger
          onCancel={() => setResetting(false)}
          onConfirm={async () => {
            const scheduled = await act("reset", dataApi.reset);
            if (scheduled) {
              setResetting(false);
              onToast(scheduled.message, "info");
              await load();
            }
          }}
        >
          <p>
            The next time ShadowCode starts, your settings, API keys,
            conversations and history move into folders named{" "}
            <code>….reset-</code> and the date, next to where they are now.
            Nothing is deleted, and you can cancel before then.
          </p>
          <p>These stay where they are: {overview.kept_on_reset.join(", ")}.</p>
        </ConfirmDialog>
      )}
    </section>
  );
}
