/** Second opinions (`/api/second-opinions…`, docs/API_CONTRACT_0.28.md): a
 * read-only review of staged changes or of one task's changes by a model the
 * user picks, or another model's view of an answer. Types, the API and the
 * pure helpers the Git tab, the Review view and the transcript share. */
import type { ConsentRequest, Usage } from "../api";
import { hunkStarts, parseUnified, type Hunk } from "./diff";
import { isApiKey, isLocal, type PickerTarget } from "./picker";
import { ApiError, request } from "./transport";

export type Severity = "high" | "medium" | "low" | "info";

export type Finding = {
  id: string;
  /** Project-relative path; empty when the reviewer named no file. */
  file: string;
  line?: number | null;
  end_line?: number | null;
  /** Header of the reviewed hunk the line falls in. */
  hunk?: string | null;
  severity: Severity | string;
  title: string;
  explanation: string;
  suggested_fix: string;
  /** `open`, `dismissed`, or `fixing` once a follow-up task was queued. */
  status: "open" | "dismissed" | "fixing" | string;
  fix_job_id?: string | null;
  fix_session_id?: string | null;
};

export type OpinionRoute = { model: string; label: string; local: boolean };

export type ReviewedFile = {
  path: string;
  status: string;
  /** The file's hunks as unified diff text. */
  diff: string;
  binary: boolean;
};

export type SecondOpinion = {
  id: string;
  kind: "review" | "ask";
  workspace: string;
  source: "staged" | "task";
  session_id?: string | null;
  task_id?: string | null;
  question: string;
  reviewer: OpinionRoute;
  writer?: OpinionRoute | null;
  same_model: boolean;
  consented: boolean;
  job_id: string;
  review_session: string;
  review_task: string;
  /** `queued`, `running`, `completed`, `failed`, `cancelled`,
   * `limit_reached` or `interrupted`. */
  status: string;
  created_at: number;
  finished_at?: number | null;
  diff_hash: string;
  files: string[];
  omitted: string[];
  diff: ReviewedFile[];
  truncated: boolean;
  context_chars: number;
  summary: string;
  findings: Finding[];
  format_note: string;
  error: string;
  usage?: Usage;
  model_name: string;
  reviewer_changed: string[];
};

export type OpinionPrefs = { model: string | null; before_commit: boolean };

export type OpinionOptions = {
  workspace: string;
  prefs: OpinionPrefs;
  offline: boolean;
  /** The model that wrote the change or answer, when known. */
  writer: OpinionRoute | null;
  /** The work ran on this computer: a cloud reviewer needs consent. */
  local_only: boolean;
};

export type StartOpinion = {
  kind: "review" | "ask";
  source: "staged" | "task";
  workspace?: string;
  session_id?: string;
  task_id?: string;
  model: string;
  question?: string;
  consent?: boolean;
};

export type OpinionScope = {
  workspace?: string;
  session_id?: string;
  task_id?: string;
  source?: "staged" | "task";
};

const query = (params: Record<string, string | undefined>) => {
  const parts = Object.entries(params)
    .filter(([, value]) => value)
    .map(([key, value]) => `${key}=${encodeURIComponent(value as string)}`);
  return parts.length ? `?${parts.join("&")}` : "";
};

function consentOf(value: unknown): ConsentRequest | null {
  if (
    value &&
    typeof value === "object" &&
    (value as { needs_consent?: unknown }).needs_consent === true
  ) {
    const body = value as ConsentRequest;
    return { ...body, handoff: body.handoff || {} };
  }
  return null;
}

/** A request that may answer `needs_consent` (409 or an IPC value). */
async function consented<T>(
  path: string,
  body: unknown,
): Promise<{ value: T } | { consent: ConsentRequest }> {
  try {
    const value = await request<T>(path, "POST", body);
    const consent = consentOf(value);
    return consent ? { consent } : { value };
  } catch (error) {
    const consent = error instanceof ApiError ? consentOf(error.body) : null;
    if (consent) return { consent };
    throw error;
  }
}

export const opinionApi = {
  list: (scope: OpinionScope) =>
    request<{ workspace: string; second_opinions: SecondOpinion[] }>(
      `/api/second-opinions${query(scope)}`,
    ),
  get: (id: string) =>
    request<SecondOpinion>(`/api/second-opinions/${encodeURIComponent(id)}`),
  /** Starts a second opinion; a cloud reviewer of local work answers
   * `consent` and nothing is sent until it is repeated with `consent`. */
  start: async (body: StartOpinion) => {
    const answer = await consented<SecondOpinion>("/api/second-opinions", body);
    return "consent" in answer
      ? answer
      : { second_opinion: answer.value as SecondOpinion };
  },
  cancel: (id: string) =>
    request<SecondOpinion>(
      `/api/second-opinions/${encodeURIComponent(id)}/cancel`,
      "POST",
      {},
    ),
  setFinding: (id: string, finding: string, status: "open" | "dismissed") =>
    request<SecondOpinion>(
      `/api/second-opinions/${encodeURIComponent(id)}/findings/${encodeURIComponent(finding)}`,
      "POST",
      { status },
    ),
  /** Queues "fix this" in the conversation the review belongs to. */
  fix: (id: string, finding: string, consent = false) =>
    consented<{
      second_opinion: SecondOpinion;
      job: { id: string; session_id: string; status: string };
    }>(
      `/api/second-opinions/${encodeURIComponent(id)}/findings/${encodeURIComponent(finding)}/fix`,
      { consent },
    ),
  options: (scope: OpinionScope) =>
    request<OpinionOptions>(`/api/second-opinions/options${query(scope)}`),
  /** A fingerprint of the changes as they are now (to spot an outdated
   * review) and the files a review would see. */
  current: (scope: OpinionScope) =>
    request<{
      hash: string;
      files: string[];
      omitted: string[];
      truncated: boolean;
    }>(`/api/second-opinions/current${query(scope)}`),
  savePrefs: (
    workspace: string,
    change: { model?: string; before_commit?: boolean },
  ) =>
    request<OpinionPrefs>("/api/second-opinions/prefs", "POST", {
      workspace,
      ...change,
    }),
};

export const isActiveOpinion = (opinion: SecondOpinion) =>
  opinion.status === "queued" || opinion.status === "running";

/** Rows a reviewer can be picked from: ready rows (only this computer's in
 * offline mode). API-key rows are many, so only featured ones, and the one
 * already chosen, are offered. */
export function reviewerChoices(
  targets: PickerTarget[],
  { offline, keep }: { offline: boolean; keep?: string },
): PickerTarget[] {
  return targets.filter(
    (target) =>
      target.availability === "ready" &&
      (!offline || isLocal(target)) &&
      (!isApiKey(target) || target.featured || target.id === keep),
  );
}

/** The reviewer to preselect: the project's last choice unless that model
 * wrote the change, else another model, preferring another provider. Work
 * that ran on this computer only gets a local model preselected; a cloud
 * model stays one explicit choice (and a consent dialog) away. */
export function suggestReviewer(
  targets: PickerTarget[],
  {
    writer,
    remembered,
    offline,
    localOnly,
  }: {
    writer?: string | null;
    remembered?: string | null;
    offline: boolean;
    localOnly: boolean;
  },
): string {
  const choices = reviewerChoices(targets, {
    offline,
    keep: remembered || undefined,
  });
  const allowed = localOnly ? choices.filter(isLocal) : choices;
  if (
    remembered &&
    remembered !== writer &&
    allowed.some((target) => target.id === remembered)
  )
    return remembered;
  const writerRow = targets.find((target) => target.id === writer);
  const others = allowed.filter((target) => target.id !== writer);
  const otherProvider = others.find(
    (target) => !writerRow || target.provider !== writerRow.provider,
  );
  return otherProvider?.id || others[0]?.id || "";
}

/** `@@ -a,b +c,d @@` → the new side's first and last line. */
export function newRange(header: string): { start: number; end: number } {
  const match = /@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@/.exec(header);
  if (!match) {
    const start = hunkStarts(header).new;
    return { start, end: start };
  }
  const start = Number(match[1]);
  const length = match[2] === undefined ? 1 : Number(match[2]);
  return { start, end: start + Math.max(length, 1) - 1 };
}

/** The findings that belong next to one hunk of `path`: the reviewer's
 * hunk, or a line inside the hunk's new lines. */
export function findingsForHunk(
  findings: Finding[],
  path: string,
  header: string,
): Finding[] {
  const range = newRange(header);
  return findings.filter(
    (finding) =>
      finding.file === path &&
      (finding.hunk === header ||
        (finding.line != null &&
          finding.line >= range.start &&
          finding.line <= range.end)),
  );
}

/** Findings of `path` that no hunk in `headers` shows (no line, a line
 * outside every hunk, or a file that is not among the hunks). */
export function unplaced(
  findings: Finding[],
  path: string,
  headers: string[],
): Finding[] {
  return findings.filter(
    (finding) =>
      finding.file === path &&
      !headers.some((header) =>
        findingsForHunk([finding], path, header).includes(finding),
      ),
  );
}

/** The reviewed diff as hunks, file by file. */
export function reviewedHunks(
  opinion: SecondOpinion,
): { path: string; status: string; binary: boolean; hunks: Hunk[] }[] {
  return opinion.diff.map((file) => ({
    path: file.path,
    status: file.status,
    binary: file.binary,
    hunks: file.binary ? [] : parseUnified(file.diff),
  }));
}

export const SEVERITY_LABEL: Record<string, string> = {
  high: "High",
  medium: "Medium",
  low: "Low",
  info: "Note",
};

const SEVERITY_ORDER: Record<string, number> = {
  high: 0,
  medium: 1,
  low: 2,
  info: 3,
};
export const bySeverity = (a: Finding, b: Finding) =>
  (SEVERITY_ORDER[a.severity] ?? 1) - (SEVERITY_ORDER[b.severity] ?? 1);

export const openFindings = (opinion: SecondOpinion) =>
  opinion.findings.filter((finding) => finding.status === "open");

/** "Waiting…", "Reviewing…", "3 findings", "No problems found", "Failed". */
export function opinionStatus(opinion: SecondOpinion): string {
  switch (opinion.status) {
    case "queued":
      return "Waiting for the running task to finish";
    case "running":
      return opinion.kind === "review" ? "Reviewing…" : "Thinking…";
    case "completed": {
      if (opinion.kind === "ask") return "Done";
      const count = opinion.findings.length;
      if (!count)
        return opinion.format_note ? "No findings read" : "No problems found";
      return `${count} finding${count === 1 ? "" : "s"}`;
    }
    case "cancelled":
      return "Stopped";
    case "limit_reached":
      return "Plan limit reached";
    case "interrupted":
      return "Interrupted";
    default:
      return "Failed";
  }
}

/** "1,234 tokens · $0.0210" or "1,234 tokens · no cost" (this computer). */
export function usageText(opinion: SecondOpinion): string {
  const usage = opinion.usage || {};
  const parts: string[] = [];
  if (usage.total_tokens)
    parts.push(`${usage.total_tokens.toLocaleString("en-US")} tokens`);
  if (opinion.reviewer.local) parts.push("no cost (this computer)");
  else if (typeof usage.cost_usd === "number")
    parts.push(
      `${usage.cost_estimated ? "about " : ""}$${usage.cost_usd.toFixed(4)}`,
    );
  else if (opinion.status === "completed")
    parts.push("cost not reported (plan allowance)");
  return parts.join(" · ");
}

/** The composer text for "Continue with this model". */
export function continuePrompt(opinion: SecondOpinion): string {
  const text = opinion.summary.trim();
  const clipped = text.length > 4000 ? `${text.slice(0, 3999)}…` : text;
  return `Here is your second opinion on the last answer:\n\n${clipped
    .split("\n")
    .map((line) => `> ${line}`)
    .join("\n")}\n\nContinue from here with that in mind.`;
}
