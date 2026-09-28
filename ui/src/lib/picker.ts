/** Composer picker rows exactly as GET /api/picker returns them
 * (docs/API_CONTRACT_0.28.md). Nothing here invents availability or usage.
 * Three groups: subscriptions (vendor CLIs), API keys (OpenRouter, billed per
 * token) and models on this computer. */

export type UsageWindow = {
  label: string;
  used_percent: number | null;
  remaining_percent: number | null;
  window_minutes: number | null;
  resets_at: number | null;
};

export type UsageSnapshot = {
  state:
    | "ok"
    | "stale"
    | "unavailable"
    | "local"
    | "limit_reached"
    | "api_key"
    | string;
  label: string;
  detail?: string[];
  plan?: string | null;
  pool?: string | null;
  pool_shared?: boolean;
  windows?: UsageWindow[];
  remaining_percent?: number | null;
  credits?: {
    has_credits?: boolean;
    unlimited?: boolean;
    balance?: string | number | null;
  } | null;
  limit_reached?: boolean;
  last_refresh?: number | null;
  provider_usage_url?: string | null;
};

export type Availability =
  "ready" | "sign_in" | "setup_required" | "unavailable";

export type BillingMode = "subscription" | "api_key" | "unknown";

export type LocalDetail = {
  path?: string;
  bytes?: number;
  architecture?: string | null;
  fits?: "gpu" | "cpu" | "no";
  context_tokens?: number;
};

export type PickerTarget = {
  id: string;
  provider: string;
  account?: string;
  model?: string;
  route?: string;
  group: "subscriptions" | "api" | "local" | string;
  name: string;
  subtitle?: string;
  /** Explicit account observation; route/group never establish billing. */
  billing?: BillingMode | null;
  inference: "cloud" | "local" | string;
  availability: Availability | string;
  availability_label?: string;
  reason?: string;
  featured?: boolean;
  vision?: boolean;
  tools?: boolean;
  /** The composer's reasoning-effort control applies to this row. */
  reasoning?: boolean;
  is_default?: boolean;
  usage?: UsageSnapshot | null;
  local?: LocalDetail;
};

export const UNKNOWN_USAGE = "Usage unavailable · Open provider usage";
export const LOCAL_USAGE = "Runs on this computer · No subscription quota";
export const API_KEY_USAGE = "API key · billed per token";

/** Missing legacy data remains unspecified, never inferred from the route. */
export function billingWarning(
  target?: { billing?: BillingMode | null } | null,
): string | undefined {
  if (target?.billing === "unknown")
    return "Billing not verified · Check your provider account";
  if (target?.billing === "api_key") return "API key login · billed per token";
  return undefined;
}

export const isLocal = (target: PickerTarget) =>
  target.inference === "local" || target.group === "local";

/** Rows paid per token with the user's own API key (OpenRouter). They never
 * share a group with subscription rows. */
export const isApiKey = (target: PickerTarget) =>
  !isLocal(target) &&
  (target.group === "api" || target.provider === "openrouter");

/** Vendors whose product name is not the first part of the row name
 * (OpenRouter rows are named after the model: "Qwen: Qwen3 Coder"). */
const VENDOR_LABELS: Record<string, string> = { openrouter: "OpenRouter" };

/** The row name without the " · This computer" suffix, for places that
 * already carry a Local badge next to it (the composer trigger). */
export const shortName = (target: PickerTarget) =>
  isLocal(target) ? target.name.replace(/ · This computer$/, "") : target.name;

export const isReady = (target: PickerTarget) =>
  target.availability === "ready";

/** "cli:cursor" → "cursor", "openrouter" → "openrouter"; local rows share the
 * "local" key. */
export function vendorKey(target: PickerTarget): string {
  if (isLocal(target)) return "local";
  return target.provider.replace(/^cli:/, "");
}

/** The product name before " · " in a row name ("Cursor · Auto" → "Cursor"). */
export function vendorLabel(target: PickerTarget): string {
  const known = VENDOR_LABELS[vendorKey(target)];
  if (known) return known;
  return target.name.split(" · ")[0] || target.provider;
}

export function availabilityLabel(target: PickerTarget): string {
  if (target.availability_label) return target.availability_label;
  switch (target.availability) {
    case "ready":
      return "Ready";
    case "sign_in":
      return "Sign in";
    case "setup_required":
      return "Setup required";
    default:
      return "Unavailable";
  }
}

export function relativeTime(seconds: number, now = Date.now() / 1000): string {
  const delta = Math.max(0, Math.round(now - seconds));
  if (delta < 60) return "just now";
  if (delta < 3600) return `${Math.floor(delta / 60)}m ago`;
  if (delta < 86400) return `${Math.floor(delta / 3600)}h ago`;
  return `${Math.floor(delta / 86400)}d ago`;
}

export function resetTime(seconds: number, now = Date.now() / 1000): string {
  const delta = seconds - now;
  if (delta <= 0) return "resets now";
  // Round to whole minutes first so 2h 59m 50s reads "3h", never "2h 60m".
  const total = Math.max(1, Math.round(delta / 60));
  if (total < 60) return `resets in ${total}m`;
  if (total < 24 * 60) {
    const hours = Math.floor(total / 60);
    const minutes = total % 60;
    return `resets in ${hours}h${minutes ? ` ${minutes}m` : ""}`;
  }
  const date = new Date(seconds * 1000);
  return `resets ${date.toLocaleDateString(undefined, { weekday: "short", month: "short", day: "numeric" })} ${date.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit" })}`;
}

/** One line for a picker row. The backend's label is authoritative; the UI only
 * adds the age of a stale snapshot, computed at render time. */
export function usageLabel(
  usage: UsageSnapshot | null | undefined,
  inference?: string,
  now = Date.now() / 1000,
): string {
  if (inference === "local" || usage?.state === "local")
    return usage?.label || LOCAL_USAGE;
  if (!usage) return UNKNOWN_USAGE;
  // Per-token prices come from the engine as-is ("API key · $0.30/M in …").
  if (usage.state === "api_key") return usage.label || API_KEY_USAGE;
  if (usage.state === "stale") {
    const checked = usage.last_refresh
      ? `Last checked ${relativeTime(usage.last_refresh, now)}`
      : "Last checked earlier";
    return usage.label && !/last checked/i.test(usage.label)
      ? `${usage.label} · ${checked}`
      : checked;
  }
  if (usage.state === "unavailable") return usage.label || UNKNOWN_USAGE;
  return usage.label || UNKNOWN_USAGE;
}

/** The usage part of a picker row after its availability, without saying
 * the same thing twice ("Plan limit reached · Plan limit reached · resets
 * in 2h" reads "Plan limit reached · resets in 2h"). */
export function usageAfterAvailability(
  availability: string,
  usage: string,
): string {
  const same = availability.trim().toLowerCase();
  const text = usage.trim();
  if (same && text.toLowerCase().startsWith(same))
    return text
      .slice(same.length)
      .replace(/^\s*·\s*/, "")
      .trim();
  return text;
}

/** Expandable detail lines: backend detail first, then windows with reset
 * times, plan/pool/credits and the refresh age — only fields that were
 * reported. */
function planLine(plan: string): string {
  return `${plan.charAt(0).toUpperCase()}${plan.slice(1)} plan`;
}

export function usageDetailLines(
  usage: UsageSnapshot | null | undefined,
  now = Date.now() / 1000,
): string[] {
  if (!usage || usage.state === "local") return [];
  // With structured windows, build every line here so reset times stay live
  // and nothing is repeated. Otherwise the engine's lines carry the reason.
  if (!usage.windows?.length) {
    const lines = [...(usage.detail || [])];
    if (usage.plan && !lines.some((l) => l.includes(usage.plan as string)))
      lines.unshift(planLine(usage.plan));
    if (usage.last_refresh)
      lines.push(`Last checked ${relativeTime(usage.last_refresh, now)}`);
    return lines;
  }
  const lines: string[] = [];
  if (usage.plan) lines.push(planLine(usage.plan));
  for (const window of usage.windows || []) {
    const parts = [window.label];
    if (window.remaining_percent != null)
      parts.push(`${Math.round(window.remaining_percent)}% left`);
    else if (window.used_percent != null)
      parts.push(`${Math.round(window.used_percent)}% used`);
    if (window.resets_at) parts.push(resetTime(window.resets_at, now));
    const line = parts.join(" · ");
    if (!lines.includes(line)) lines.push(line);
  }
  if (usage.pool)
    lines.push(
      `${usage.pool_shared ? "Shared pool" : "Pool"}: ${usage.pool}${usage.pool_shared ? " (shared with other models of this plan)" : ""}`,
    );
  // Credits only when the provider says the account has some.
  if (usage.credits?.unlimited) lines.push("Credits: unlimited");
  else if (usage.credits?.has_credits && usage.credits.balance != null)
    lines.push(`Credits: ${usage.credits.balance}`);
  if (usage.last_refresh)
    lines.push(`Last checked ${relativeTime(usage.last_refresh, now)}`);
  return lines;
}

export type TargetGroups = {
  subscriptions: PickerTarget[];
  api: PickerTarget[];
  local: PickerTarget[];
};

/** API-key rows go to their own group; every other non-local row is a
 * subscription row. `featured` only orders subscription rows. */
export function groupTargets(targets: PickerTarget[]): TargetGroups {
  const subscriptions: PickerTarget[] = [];
  const api: PickerTarget[] = [];
  const local: PickerTarget[] = [];
  for (const target of targets) {
    if (isLocal(target)) local.push(target);
    else if (isApiKey(target)) api.push(target);
    else subscriptions.push(target);
  }
  subscriptions.sort(
    (a, b) => Number(b.featured !== false) - Number(a.featured !== false),
  );
  return { subscriptions, api, local };
}

export function matchesQuery(target: PickerTarget, query: string): boolean {
  const words = query.toLowerCase().split(/\s+/).filter(Boolean);
  if (!words.length) return true;
  const haystack = [
    target.name,
    target.subtitle,
    target.provider,
    target.model,
    target.availability_label,
    isLocal(target)
      ? "local this computer"
      : target.billing === "unknown"
        ? "cloud billing unverified"
        : target.billing === "api_key" || isApiKey(target)
          ? "cloud api key"
          : "cloud subscription",
  ]
    .filter(Boolean)
    .join(" ")
    .toLowerCase();
  return words.every((word) => haystack.includes(word));
}

/** Vendors with more rows than this collapse to default + recent + selected. */
export const COLLAPSE_AFTER = 4;

export type VendorSection = {
  key: string;
  label: string;
  rows: PickerTarget[];
  hidden: number;
};

/** Split a group into per-vendor sections, collapsing long vendor lists unless
 * the user expanded them or is searching. */
export function vendorSections(
  rows: PickerTarget[],
  options: {
    expanded: string[];
    recent: string[];
    selected: string;
    searching: boolean;
  },
): VendorSection[] {
  const order: string[] = [];
  const byVendor = new Map<string, PickerTarget[]>();
  for (const row of rows) {
    const key = vendorKey(row);
    if (!byVendor.has(key)) {
      byVendor.set(key, []);
      order.push(key);
    }
    byVendor.get(key)!.push(row);
  }
  return order.map((key) => {
    const all = byVendor.get(key)!;
    const label = key === "local" ? "On this computer" : vendorLabel(all[0]);
    if (
      options.searching ||
      options.expanded.includes(key) ||
      all.length <= COLLAPSE_AFTER
    )
      return { key, label, rows: all, hidden: 0 };
    const keep = all.filter(
      (row, index) =>
        row.is_default ||
        row.id === options.selected ||
        options.recent.includes(row.id) ||
        (index === 0 && !all.some((r) => r.is_default)),
    );
    return { key, label, rows: keep, hidden: all.length - keep.length };
  });
}

const RECENT_KEY = "shadow:recent-targets";

export function recentTargets(): string[] {
  try {
    const value = JSON.parse(localStorage.getItem(RECENT_KEY) || "[]");
    return Array.isArray(value) ? value.map(String).slice(0, 8) : [];
  } catch {
    return [];
  }
}

export function rememberRecent(id: string) {
  try {
    const next = [id, ...recentTargets().filter((item) => item !== id)].slice(
      0,
      8,
    );
    localStorage.setItem(RECENT_KEY, JSON.stringify(next));
  } catch {
    /* Recent rows are a convenience only. */
  }
}

/** What activating a row does. Non-ready rows are never dead: they explain or
 * lead to the fix. */
export type RowAction =
  | { kind: "select" }
  | { kind: "connect"; vendor: string }
  | { kind: "setup"; hint: string; local: boolean }
  | { kind: "explain"; reason: string };

/** Antigravity's agent server is installed from its Accounts card. */
export const ANTIGRAVITY_SETUP_HINT =
  "Install the Antigravity agent in Settings › Accounts.";

export function rowAction(target: PickerTarget): RowAction {
  switch (target.availability) {
    case "ready":
      return { kind: "select" };
    case "sign_in":
      return { kind: "connect", vendor: vendorKey(target) };
    case "setup_required":
      return {
        kind: "setup",
        local: isLocal(target),
        // Antigravity's row reason already names the download; the hint
        // says where to install it (its Accounts card).
        hint:
          vendorKey(target) === "antigravity"
            ? ANTIGRAVITY_SETUP_HINT
            : target.reason ||
              (isLocal(target)
                ? "The local runtime or this model needs setup."
                : isApiKey(target)
                  ? `Add an API key for ${vendorLabel(target)} in Accounts.`
                  : `Install the ${vendorLabel(target)} command-line tool, then refresh Accounts.`),
      };
    default:
      return {
        kind: "explain",
        reason: target.reason || "This option is unavailable right now.",
      };
  }
}
