import { useState } from "react";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { UnifiedPicker } from "./UnifiedPicker";
import type { PickerTarget } from "../lib/picker";

afterEach(() => cleanup());
beforeEach(() => localStorage.clear());

const now = Date.now() / 1000;
const codex: PickerTarget = {
  id: "cli:codex:gpt-6-astra",
  provider: "cli:codex",
  group: "subscriptions",
  name: "Codex · GPT-6-Astra",
  inference: "cloud",
  availability: "ready",
  availability_label: "Ready",
  vision: true,
  tools: true,
  is_default: true,
  usage: {
    state: "ok",
    label: "Shared plan usage · 2% left",
    windows: [
      {
        label: "Weekly",
        used_percent: 98,
        remaining_percent: 2,
        window_minutes: 10080,
        resets_at: now + 2 * 3600 + 120,
      },
    ],
    last_refresh: now - 120,
    provider_usage_url: "https://chatgpt.com/codex/settings/usage",
  },
};
const cursor: PickerTarget = {
  id: "cli:cursor:auto",
  provider: "cli:cursor",
  group: "subscriptions",
  name: "Cursor · Auto",
  inference: "cloud",
  availability: "sign_in",
  availability_label: "Sign in",
  reason: "Not signed in",
  vision: false,
  tools: true,
  usage: {
    state: "unavailable",
    label: "Usage unavailable · Open provider usage",
  },
};
const grok: PickerTarget = {
  ...cursor,
  id: "cli:grok",
  provider: "cli:grok",
  name: "Grok · Default",
  featured: false,
  availability: "unavailable",
  availability_label: "Unavailable",
  reason: "Offline mode: cloud rows are off",
};
const qwen: PickerTarget = {
  id: "local:gguf:1",
  provider: "llamacpp",
  group: "local",
  name: "qwen3:14b · This computer",
  inference: "local",
  availability: "ready",
  availability_label: "Ready",
  vision: false,
  tools: true,
  usage: {
    state: "local",
    label: "Runs on this computer · No subscription quota",
  },
};
const gptoss: PickerTarget = {
  ...qwen,
  id: "local:gguf:2",
  name: "gpt-oss:20b · This computer",
  availability: "setup_required",
  availability_label: "Setup required",
  reason: "unknown model architecture: gptoss",
  tools: false,
};

function Harness({
  targets = [codex, cursor, grok, qwen, gptoss],
  initial = "",
  onConnect = vi.fn(),
  onSetup = vi.fn(),
  onSelect = vi.fn(),
  onRefresh,
}: {
  targets?: PickerTarget[];
  initial?: string;
  onConnect?: (vendor?: string) => void;
  onSetup?: (target: PickerTarget) => void;
  onSelect?: (id: string) => void;
  onRefresh?: () => Promise<void>;
}) {
  const [value, setValue] = useState(initial);
  const [open, setOpen] = useState(false);
  return (
    <UnifiedPicker
      targets={targets}
      value={value}
      open={open}
      onOpenChange={setOpen}
      onSelect={(id) => {
        setValue(id);
        onSelect(id);
      }}
      onConnect={onConnect}
      onSetup={onSetup}
      onRefresh={onRefresh}
      onAddLocal={vi.fn()}
    />
  );
}

const trigger = () =>
  screen.getByRole("button", { name: /Model for this task/ });
const search = () => screen.getByRole("combobox", { name: "Search models" });
const key = (k: string) => fireEvent.keyDown(search(), { key: k });
const activeOption = () => {
  const id = search().getAttribute("aria-activedescendant");
  return id ? document.getElementById(id) : null;
};

it.each([
  [
    "unknown",
    "Billing unverified",
    "Billing unverified · API charges may apply",
  ],
  ["api_key", "API key", "API key login · billed per token"],
])(
  "shows explicit %s vendor billing without changing the selected route",
  (billing, badge, warning) => {
    const target = {
      ...codex,
      billing: billing as "unknown" | "api_key",
      usage: { state: "unavailable", label: "Usage unavailable", detail: [] },
    };
    const onSelect = vi.fn();
    render(
      <Harness targets={[target]} initial={target.id} onSelect={onSelect} />,
    );
    expect(trigger().textContent).toContain(badge);
    expect(trigger().getAttribute("aria-label")).toContain(warning);
    fireEvent.click(trigger());
    const group = screen.getByRole("group", { name: "Vendor CLIs" });
    const row = within(group).getByRole("option", {
      name: /Codex · GPT-6-Astra/,
    });
    expect(row.textContent).toContain(warning);
    expect(within(group).queryByText("Subscriptions")).toBeNull();
    fireEvent.change(search(), { target: { value: "subscription" } });
    expect(screen.queryByRole("option", { name: /Codex/ })).toBeNull();
    fireEvent.change(search(), { target: { value: "codex" } });
    key("Enter");
    expect(onSelect).toHaveBeenCalledWith(target.id);
  },
);

it.each([
  ["subscription", "Subscriptions"],
  ["unknown", "Vendor CLIs"],
  ["api_key", "Vendor CLIs"],
] as const)(
  "derives the mixed vendor heading from %s billing, excluding local and API rows",
  (billing, heading) => {
    const api: PickerTarget = {
      ...codex,
      id: "openrouter:test",
      provider: "openrouter",
      group: "api",
      name: "API test",
      billing: "api_key",
    };
    render(
      <Harness
        targets={[
          { ...codex, billing },
          { ...cursor, billing: "subscription" },
          { ...qwen, billing: "unknown" },
          api,
        ]}
      />,
    );
    fireEvent.click(trigger());
    expect(
      [...document.querySelectorAll(".unified-picker-heading")].map(
        (node) => node.textContent,
      ),
    ).toEqual([heading, "On this computer", "API keys"]);
    const vendorGroup = screen.getByRole("group", { name: heading });
    expect(
      within(vendorGroup).getByRole("option", { name: /Codex · GPT-6-Astra/ }),
    ).toBeTruthy();
    expect(
      within(vendorGroup).getByRole("option", { name: /Cursor · Auto/ }),
    ).toBeTruthy();
  },
);

it("shows 'Choose a model' until a row is chosen and groups both sources", () => {
  render(<Harness />);
  expect(trigger().textContent).toContain("Choose a model");
  fireEvent.click(trigger());
  const subs = screen.getByRole("group", { name: "Subscriptions" });
  const local = screen.getByRole("group", { name: "On this computer" });
  expect(
    within(subs).getByRole("option", { name: /Codex · GPT-6-Astra/ }),
  ).toBeTruthy();
  // featured:false still lands in Subscriptions.
  expect(
    within(subs).getByRole("option", { name: /Grok · Default/ }),
  ).toBeTruthy();
  expect(
    within(local).getByRole("option", { name: /qwen3:14b · This computer/ }),
  ).toBeTruthy();
  // Each row: Local/Cloud, availability, usage.
  const text = (name: RegExp) =>
    screen.getByRole("option", { name }).textContent || "";
  expect(text(/Codex/)).toMatch(/Cloud · Ready · Shared plan usage · 2% left/);
  expect(text(/qwen3/)).toMatch(
    /Local · Ready · Runs on this computer · No subscription quota/,
  );
  // Unknown usage is not repeated on every row; it stays in the details.
  expect(text(/Cursor/)).toMatch(/Cloud · Sign in$/);
  expect(text(/Cursor/)).not.toMatch(/%/);
  expect(text(/gpt-oss/)).toMatch(/Local · Setup required/);
  // Capability badges only when verified.
  expect(screen.getAllByText("Vision")).toHaveLength(1);
  expect(screen.getAllByText("Chat only")).toHaveLength(1);
  // Non-ready rows are options, not disabled buttons.
  const blocked = screen.getByRole("option", { name: /Cursor · Auto/ });
  expect(blocked.tagName).not.toBe("BUTTON");
  expect(document.body.textContent).not.toMatch(/72% remaining|\$12,430/);
});

it("refreshes an open picker once and restores search focus", async () => {
  let finishRefresh!: () => void;
  const onRefresh = vi.fn(
    () => new Promise<void>((resolve) => (finishRefresh = resolve)),
  );
  render(<Harness onRefresh={onRefresh} />);
  fireEvent.click(trigger());
  await waitFor(() => expect(document.activeElement).toBe(search()));
  fireEvent.click(screen.getByRole("button", { name: "Refresh models" }));
  expect(onRefresh).toHaveBeenCalledTimes(1);
  expect(
    screen
      .getByRole("button", { name: "Checking models…" })
      .hasAttribute("disabled"),
  ).toBe(true);
  finishRefresh();
  await waitFor(() =>
    expect(screen.getByRole("button", { name: "Refresh models" })).toBeTruthy(),
  );
  await waitFor(() => expect(document.activeElement).toBe(search()));
  expect(screen.getByRole("listbox")).toBeTruthy();
});

it("selects with the keyboard and restores focus to the trigger", async () => {
  const onSelect = vi.fn();
  render(<Harness onSelect={onSelect} />);
  fireEvent.click(trigger());
  await waitFor(() => expect(document.activeElement).toBe(search()));
  expect(activeOption()?.textContent).toContain("Codex · GPT-6-Astra");
  key("End");
  // The API keys group comes last; with no rows it offers to add a key.
  expect(activeOption()?.textContent).toBe("Add an OpenRouter API key…");
  key("Home");
  expect(activeOption()?.textContent).toContain("Codex");
  key("ArrowDown");
  key("ArrowDown");
  key("ArrowDown");
  expect(activeOption()?.textContent).toContain("qwen3:14b");
  key("ArrowUp");
  key("ArrowDown");
  key("Enter");
  expect(onSelect).toHaveBeenCalledWith("local:gguf:1");
  expect(screen.queryByRole("listbox")).toBeNull();
  await waitFor(() => expect(document.activeElement).toBe(trigger()));
  expect(trigger().textContent).toMatch(/^Local/);
  // The Local badge replaces the " · This computer" suffix in the trigger.
  expect(trigger().textContent).toBe("Localqwen3:14b");
  expect(trigger().getAttribute("aria-label")).toContain(
    "qwen3:14b · This computer",
  );
});

it("filters by typing and closes with Escape", async () => {
  render(<Harness />);
  fireEvent.click(trigger());
  fireEvent.change(search(), { target: { value: "qwen" } });
  expect(screen.getAllByRole("option")).toHaveLength(1);
  expect(screen.getByText("No subscription matches")).toBeTruthy();
  key("Escape");
  expect(screen.queryByRole("listbox")).toBeNull();
  await waitFor(() => expect(document.activeElement).toBe(trigger()));
});

it("routes non-ready rows to their fix instead of disabling them", () => {
  const onConnect = vi.fn();
  const onSetup = vi.fn();
  render(<Harness onConnect={onConnect} onSetup={onSetup} />);
  fireEvent.click(trigger());
  // Sign in → Accounts › Connect for that vendor.
  fireEvent.click(screen.getByRole("option", { name: /Cursor · Auto/ }));
  expect(onConnect).toHaveBeenCalledWith("cursor");

  fireEvent.click(trigger());
  // Unavailable → the reason is shown.
  fireEvent.click(screen.getByRole("option", { name: /Grok · Default/ }));
  expect(
    screen.getAllByText(/Offline mode: cloud rows are off/).length,
  ).toBeGreaterThan(0);
  // Setup required → the setup hint, then the place that fixes it.
  const setup = screen.getByRole("option", { name: /gpt-oss:20b/ });
  fireEvent.click(setup);
  expect(
    screen.getAllByText(/unknown model architecture: gptoss/).length,
  ).toBeGreaterThan(0);
  fireEvent.click(screen.getByRole("button", { name: "Open Local models" }));
  expect(onSetup).toHaveBeenCalledWith(
    expect.objectContaining({ id: "local:gguf:2" }),
  );
});

it("opens usage details from the keyboard with windows and reset times", () => {
  render(<Harness />);
  fireEvent.click(trigger());
  key("ArrowRight");
  const details = document.querySelector(".unified-picker-details")!;
  expect(details.textContent).toContain("Weekly · 2% left · resets in 2h");
  expect(details.textContent).toContain("Last checked 2m ago");
  expect(
    within(details as HTMLElement).getByRole("link", {
      name: "Open Codex usage",
    }),
  ).toBeTruthy();
  expect(activeOption()?.getAttribute("aria-describedby")).toContain("details");
  key("ArrowLeft");
  expect(document.querySelector(".unified-picker-details")).toBeNull();
});

it("collapses a vendor with many models and expands on request", () => {
  const many = Array.from({ length: 40 }, (_, i) => ({
    ...codex,
    id: `cli:cursor:m${i}`,
    provider: "cli:cursor",
    name: i === 0 ? "Cursor · Auto" : `Cursor · Model ${i}`,
    is_default: i === 0,
  }));
  render(<Harness targets={many} />);
  fireEvent.click(trigger());
  expect(screen.getAllByRole("option", { name: /^Cursor/ })).toHaveLength(1);
  const more = screen.getByRole("option", {
    name: "Show all 40 Cursor models",
  });
  fireEvent.click(more);
  expect(screen.getAllByRole("option", { name: /^Cursor/ })).toHaveLength(40);
});

const openrouterRow = (
  i: number,
  over: Partial<PickerTarget> = {},
): PickerTarget => ({
  id: `api:openrouter:vendor/model-${i}`,
  provider: "openrouter",
  account: "",
  model: `vendor/model-${i}`,
  route: "native",
  group: "api",
  name: `Vendor: Model ${i}`,
  subtitle: `OpenRouter · vendor/model-${i}`,
  inference: "cloud",
  availability: "ready",
  availability_label: "Ready",
  reason: "",
  featured: false,
  vision: false,
  tools: true,
  is_default: false,
  usage: {
    state: "api_key",
    label: "API key · $0.10/M in · $0.40/M out",
    detail: ["Billed per token to your OpenRouter key"],
    provider_usage_url: "https://openrouter.ai/activity",
  },
  ...over,
});
const qwenCoder = openrouterRow(0, {
  id: "api:openrouter:qwen/qwen3-coder",
  model: "qwen/qwen3-coder",
  name: "Qwen: Qwen3 Coder",
  subtitle: "OpenRouter · qwen/qwen3-coder",
  usage: {
    state: "api_key",
    label: "API key · $0.30/M in · $1.20/M out",
    detail: ["Input $0.30/M tokens · output $1.20/M tokens"],
    provider_usage_url: "https://openrouter.ai/activity",
  },
});
const openrouterRows = [
  qwenCoder,
  ...Array.from({ length: 39 }, (_, i) =>
    openrouterRow(i + 1, { tools: i % 4 !== 0 }),
  ),
];

it("shows OpenRouter rows in their own API keys group, collapsed", () => {
  render(<Harness targets={[codex, ...openrouterRows, qwen]} />);
  fireEvent.click(trigger());
  const groups = screen.getAllByRole("group");
  expect(groups.map((g) => g.getAttribute("aria-labelledby"))).toHaveLength(3);
  const api = screen.getByRole("group", { name: "API keys" });
  // After On this computer (free local rows win a shared search), with the
  // billing note.
  expect(groups[2]).toBe(api);
  expect(api.getAttribute("aria-describedby")).toBeTruthy();
  expect(
    document.getElementById(api.getAttribute("aria-describedby")!)?.textContent,
  ).toBe("Billed per token by the provider");
  const subs = screen.getByRole("group", { name: "Subscriptions" });
  expect(within(subs).queryByRole("option", { name: /Qwen/ })).toBeNull();
  // Collapsed: the first row plus "Show all".
  expect(within(api).getAllByRole("option")).toHaveLength(2);
  const row = within(api).getByRole("option", { name: /Qwen: Qwen3 Coder/ });
  expect(row.textContent).toMatch(
    /Cloud · Ready · API key · \$0\.30\/M in · \$1\.20\/M out/,
  );
  expect(row.textContent).not.toMatch(/subscription|plan/i);
  fireEvent.click(
    within(api).getByRole("option", { name: "Show all 40 OpenRouter models" }),
  );
  expect(within(api).getAllByRole("option")).toHaveLength(40);
  // tools:false rows carry the Chat only badge.
  expect(within(api).getAllByText("Chat only")).toHaveLength(10);
});

it("finds an OpenRouter model by name words or slug and picks it", async () => {
  const onSelect = vi.fn();
  render(
    <Harness targets={[codex, ...openrouterRows, qwen]} onSelect={onSelect} />,
  );
  fireEvent.click(trigger());
  fireEvent.change(search(), { target: { value: "qwen coder" } });
  expect(
    screen
      .getAllByRole("option")
      .map((o) => o.querySelector("strong")?.textContent),
  ).toEqual(["Qwen: Qwen3 Coder"]);
  fireEvent.change(search(), { target: { value: "vendor/model-17" } });
  expect(screen.getAllByRole("option")).toHaveLength(1);
  expect(screen.getByText("No subscription matches")).toBeTruthy();
  fireEvent.change(search(), { target: { value: "qwen3-coder" } });
  key("Enter");
  expect(onSelect).toHaveBeenCalledWith("api:openrouter:qwen/qwen3-coder");
  await waitFor(() => expect(document.activeElement).toBe(trigger()));
  // The trigger says it is API-key usage, not a subscription.
  expect(trigger().textContent).toBe("API keyQwen: Qwen3 Coder");
});

it("sends a keyless OpenRouter row to Accounts and explains it in details", () => {
  const onConnect = vi.fn();
  const keyless = openrouterRows.map((t) => ({
    ...t,
    availability: "sign_in",
    availability_label: "Add API key",
    reason: "Add an OpenRouter API key in Accounts",
  }));
  render(<Harness targets={[codex, ...keyless]} onConnect={onConnect} />);
  fireEvent.click(trigger());
  const row = screen.getByRole("option", { name: /Qwen: Qwen3 Coder/ });
  expect(row.textContent).toContain("Cloud · Add API key");
  // Details from the keyboard: price lines and the activity link.
  key("ArrowDown");
  expect(activeOption()).toBe(row);
  key("ArrowRight");
  const details = document.querySelector<HTMLElement>(
    ".unified-picker-details",
  )!;
  expect(details.textContent).toContain("OpenRouter · qwen/qwen3-coder");
  expect(details.textContent).toContain("API key · $0.30/M in · $1.20/M out");
  expect(details.textContent).toContain(
    "Input $0.30/M tokens · output $1.20/M tokens",
  );
  expect(
    within(details)
      .getByRole("link", { name: "Open OpenRouter activity" })
      .getAttribute("href"),
  ).toBe("https://openrouter.ai/activity");
  fireEvent.click(
    within(details).getByRole("button", { name: "Add OpenRouter API key" }),
  );
  expect(onConnect).toHaveBeenLastCalledWith("openrouter");
  onConnect.mockClear();
  fireEvent.click(trigger());
  fireEvent.click(screen.getByRole("option", { name: /Qwen: Qwen3 Coder/ }));
  expect(onConnect).toHaveBeenCalledWith("openrouter");
});

it("offers to add an OpenRouter key when there are no API-key rows", () => {
  const onConnect = vi.fn();
  render(<Harness targets={[codex, qwen]} onConnect={onConnect} />);
  fireEvent.click(trigger());
  const api = screen.getByRole("group", { name: "API keys" });
  const add = within(api).getByRole("option", {
    name: "Add an OpenRouter API key…",
  });
  expect(add.getAttribute("aria-selected")).toBe("false");
  key("ArrowDown");
  key("ArrowDown");
  expect(activeOption()).toBe(add);
  key("Enter");
  expect(onConnect).toHaveBeenCalledWith("openrouter");
  expect(screen.queryByRole("listbox")).toBeNull();
  // While searching the group just reports no match.
  fireEvent.click(trigger());
  fireEvent.change(search(), { target: { value: "qwen" } });
  expect(screen.getByText("No API-key model matches")).toBeTruthy();
  expect(screen.queryByText("Add an OpenRouter API key…")).toBeNull();
});

it("sends an Antigravity row that needs its agent to Accounts with the install hint", () => {
  const antigravity: PickerTarget = {
    ...cursor,
    id: "cli:antigravity",
    provider: "cli:antigravity",
    name: "Antigravity · Default",
    availability: "setup_required",
    availability_label: "Setup required",
    reason:
      "Install the Antigravity agent from Settings › Accounts (Google's official ACP server, a 334 MB download from dl.google.com).",
  };
  const onSetup = vi.fn();
  const onConnect = vi.fn();
  render(
    <Harness
      targets={[codex, antigravity, qwen]}
      onSetup={onSetup}
      onConnect={onConnect}
    />,
  );
  fireEvent.click(trigger());
  fireEvent.click(
    screen.getByRole("option", { name: /Antigravity · Default/ }),
  );
  expect(
    screen.getByText("Install the Antigravity agent in Settings › Accounts."),
  ).toBeTruthy();
  expect(screen.queryByText(/Run agy/i)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Open Accounts" }));
  expect(onSetup).toHaveBeenCalledWith(
    expect.objectContaining({ id: "cli:antigravity" }),
  );
  expect(onConnect).not.toHaveBeenCalled();
});
