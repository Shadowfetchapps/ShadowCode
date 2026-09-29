import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { RolesView } from "../../api";
import type { PickerTarget } from "../../lib/picker";

const mocks = vi.hoisted(() => ({ roles: vi.fn(), saveRoles: vi.fn() }));
vi.mock("../../api", () => ({ api: mocks }));

import { RolesPage } from "./RolesPage";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

const view = (over: Partial<RolesView["setup"]> = {}): RolesView => ({
  workspace: "/home/u/project",
  setup: {
    pipeline: false,
    plan: "",
    implement: "",
    review: "",
    explore: "",
    preset: "",
    ...over,
  },
  roles: {
    plan: {
      role: "plan",
      label: "Plan",
      setting: over.plan || "",
      name: over.plan ? "Claude Code" : "Qwen3 14B",
      local: !over.plan,
      runner: over.plan ? "vendor" : "shadowcode",
      cost: over.plan ? "subscription" : "local",
      needs_consent: Boolean(over.plan),
    },
    implement: {
      role: "implement",
      label: "Implement",
      setting: "",
      name: "Qwen3 14B",
      local: true,
      runner: "shadowcode",
      cost: "local",
    },
    review: {
      role: "review",
      label: "Review",
      setting: "",
      skipped: over.review === "skip",
      name: "Qwen3 14B",
      local: true,
      runner: "shadowcode",
      cost: "local",
    },
    explore: {
      role: "explore",
      label: "Explore",
      setting: "",
      name: "Qwen3 14B",
      local: true,
      runner: "shadowcode",
      cost: "local",
    },
  },
  presets: [
    {
      id: "claude-plans",
      label: "Claude Code plans",
      description: "Claude Code writes the plan.",
      roles: { plan: "cli:claude", implement: "", review: "skip", explore: "" },
    },
  ],
  conversation: { id: "local:gguf:q", name: "Qwen3 14B", local: true },
  offline: false,
  consented: [],
});

const targets: PickerTarget[] = [
  {
    id: "cli:claude",
    provider: "cli:claude",
    group: "subscriptions",
    name: "Claude Code · Default",
    inference: "cloud",
    availability: "ready",
  },
  {
    id: "cli:codex",
    provider: "cli:codex",
    group: "subscriptions",
    name: "Codex · Default",
    inference: "cloud",
    availability: "sign_in",
    availability_label: "Sign in",
  },
  {
    id: "local:gguf:q",
    provider: "llamacpp",
    group: "local",
    name: "Qwen3 14B · This computer",
    inference: "local",
    availability: "ready",
  },
];

it("chooses a model per role, applies presets and turns the pipeline on", async () => {
  mocks.roles.mockResolvedValue(view());
  mocks.saveRoles.mockImplementation(async (change: Record<string, unknown>) =>
    view({
      pipeline: change.pipeline === true,
      plan: change.plan === "cli:claude" || change.preset ? "cli:claude" : "",
      review: change.preset ? "skip" : "",
      preset: change.preset ? String(change.preset) : "",
    }),
  );
  const onChanged = vi.fn();
  render(
    <RolesPage
      workspace="/home/u/project"
      sessionId="s1"
      model="local:gguf:q"
      targets={targets}
      onChanged={onChanged}
      onToast={() => {}}
    />,
  );
  const plan = (await screen.findByLabelText(/^Plan/)) as HTMLSelectElement;
  expect(mocks.roles).toHaveBeenCalledWith(
    "/home/u/project",
    "s1",
    "local:gguf:q",
  );
  const options = [...plan.options].map((o) => o.textContent);
  expect(options).toEqual([
    "Conversation model (Qwen3 14B)",
    "Skip this step",
    "Claude Code · Default",
    "Codex · Default (Sign in)",
    "Qwen3 14B · This computer",
  ]);
  // The implement role cannot be skipped.
  const implement = screen.getByLabelText(/^Implement/) as HTMLSelectElement;
  expect([...implement.options].some((o) => o.value === "skip")).toBe(false);
  fireEvent.change(plan, { target: { value: "cli:claude" } });
  await waitFor(() =>
    expect(mocks.saveRoles).toHaveBeenCalledWith({
      workspace: "/home/u/project",
      session_id: "s1",
      model: "local:gguf:q",
      plan: "cli:claude",
    }),
  );
  await waitFor(() => expect(onChanged).toHaveBeenCalled());
  expect(
    await screen.findByText(
      /Runs in the cloud: ShadowCode asks before this conversation/,
    ),
  ).toBeTruthy();
  expect(
    screen.getByText("Claude Code · vendor CLI · cloud · Subscription"),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Claude Code plans/ }));
  await waitFor(() =>
    expect(mocks.saveRoles).toHaveBeenLastCalledWith(
      expect.objectContaining({ preset: "claude-plans" }),
    ),
  );
  await waitFor(() =>
    expect(
      screen
        .getByRole("button", { name: /Claude Code plans/ })
        .getAttribute("aria-pressed"),
    ).toBe("true"),
  );
  fireEvent.click(
    screen.getByRole("switch", { name: /Plan → Implement → Review/ }),
  );
  await waitFor(() =>
    expect(mocks.saveRoles).toHaveBeenLastCalledWith(
      expect.objectContaining({ pipeline: true }),
    ),
  );
});
