import { describe, expect, it } from "vitest";
import {
  resetTime,
  availabilityLabel,
  groupTargets,
  matchesQuery,
  rowAction,
  UNKNOWN_USAGE,
  usageDetailLines,
  usageLabel,
  vendorKey,
  vendorLabel,
  vendorSections,
  type PickerTarget,
  usageAfterAvailability,
} from "./picker";

const now = 1_800_000_000;
const row = (over: Partial<PickerTarget>): PickerTarget => ({
  id: "cli:codex:gpt-6-astra",
  provider: "cli:codex",
  group: "subscriptions",
  name: "Codex · GPT-6-Astra",
  inference: "cloud",
  availability: "ready",
  availability_label: "Ready",
  usage: { state: "unavailable", label: UNKNOWN_USAGE },
  ...over,
});

const openrouter = (
  slug: string,
  name: string,
  over: Partial<PickerTarget> = {},
) =>
  row({
    id: `api:openrouter:${slug}`,
    provider: "openrouter",
    account: "",
    model: slug,
    route: "native",
    group: "api",
    name,
    subtitle: `OpenRouter · ${slug}`,
    featured: false,
    is_default: false,
    usage: {
      state: "api_key",
      label: "API key · $0.30/M in · $1.20/M out",
      detail: ["Billed per token to your OpenRouter key"],
      provider_usage_url: "https://openrouter.ai/activity",
    },
    ...over,
  });

describe("usage labels", () => {
  it("uses the backend label and never invents numbers", () => {
    expect(
      usageLabel({
        state: "ok",
        label: "Shared plan usage · 2% left · resets Fri",
      }),
    ).toBe("Shared plan usage · 2% left · resets Fri");
    expect(usageLabel({ state: "unavailable", label: "" })).toBe(UNKNOWN_USAGE);
    expect(usageLabel(null)).toBe(UNKNOWN_USAGE);
    expect(usageLabel({ state: "unavailable", label: "" })).not.toMatch(/\d+%/);
  });

  it("computes the age of a stale snapshot at render time", () => {
    expect(
      usageLabel(
        { state: "stale", label: "", last_refresh: now - 5 * 60 },
        "cloud",
        now,
      ),
    ).toBe("Last checked 5m ago");
    expect(
      usageLabel(
        { state: "stale", label: "2% left", last_refresh: now - 7200 },
        "cloud",
        now,
      ),
    ).toBe("2% left · Last checked 2h ago");
  });

  it("shows the API-key price label as the engine sent it", () => {
    const qwen = openrouter("qwen/qwen3-coder", "Qwen: Qwen3 Coder");
    expect(usageLabel(qwen.usage, "cloud")).toBe(
      "API key · $0.30/M in · $1.20/M out",
    );
    expect(usageLabel({ state: "api_key", label: "API key · free" })).toBe(
      "API key · free",
    );
    expect(usageLabel({ state: "api_key", label: "" })).toBe(
      "API key · billed per token",
    );
    expect(usageDetailLines(qwen.usage, now)).toEqual([
      "Billed per token to your OpenRouter key",
    ]);
  });

  it("labels local rows as free of subscription quota", () => {
    expect(usageLabel(undefined, "local")).toBe(
      "Runs on this computer · No subscription quota",
    );
  });

  it("builds live window lines from structured usage without repeating the engine text", () => {
    const lines = usageDetailLines(
      {
        state: "ok",
        label: "2% left",
        detail: ["ChatGPT plan: pro", "Weekly: 98% used · resets in 9h"],
        plan: "pro",
        pool: "codex",
        pool_shared: true,
        windows: [
          {
            label: "Weekly",
            used_percent: 98,
            remaining_percent: 2,
            window_minutes: 10080,
            resets_at: now + 3 * 3600,
          },
        ],
        credits: { has_credits: false, unlimited: false, balance: "0" },
        last_refresh: now - 60,
      },
      now,
    );
    expect(lines[0]).toBe("Pro plan");
    expect(lines).toContain("Weekly · 2% left · resets in 3h");
    expect(lines.some((l) => l.includes("98% used"))).toBe(false);
    expect(lines.some((l) => l.startsWith("Shared pool: codex"))).toBe(true);
    expect(lines.some((l) => l.startsWith("Credits"))).toBe(false);
    expect(lines).toContain("Last checked 1m ago");
    expect(usageDetailLines({ state: "unavailable", label: "" }, now)).toEqual(
      [],
    );
  });
  it("rounds reset times to whole minutes", () => {
    expect(resetTime(now + 3 * 3600 - 10, now)).toBe("resets in 3h");
    expect(resetTime(now + 3599, now)).toBe("resets in 1h");
    expect(resetTime(now + 90 * 60 + 20, now)).toBe("resets in 1h 30m");
    expect(resetTime(now + 20, now)).toBe("resets in 1m");
  });
  it("shows the engine's reason when no window is reported", () => {
    expect(
      usageDetailLines(
        {
          state: "unavailable",
          label: "Usage unavailable",
          detail: ["Codex did not report rate limits for this account"],
          last_refresh: now - 60,
        },
        now,
      ),
    ).toEqual([
      "Codex did not report rate limits for this account",
      "Last checked 1m ago",
    ]);
  });
});

describe("grouping", () => {
  it("puts every cloud row under Subscriptions even when not featured", () => {
    const grok = row({
      id: "cli:grok",
      provider: "cli:grok",
      name: "Grok · Default",
      featured: false,
    });
    const local = row({
      id: "local:gguf:abc",
      provider: "llamacpp",
      group: "local",
      name: "qwen3:14b · This computer",
      inference: "local",
    });
    const groups = groupTargets([grok, local, row({})]);
    expect(groups.subscriptions.map((t) => t.id)).toEqual([
      "cli:codex:gpt-6-astra",
      "cli:grok",
    ]);
    expect(groups.local.map((t) => t.name)).toEqual([
      "qwen3:14b · This computer",
    ]);
  });

  it("keeps API-key rows out of Subscriptions", () => {
    const qwen = openrouter("qwen/qwen3-coder", "Qwen: Qwen3 Coder");
    // Rows are recognised by group or, failing that, by provider.
    const bare = openrouter("z-ai/glm-4.6", "Z.AI: GLM 4.6", {
      group: "subscriptions",
    });
    const groups = groupTargets([qwen, row({}), bare]);
    expect(groups.subscriptions.map((t) => t.id)).toEqual([
      "cli:codex:gpt-6-astra",
    ]);
    expect(groups.api.map((t) => t.id)).toEqual([
      "api:openrouter:qwen/qwen3-coder",
      "api:openrouter:z-ai/glm-4.6",
    ]);
    expect(groups.local).toEqual([]);
    expect(vendorKey(qwen)).toBe("openrouter");
    expect(vendorLabel(qwen)).toBe("OpenRouter");
  });

  it("collapses OpenRouter to one section and searches name, slug and subtitle", () => {
    const rows = Array.from({ length: 40 }, (_, i) =>
      openrouter(`vendor/model-${i}`, `Vendor: Model ${i}`),
    );
    rows[7] = openrouter("qwen/qwen3-coder", "Qwen: Qwen3 Coder");
    const [section, ...rest] = vendorSections(rows, {
      expanded: [],
      recent: [],
      selected: "",
      searching: false,
    });
    expect(rest).toEqual([]);
    expect(section).toMatchObject({ key: "openrouter", label: "OpenRouter" });
    expect(section.rows.map((r) => r.id)).toEqual([
      "api:openrouter:vendor/model-0",
    ]);
    expect(section.hidden).toBe(39);
    const found = (q: string) =>
      rows.filter((r) => matchesQuery(r, q)).map((r) => r.model);
    expect(found("qwen coder")).toEqual(["qwen/qwen3-coder"]);
    expect(found("qwen3-coder")).toEqual(["qwen/qwen3-coder"]);
    expect(found("openrouter qwen/")).toEqual(["qwen/qwen3-coder"]);
  });

  it("collapses long vendor lists to default, recent and selected rows", () => {
    const cursor = Array.from({ length: 40 }, (_, i) =>
      row({
        id: `cli:cursor:m${i}`,
        provider: "cli:cursor",
        name: `Cursor · Model ${i}`,
        is_default: i === 0,
      }),
    );
    const [section] = vendorSections(cursor, {
      expanded: [],
      recent: ["cli:cursor:m7"],
      selected: "cli:cursor:m9",
      searching: false,
    });
    expect(section.rows.map((r) => r.id)).toEqual([
      "cli:cursor:m0",
      "cli:cursor:m7",
      "cli:cursor:m9",
    ]);
    expect(section.hidden).toBe(37);
    const [open] = vendorSections(cursor, {
      expanded: ["cursor"],
      recent: [],
      selected: "",
      searching: false,
    });
    expect(open.rows).toHaveLength(40);
  });
});

describe("row actions", () => {
  it("never leaves a non-ready row without an explanation or a fix", () => {
    expect(rowAction(row({}))).toEqual({ kind: "select" });
    expect(
      rowAction(row({ provider: "cli:cursor", availability: "sign_in" })),
    ).toEqual({ kind: "connect", vendor: "cursor" });
    expect(
      rowAction(
        row({
          availability: "setup_required",
          reason: "Install the Codex CLI",
        }),
      ),
    ).toMatchObject({
      kind: "setup",
      hint: "Install the Codex CLI",
      local: false,
    });
    expect(
      rowAction(row({ availability: "unavailable", reason: "Offline mode" })),
    ).toEqual({ kind: "explain", reason: "Offline mode" });
    expect(
      availabilityLabel(
        row({ availability: "sign_in", availability_label: "" }),
      ),
    ).toBe("Sign in");
    // An OpenRouter row without a key leads to Accounts › OpenRouter.
    expect(
      rowAction(
        openrouter("qwen/qwen3-coder", "Qwen: Qwen3 Coder", {
          availability: "sign_in",
          availability_label: "Add API key",
        }),
      ),
    ).toEqual({ kind: "connect", vendor: "openrouter" });
    expect(
      rowAction(
        openrouter("qwen/qwen3-coder", "Qwen: Qwen3 Coder", {
          availability: "setup_required",
          reason: "",
        }),
      ),
    ).toMatchObject({
      kind: "setup",
      hint: "Add an API key for OpenRouter in Accounts.",
    });
    // Antigravity's agent server is installed from its Accounts card.
    expect(
      rowAction(
        row({
          provider: "cli:antigravity",
          availability: "setup_required",
          reason: "Install the Antigravity agent from Settings › Accounts",
        }),
      ),
    ).toEqual({
      kind: "setup",
      local: false,
      hint: "Install the Antigravity agent in Settings › Accounts.",
    });
  });
});

it("a picker row does not repeat its availability in the usage", () => {
  expect(
    usageAfterAvailability(
      "Plan limit reached",
      "Plan limit reached · resets in 2h",
    ),
  ).toBe("resets in 2h");
  expect(
    usageAfterAvailability("Plan limit reached", "Plan limit reached"),
  ).toBe("");
  expect(
    usageAfterAvailability(
      "Ready",
      "Shared plan usage · 2% left · resets in 3h",
    ),
  ).toBe("Shared plan usage · 2% left · resets in 3h");
});
