import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { useState } from "react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { CompareDialog } from "./CompareDialog";
import type { PickerTarget } from "../lib/picker";

afterEach(() => cleanup());

const row = (over: Partial<PickerTarget>): PickerTarget => ({
  id: "x",
  provider: "cli:codex",
  group: "subscriptions",
  name: "X",
  inference: "cloud",
  availability: "ready",
  availability_label: "Ready",
  ...over,
});
const targets: PickerTarget[] = [
  row({
    id: "cli:codex:astra",
    name: "Codex · GPT-6-Astra",
    usage: { state: "ok", label: "Shared plan usage · 2% left" },
  }),
  row({
    id: "cli:claude",
    name: "Claude Code · Default",
    availability: "sign_in",
    availability_label: "Sign in",
    reason: "Not signed in",
  }),
  row({
    id: "api:openrouter:qwen/qwen3-coder",
    provider: "openrouter",
    group: "api",
    name: "Qwen: Qwen3 Coder",
    usage: {
      state: "api_key",
      label: "API key · $0.30/M in · $1.20/M out",
    },
  }),
  row({
    id: "local:gguf:qwen",
    provider: "llamacpp",
    group: "local",
    inference: "local",
    name: "qwen3:14b · This computer",
    usage: { state: "local", label: "Runs on this computer" },
  }),
  row({
    id: "local:gguf:gemma",
    provider: "llamacpp",
    group: "local",
    inference: "local",
    name: "gemma-4:12b · This computer",
  }),
];

function Harness({
  initial = ["", ""],
  onStart = vi.fn(async () => undefined),
  onOpenAllowance = vi.fn(),
  uncommitted = 0,
  unsavedFiles = [],
}: {
  initial?: string[];
  onStart?: (models: string[]) => Promise<void>;
  onOpenAllowance?: () => void;
  uncommitted?: number;
  unsavedFiles?: string[];
}) {
  const [models, setModels] = useState(initial);
  return (
    <CompareDialog
      task="Fix the add function"
      targets={targets}
      models={models}
      onModels={setModels}
      uncommitted={uncommitted}
      unsavedFiles={unsavedFiles}
      onStart={onStart}
      onClose={() => undefined}
      onOpenAllowance={onOpenAllowance}
      onConnect={() => undefined}
      onSetup={() => undefined}
      onAddLocal={() => undefined}
    />
  );
}

const dialog = () => screen.getByRole("dialog", { name: "Compare models" });
const startButton = () =>
  within(dialog()).getByRole("button", { name: "Start comparison" });

async function choose(slot: number, name: RegExp) {
  fireEvent.click(
    within(dialog()).getByRole("button", {
      name: new RegExp(`^Model ${slot}:`),
    }),
  );
  fireEvent.click(await screen.findByRole("option", { name }));
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
}

describe("CompareDialog", () => {
  it("blocks a comparison while an open editor draft is missing from the disk snapshot", async () => {
    const onStart = vi.fn(async () => undefined);
    render(
      <Harness
        initial={["cli:codex:astra", "api:openrouter:qwen/qwen3-coder"]}
        onStart={onStart}
        unsavedFiles={["src/main.ts", "README.md"]}
      />,
    );

    expect(
      screen.getByText(/Save or discard these open drafts/).textContent,
    ).toContain("src/main.ts, README.md");
    expect(startButton().getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(startButton());
    expect(onStart).not.toHaveBeenCalled();
  });

  it("shows the task, the cost note and needs two ready models", async () => {
    const onStart = vi.fn(async () => undefined);
    render(<Harness onStart={onStart} />);
    expect(within(dialog()).getByText("Fix the add function")).toBeTruthy();
    expect(dialog().textContent).toContain(
      "Each model uses its own plan allowance or OpenRouter credit.",
    );
    expect(dialog().textContent).toContain(
      "Every model starts from your latest commit.",
    );
    expect(startButton().getAttribute("aria-disabled")).toBe("true");
    expect(startButton().getAttribute("title")).toBe("Choose a model.");
    fireEvent.click(startButton());
    expect(onStart).not.toHaveBeenCalled();
    expect(within(dialog()).getByRole("alert").textContent).toBe(
      "Choose a model.",
    );

    await choose(1, /Codex · GPT-6-Astra/);
    await choose(2, /Qwen: Qwen3 Coder/);
    expect(startButton().getAttribute("aria-disabled")).toBe("false");
    // Each chosen row's usage: its own allowance or per-token price.
    const cost = within(dialog()).getByRole("region", { name: "Cost" });
    expect(cost.textContent).toContain("Shared plan usage · 2% left");
    expect(cost.textContent).toContain("API key · $0.30/M in · $1.20/M out");
    expect(
      within(dialog()).getByRole("button", { name: /^Model 2: / }).textContent,
    ).toContain("API key");
    fireEvent.click(startButton());
    await waitFor(() =>
      expect(onStart).toHaveBeenCalledWith([
        "cli:codex:astra",
        "api:openrouter:qwen/qwen3-coder",
      ]),
    );
  });

  it("allows two local models and explains sequential execution", async () => {
    render(<Harness />);
    await choose(1, /qwen3:14b · This computer/);
    await choose(2, /gemma-4:12b/);
    expect(startButton().getAttribute("aria-disabled")).not.toBe("true");
    expect(dialog().textContent).toContain("Local models run one at a time");
  });

  it("accepts an existing local lineup while rejecting stale rows", () => {
    render(<Harness initial={["local:gguf:qwen", "local:gguf:gemma"]} />);
    expect(startButton().getAttribute("aria-disabled")).not.toBe("true");
    cleanup();
    render(<Harness initial={["cli:codex:astra", "cli:claude"]} />);
    expect(startButton().getAttribute("title")).toBe(
      "Claude Code · Default: Sign in · Not signed in",
    );
  });

  it("adds and removes a third model", async () => {
    render(<Harness initial={["cli:codex:astra", "local:gguf:qwen"]} />);
    expect(dialog().textContent).toContain("about two times");
    fireEvent.click(
      within(dialog()).getByRole("button", { name: /Add a third model/ }),
    );
    expect(
      within(dialog()).getByRole("button", { name: /^Model 3:/ }),
    ).toBeTruthy();
    expect(dialog().textContent).toContain("about three times");
    expect(startButton().getAttribute("aria-disabled")).toBe("true");
    expect(
      within(dialog()).queryByRole("button", { name: /Add a third model/ }),
    ).toBeNull();
    fireEvent.click(
      within(dialog()).getByRole("button", { name: "Remove model 3" }),
    );
    expect(
      within(dialog()).queryByRole("button", { name: /^Model 3:/ }),
    ).toBeNull();
    expect(startButton().getAttribute("aria-disabled")).toBe("false");
  });

  it("says uncommitted work is included, links Allowance and shows start errors inline", async () => {
    const onOpenAllowance = vi.fn();
    const onStart = vi.fn(async () => {
      throw new Error("Open the repository root before comparing models");
    });
    render(
      <Harness
        initial={["cli:codex:astra", "local:gguf:qwen"]}
        uncommitted={3}
        onStart={onStart}
        onOpenAllowance={onOpenAllowance}
      />,
    );
    expect(dialog().textContent).toContain(
      "Your 3 uncommitted changes are included",
    );
    fireEvent.click(
      within(dialog()).getByRole("button", { name: "Open Allowance" }),
    );
    expect(onOpenAllowance).toHaveBeenCalled();
    fireEvent.click(startButton());
    const alert = await within(dialog()).findByRole("alert");
    expect(alert.textContent).toBe(
      "Open the repository root before comparing models",
    );
    expect(dialog()).toBeTruthy();
  });
});
