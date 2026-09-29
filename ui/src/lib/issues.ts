import { request } from "./transport";
import type { ForgeCli, RemoteInfo } from "./forge";
import type { PrDraft } from "../hooks/useDrawerMemory";

/** "Start from an issue" (`/api/issues…`, docs/API_CONTRACT.md). */

export type IssueComment = { author: string; body: string; created_at: string };

export type Issue = {
  number: number;
  title: string;
  body: string;
  url: string;
  author: string;
  labels: string[];
  state: string;
  updated_at: string;
  comments: IssueComment[];
  comment_count: number;
};

export type IssueList = {
  ready: boolean;
  remote?: string | null;
  provider: "github" | "gitlab" | "other" | null;
  remote_info?: RemoteInfo;
  cli: ForgeCli;
  /** Why issues cannot be listed (no remote, another forge). */
  reason?: string;
  issues: Issue[];
};

export type IssueDetail = {
  provider: "github" | "gitlab";
  issue: Issue;
  /** The task to review in the composer. */
  task: string;
  /** The task's first words; a finished task starting with them offers a
   * pull request that closes the issue. */
  marker: string;
  branch: string;
};

/** The issue a task was started from (kept in drawer memory). */
export type IssueLink = {
  number: number;
  title: string;
  url: string;
  provider: "github" | "gitlab";
  marker: string;
  branch: string;
};

export const issuesApi = {
  list: (remote = "") =>
    request<IssueList>(
      `/api/issues${remote ? `?remote=${encodeURIComponent(remote)}` : ""}`,
    ),
  get: (number: number, remote = "") =>
    request<IssueDetail>(
      `/api/issues/${number}${remote ? `?remote=${encodeURIComponent(remote)}` : ""}`,
    ),
};

export function linkOf(detail: IssueDetail): IssueLink {
  return {
    number: detail.issue.number,
    title: detail.issue.title,
    url: detail.issue.url,
    provider: detail.provider,
    marker: detail.marker,
    branch: detail.branch,
  };
}

/** The pull request offered once a task started from an issue finishes:
 * GitHub and GitLab both close the issue when it merges. */
export function closingPr(link: IssueLink, base = ""): PrDraft {
  return {
    title: link.title,
    body: `Closes #${link.number}\n\n${link.url}`,
    base,
    draft: false,
  };
}

/** Offer the pull request when the conversation's last task started from
 * this issue and finished. */
export function issueFollowUp(
  link: IssueLink | null,
  job: { task?: string; status?: string } | null | undefined,
  busy: boolean,
): IssueLink | null {
  if (!link || !job || busy) return null;
  if (job.status !== "completed") return null;
  return (job.task || "").startsWith(link.marker) ? link : null;
}

export const forgeName = (provider: string | null | undefined) =>
  provider === "gitlab" ? "GitLab" : "GitHub";
