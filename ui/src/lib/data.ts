import { invoke, request } from "./transport";

/** Settings › Your data (`/api/data…`, docs/API_CONTRACT.md and
 * docs/DATA.md): backups, restore, repair and reset. Times are Unix seconds. */

export type BackupFile = { path: string; bytes: number; sha256: string };

export type BackupManifest = {
  format: string;
  format_version: number;
  app_version: string;
  schema_version: number;
  created_at: number;
  includes_secrets: boolean;
  reason: "manual" | "before-restore" | "before-repair" | "upgrade-copy";
  files: BackupFile[];
  raw_copy: boolean;
  /** Keys kept in the keyring that could not be read into the backup. */
  keys_left_out?: string[];
};

export type ListedBackup = {
  path: string;
  name: string;
  app_version: string;
  schema_version: number;
  created_at: number;
  includes_secrets: boolean;
  reason: BackupManifest["reason"];
  bytes: number;
};

export type UpgradeCopy = {
  path: string;
  name: string;
  bytes: number;
  created_at: number;
  schema_version: number | null;
};

export type PendingOperation = {
  kind: "restore" | "reset";
  requested_at: number;
  source: string | null;
  include_secrets: boolean;
};

export type RepairCheck = {
  id: string;
  label: string;
  status: "pass" | "warn" | "fail" | "not_checked";
  detail: string;
};

export type LastOperation = {
  kind: "restore" | "reset" | "repair";
  ok: boolean;
  finished_at: number;
  error?: string;
  source?: string | null;
  backup_of_previous_data?: string;
  secrets_restored?: boolean;
  moved_to?: string[];
  checks?: RepairCheck[];
  backup?: string;
};

export type DataOverview = {
  folders: { config: string; data: string; state: string };
  database: {
    path: string;
    bytes: number;
    wal_bytes: number;
    schema_version: number | null;
    supported_schema_version: number;
  };
  app_version: string;
  backups_folder: string;
  backups: ListedBackup[];
  upgrade_copies: UpgradeCopy[];
  reset_folders: string[];
  kept_on_reset: string[];
  pending: PendingOperation | null;
  last_operation: LastOperation | null;
};

export type BackupSummary = {
  conversations: number;
  tasks: number;
  jobs: number;
  goals: number;
  automations: number;
  comparisons: number;
  last_activity: number | null;
};

export type Inspection = {
  path: string;
  kind: "backup" | "database";
  manifest: BackupManifest;
  summary: BackupSummary | null;
  ignored: string[];
  problems: string[];
  restorable: boolean;
};

export type RepairReport = {
  kind: "repair";
  ok: boolean;
  finished_at: number;
  checks: RepairCheck[];
  cleared: string[];
  backup: string;
};

export type Scheduled = {
  scheduled: true;
  pending: PendingOperation;
  message: string;
};

export const dataApi = {
  overview: () => request<DataOverview>("/api/data"),
  createBackup: (includeSecrets: boolean, folder = "") =>
    request<{ path: string; manifest: BackupManifest }>(
      "/api/data/backups",
      "POST",
      { include_secrets: includeSecrets, folder },
    ),
  inspect: (path: string) =>
    request<Inspection>("/api/data/backups/inspect", "POST", { path }),
  restore: (path: string, includeSecrets: boolean) =>
    request<Scheduled>("/api/data/restore", "POST", {
      path,
      include_secrets: includeSecrets,
    }),
  reset: () =>
    request<Scheduled>("/api/data/reset", "POST", { confirm: "reset" }),
  cancelPending: () =>
    request<{ cancelled: boolean }>("/api/data/pending", "DELETE"),
  repair: () => request<RepairReport>("/api/data/repair", "POST", {}),
};

/** A folder picker titled for backups (desktop only; `null` when closed). */
export const pickDataFolder = (purpose: "backup" | "restore") =>
  invoke<string | null>("pick_data_folder", { purpose });

/** "12.4 MB" style sizes. */
export function formatBytes(bytes: number): string {
  if (bytes < 1000) return `${bytes} bytes`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = bytes / 1000;
  let unit = 0;
  while (value >= 1000 && unit < units.length - 1) {
    value /= 1000;
    unit += 1;
  }
  return `${value.toFixed(value < 10 ? 1 : 0)} ${units[unit]}`;
}

/** What a backup holds, in one sentence. */
export function describeSummary(summary: BackupSummary | null): string {
  if (!summary) return "Its contents could not be read.";
  const parts = [
    [summary.conversations, "conversation"],
    [summary.tasks, "task"],
    [summary.goals, "goal"],
    [summary.automations, "automation"],
    [summary.comparisons, "comparison"],
  ]
    .filter(([count]) => Number(count) > 0)
    .map(([count, word]) => `${count} ${word}${count === 1 ? "" : "s"}`);
  return parts.length
    ? `It holds ${parts.join(", ")}.`
    : "It holds no conversations.";
}
