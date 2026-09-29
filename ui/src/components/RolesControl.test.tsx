import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import type { RolesView, RoleTarget } from "../api";
import { RolesControl } from "./RolesControl";
import { ComposerMoreOptions } from "./ComposerMoreOptions";

afterEach(cleanup);

const target = (over: Partial<RoleTarget>): RoleTarget => ({
  role: "plan",
  label: "Plan",
  setting: "",
  name: "Qwen3 14B",
  local: true,
  runner: "shadowcode",
  cost: "local",
  ...over,
});

const view = (pipeline: boolean, over: Partial<RolesView> = {}): RolesView => ({
  workspace: "/p",
  setup: {
    pipeline,
    plan: "cli:claude",
    implement: "",
    review: "",
    explore: "",
    preset: "claude-plans",
  },
  roles: {
    plan: target({
      role: "plan",
      name: "Claude Code",
      local: false,
      runner: "vendor",
      cost: "subscription",
      needs_consent: true,
    }),
    implement: target({ role: "implement" }),
    review: target({ role: "review", skipped: true }),
    explore: target({ role: "explore" }),
  },
  presets: [
    {
      id: "claude-plans",
      label: "Claude Code plans",
      description: "Claude Code writes the plan.",
      roles: { plan: "cli:claude", implement: "", review: "skip", explore: "" },
    },
    {
      id: "all-local",
      label: "Everything on this computer",
      description: "One local model.",
      roles: {
        plan: "local",
        implement: "local",
        review: "local",
        explore: "",
      },
    },
  ],
  conversation: { id: "local:gguf:q", name: "Qwen3 14B", local: true },
  offline: false,
  consented: [],
  ...over,
});

it("turns roles on, picks a preset and opens Settings", () => {
  const onToggle = vi.fn();
  const onPreset = vi.fn();
  const onOpenSettings = vi.fn();
  const { rerender } = render(
    <RolesControl
      view={view(false)}
      saving={false}
      mode="code"
      onToggle={onToggle}
      onPreset={onPreset}
      onOpenSettings={onOpenSettings}
    />,
  );
  expect(
    screen.getByText(/Off: each message runs on the model you picked/),
  ).toBeTruthy();
  const toggle = screen.getByRole("switch", {
    name: /Plan → Implement → Review/,
  });
  fireEvent.click(toggle);
  expect(onToggle).toHaveBeenCalledWith(true);
  fireEvent.change(screen.getByLabelText("Roles preset"), {
    target: { value: "all-local" },
  });
  expect(onPreset).toHaveBeenCalledWith("all-local");
  fireEvent.click(
    screen.getByRole("button", { name: /Choose a model for each role/ }),
  );
  expect(onOpenSettings).toHaveBeenCalled();
  rerender(
    <RolesControl
      view={view(true)}
      saving={false}
      mode="ask"
      onToggle={onToggle}
      onPreset={onPreset}
      onOpenSettings={onOpenSettings}
    />,
  );
  expect(
    screen.getByText("Plan: Claude Code · Implement: Qwen3 14B"),
  ).toBeTruthy();
  // A cloud role in a conversation on this computer asks first.
  expect(screen.getByText(/Claude Code runs in the cloud/)).toBeTruthy();
  expect(screen.getByText(/Ask answers on the model you picked/)).toBeTruthy();
});

it("says why roles cannot run", () => {
  const blocked = view(true);
  blocked.roles.plan = {
    ...blocked.roles.plan,
    needs_consent: false,
    blocked: "Offline mode: the plan role uses Claude Code.",
  };
  render(
    <RolesControl
      view={blocked}
      saving={false}
      mode="code"
      onToggle={() => {}}
      onPreset={() => {}}
      onOpenSettings={() => {}}
    />,
  );
  expect(
    screen.getByText("Offline mode: the plan role uses Claude Code."),
  ).toBeTruthy();
});

it("marks the More trigger while roles are on", () => {
  const { container } = render(
    <ComposerMoreOptions indicator="High" roles>
      <button type="button">Compare</button>
    </ComposerMoreOptions>,
  );
  const summary = container.querySelector("summary")!;
  expect(summary.textContent).toContain("Roles");
  expect(summary.getAttribute("aria-label")).toBe(
    "More task options, reasoning effort High, Plan → Implement → Review on",
  );
});
