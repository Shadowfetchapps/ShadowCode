import { request } from "./transport";
import type { SecretFinding } from "../api";

/** The drawer's Git panel API (`/api/git…`, docs/API_CONTRACT.md).
 * Staging and committing stay on `api.gitAdd` / `api.gitCommit`. */

export type RemoteInfo = {
  host: string;
  path: string;
  web_url: string;
  kind: "github" | "gitlab" | "other";
};

export type GitOverview = {
  repo: boolean;
  branch?: string | null;
  detached?: boolean;
  has_commits?: boolean;
  upstream?: string | null;
  ahead?: number;
  behind?: number;
  staged?: number;
  changed?: number;
  branches?: { name: string; upstream: string; current: boolean }[];
  remotes?: { name: string; info: RemoteInfo | null }[];
  remote?: string | null;
  remote_info?: RemoteInfo | null;
  bases?: string[];
  default_base?: string;
};

export type Suggestion = {
  kind: "commit" | "pr";
  /** `model`: the conversation's model; `local`: the loaded local model;
   * `summary`: written without a model. */
  source: "model" | "local" | "summary";
  model: string;
  note: string;
  message?: string;
  title?: string;
  body?: string;
};

export type ForgeCli = {
  name: "gh" | "glab" | null;
  installed?: boolean;
  version?: string;
  authenticated?: boolean;
  detail?: string;
  install_url?: string;
  login_command?: string;
};

export type PullRequest = {
  number: number;
  url: string;
  state?: string;
  draft?: boolean;
  title?: string;
  base?: string;
};

export type PrStatus = {
  remote: string | null;
  provider: "github" | "gitlab" | "other" | null;
  remote_info?: RemoteInfo;
  cli: ForgeCli;
  base?: string;
  compare_url?: string | null;
  pr: PullRequest | null;
};

export type PrCreated = {
  ok: boolean;
  /** Refused in band: the commits to push may contain a secret. */
  secrets?: SecretFinding[];
  secrets_truncated?: boolean;
  error?: string;
  url: string;
  number: number | null;
  provider: string;
  pushed: boolean;
  branch: string;
  base: string;
  draft: boolean;
};

export type CheckBucket = "pass" | "fail" | "pending" | "skipping";
export type PrChecks = {
  supported: boolean;
  checks: {
    name: string;
    workflow?: string;
    state?: string;
    bucket: CheckBucket;
    link?: string | null;
    description?: string;
  }[];
  summary: Partial<Record<CheckBucket, number>>;
  overall?: "pass" | "fail" | "pending" | "none";
  url: string;
  checked_at?: number;
};

const q = (value: string) => encodeURIComponent(value);

export const forgeApi = {
  overview: (remote = "") =>
    request<GitOverview>(`/api/git${remote ? `?remote=${q(remote)}` : ""}`),
  branch: (name: string, create: boolean) =>
    request<{ ok: boolean; branch: string }>("/api/git/branch", "POST", {
      name,
      create,
    }),
  suggest: (kind: "commit" | "pr", base = "", remote = "") =>
    request<Suggestion>("/api/git/suggest", "POST", { kind, base, remote }),
  push: (remote = "", allowSecrets = false) =>
    request<{
      ok: boolean;
      remote: string;
      branch: string;
      output: string;
      secrets?: SecretFinding[];
      secrets_truncated?: boolean;
      error?: string;
    }>("/api/git/push", "POST", {
      remote,
      ...(allowSecrets ? { allow_secrets: true } : {}),
    }),
  prStatus: (remote = "", base = "") =>
    request<PrStatus>(`/api/git/pr?remote=${q(remote)}&base=${q(base)}`),
  createPr: (body: {
    title: string;
    body: string;
    base: string;
    draft: boolean;
    remote?: string;
    allow_secrets?: boolean;
  }) => request<PrCreated>("/api/git/pr", "POST", body),
  checks: (number: number, remote = "") =>
    request<PrChecks>(
      `/api/git/pr/checks?number=${number}&remote=${q(remote)}`,
    ),
};

/** Mirrors the engine's rule so the form can explain a bad name at once. */
export function branchNameProblem(name: string): string {
  const n = name.trim();
  if (!n) return "Enter a branch name";
  if (n.length > 200) return "Branch names are at most 200 characters";
  if (n.startsWith("-")) return "A branch name cannot start with a dash";
  if (/[\s~^:?*[\\\x00-\x1f\x7f]/.test(n))
    return "A branch name cannot contain spaces or any of ~ ^ : ? * [ \\";
  if (
    n.includes("..") ||
    n.includes("@{") ||
    n.includes("//") ||
    n === "@" ||
    n === "HEAD" ||
    n.startsWith("/") ||
    n.endsWith("/") ||
    n.endsWith(".") ||
    n.endsWith(".lock") ||
    n.split("/").some((part) => part.startsWith("."))
  )
    return "That is not a valid branch name";
  return "";
}

/** "3 to push · 1 to pull" style status of a branch against its upstream. */
export function syncText(o: GitOverview): string {
  if (!o.branch) return o.detached ? "Detached commit (no branch)" : "";
  if (!o.upstream)
    return o.remote
      ? `Not on ${o.remote} yet${o.ahead ? ` · ${o.ahead} commit${o.ahead === 1 ? "" : "s"} to publish` : ""}`
      : "No remote configured";
  const parts = [];
  if (o.ahead) parts.push(`${o.ahead} to push`);
  if (o.behind) parts.push(`${o.behind} to pull`);
  return parts.length
    ? `${parts.join(" · ")} · ${o.upstream}`
    : `Up to date with ${o.upstream}`;
}

/** "Signed in as octo" from the forge CLI's own status line
 * ("Logged in to github.com account octo (keyring)" from gh, "Logged in to
 * gitlab.com as octo (/home/…/config.yml)" from glab). Anything else is shown
 * without the trailing storage detail in parentheses. */
export function signedInText(detail: string): string {
  const text = detail.trim();
  const account =
    text.match(/\baccount\s+([^\s()]+)/i)?.[1] ||
    text.match(/\blogged in to \S+ as\s+([^\s()]+)/i)?.[1];
  if (account) return `Signed in as ${account}`;
  return text.replace(/\s*\([^)]*\)\s*$/, "");
}
