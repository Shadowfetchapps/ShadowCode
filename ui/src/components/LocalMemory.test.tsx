import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { EventRow } from "../api";
import { replay } from "../lib/transcript";
import { LocalMemoryItem } from "./LocalMemory";
import type { ChatItem } from "./cards";

afterEach(() => cleanup());

const event = (
  id: number,
  type: string,
  payload: Record<string, unknown>,
  task_id = "a",
): EventRow => ({ id, ts: id, type, payload, task_id });

const summary =
  "Not enough free GPU memory to load Qwen3 14B with a 32,768-token context. Nothing in your project was changed. Try a smaller context (16,384 tokens), close other programs that use the GPU, or choose a smaller model.";

const outOfMemory = [
  event(1, "user.message", { text: "Explain main.rs" }),
  event(2, "local.runtime_progress", { phase: "preparing" }),
  event(3, "local.runtime_progress", { phase: "loading" }),
  event(4, "agent.completed", {
    success: false,
    cancelled: false,
    summary,
    local_out_of_memory: {
      model_id: "local:gguf:qwen3-14b",
      model: "Qwen3 14B",
      context_tokens: 32768,
      memory: "gpu",
      cpu_tried: false,
      smaller_context: 16384,
      detail: "cudaMalloc failed: out of memory",
    },
  }),
];

type MemoryItem = Extract<ChatItem, { kind: "memory" }>;

describe("a local model that ran out of memory", () => {
  it("ends the task with the plain reason and one card offering a way on", () => {
    const state = replay(outOfMemory);
    expect(state.stage).toBe("FAILED");
    const agent = state.items.filter((item) => item.kind === "agent");
    expect(agent).toHaveLength(1);
    expect(agent[0]).toMatchObject({ who: "Needs attention", text: summary });
    const cards = state.items.filter(
      (item): item is MemoryItem => item.kind === "memory",
    );
    expect(cards).toHaveLength(1);
    expect(cards[0]).toMatchObject({
      taskId: "a",
      model: "Qwen3 14B",
      smallerContext: 16384,
      request: "Explain main.rs",
    });
    expect(cards[0].resolved).toBeFalsy();
    // Replaying the same rows again adds nothing.
    const again = replay([...outOfMemory, ...outOfMemory]);
    expect(again.items.filter((item) => item.kind === "memory")).toHaveLength(
      1,
    );
  });

  it("closes the card once another message is sent", () => {
    const state = replay([
      ...outOfMemory,
      event(5, "user.message", { text: "Explain main.rs" }, "b"),
    ]);
    const card = state.items.find(
      (item): item is MemoryItem => item.kind === "memory",
    );
    expect(card?.resolved).toBe(true);
  });

  it("other failures get no memory card", () => {
    const state = replay([
      event(1, "user.message", { text: "Explain main.rs" }),
      event(2, "agent.completed", {
        success: false,
        cancelled: false,
        summary: "llama-server exited before it became ready.",
      }),
    ]);
    expect(state.items.some((item) => item.kind === "memory")).toBe(false);
  });

  it("saves the smaller context, then sends the request again", async () => {
    const onUseContext = vi.fn(async () => {});
    const onChoose = vi.fn();
    const onOpenLocal = vi.fn();
    const item = replay(outOfMemory).items.find(
      (row): row is MemoryItem => row.kind === "memory",
    )!;
    render(
      <LocalMemoryItem
        item={item}
        onUseContext={onUseContext}
        onChoose={onChoose}
        onOpenLocal={onOpenLocal}
      />,
    );
    const card = screen.getByRole("region", { name: "Not enough memory" });
    expect(card.textContent).toContain("16,384-token context");
    fireEvent.click(
      screen.getByRole("button", {
        name: "Use a 16,384-token context and retry",
      }),
    );
    await waitFor(() => expect(onUseContext).toHaveBeenCalledWith(item, 16384));
    fireEvent.click(
      screen.getByRole("button", { name: "Choose another model" }),
    );
    expect(onChoose).toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Open Local models" }));
    expect(onOpenLocal).toHaveBeenCalled();
  });

  it("shows a failed save instead of retrying silently", async () => {
    const item: MemoryItem = {
      kind: "memory",
      taskId: "a",
      text: "",
      model: "Qwen3 14B",
      smallerContext: 8192,
      request: "Explain main.rs",
    };
    render(
      <LocalMemoryItem
        item={item}
        onUseContext={async () => {
          throw new Error("Error: local_engine.context_size must be 0");
        }}
        onChoose={vi.fn()}
        onOpenLocal={vi.fn()}
      />,
    );
    fireEvent.click(
      screen.getByRole("button", {
        name: "Use a 8,192-token context and retry",
      }),
    );
    expect((await screen.findByRole("alert")).textContent).toBe(
      "local_engine.context_size must be 0",
    );
  });

  it("without a usable smaller context, offers another model only", () => {
    render(
      <LocalMemoryItem
        item={{
          kind: "memory",
          taskId: "a",
          text: "",
          model: "Tiny",
          request: "hi",
        }}
        onUseContext={vi.fn()}
        onChoose={vi.fn()}
        onOpenLocal={vi.fn()}
      />,
    );
    expect(
      screen.queryByRole("button", { name: /context and retry/ }),
    ).toBeNull();
    expect(
      screen.getByRole("button", { name: "Choose another model" }),
    ).toBeTruthy();
    cleanup();
    render(
      <LocalMemoryItem
        item={{
          kind: "memory",
          taskId: "a",
          text: "",
          model: "Tiny",
          request: "hi",
          resolved: true,
        }}
        onUseContext={vi.fn()}
        onChoose={vi.fn()}
        onOpenLocal={vi.fn()}
      />,
    );
    expect(screen.queryAllByRole("button")).toHaveLength(0);
  });
});
