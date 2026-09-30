import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { LimitFallbackItem } from "./LimitFallback";
import type { ChatItem } from "./cards";

afterEach(() => cleanup());

const NOW = 1_790_000_000;
const RESUME_ON_CODEX = /^Resume on Codex (?:tomorrow )?at /;

const ask: Extract<ChatItem, { kind: "limit" }> = {
  kind: "limit",
  taskId: "a",
  mode: "ask",
  text: "Codex reached its plan limit.",
  from: "Codex",
  request: "Fix the add function",
};

function show(
  item: Extract<ChatItem, { kind: "limit" }>,
  fallback = { id: "local:gguf:qwen", name: "qwen3:14b" } as {
    id: string;
    name: string;
  } | null,
) {
  const props = {
    onContinue: vi.fn(),
    onChoose: vi.fn(),
    onOpenLocal: vi.fn(),
    onScheduleResume: vi.fn(),
  };
  render(
    <LimitFallbackItem item={item} fallback={fallback} now={NOW} {...props} />,
  );
  return props;
}

it("asks: Continue on the fallback, or try it on another model", () => {
  const props = show(ask);
  const card = screen.getByRole("region", { name: "Plan limit reached" });
  expect(card.textContent).toContain("Codex reached its plan limit.");
  fireEvent.click(
    screen.getByRole("button", { name: "Continue on qwen3:14b" }),
  );
  expect(props.onContinue).toHaveBeenCalledWith({
    id: "local:gguf:qwen",
    name: "qwen3:14b",
  });
  fireEvent.click(screen.getByRole("button", { name: "Try on…" }));
  expect(props.onChoose).toHaveBeenCalled();
  cleanup();
  // Without a ready local model the card leads to Local models instead.
  const none = show(ask, null);
  expect(screen.queryByRole("button", { name: /Continue on/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Open Local models" }));
  expect(none.onOpenLocal).toHaveBeenCalled();
  cleanup();
  // Once a follow-up started, the offer is gone.
  show({ ...ask, resolved: true });
  expect(screen.queryAllByRole("button")).toHaveLength(0);
});

it("says where the conversation continued, or why it could not", () => {
  show({
    kind: "limit",
    taskId: "a",
    mode: "continued",
    text: "Codex reached its plan limit. Continuing on qwen3:14b on this computer.",
    from: "Codex",
    to: "qwen3:14b",
  });
  expect(
    screen.getByText(
      "Codex reached its plan limit. Continuing on qwen3:14b on this computer.",
    ),
  ).toBeTruthy();
  expect(screen.queryAllByRole("button")).toHaveLength(0);
  cleanup();
  const props = show({
    kind: "limit",
    taskId: "a",
    mode: "unavailable",
    text: "Codex reached its plan limit. No local model is ready.",
    from: "Codex",
    reason: "No local model is ready.",
  });
  fireEvent.click(screen.getByRole("button", { name: "Open Local models" }));
  expect(props.onOpenLocal).toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Try on…" }));
  expect(props.onChoose).toHaveBeenCalled();
});

it("offers Resume at the reset time only when the vendor said it", () => {
  const at = NOW + 2 * 3600;
  const item: Extract<ChatItem, { kind: "limit" }> = {
    ...ask,
    jobId: "job-1",
    resetsAt: at,
  };
  const props = show(item);
  // Two hours on can be tomorrow in some time zones; either way the phrase
  // carries a single "at".
  const button = screen.getByRole("button", { name: RESUME_ON_CODEX });
  expect(button.textContent).not.toContain("at tomorrow");
  fireEvent.click(button);
  expect(props.onScheduleResume).toHaveBeenCalledWith(item);
  cleanup();
  // Already scheduled, already past, or unknown: no offer.
  for (const other of [
    { ...item, resumeScheduled: true },
    { ...item, resetsAt: NOW - 60 },
    { ...item, resetsAt: undefined },
  ]) {
    show(other);
    expect(screen.queryByRole("button", { name: /^Resume on/ })).toBeNull();
    cleanup();
  }
  // An automatic local continuation can still resume on the vendor later.
  show({
    ...item,
    mode: "continued",
    to: "qwen3:14b",
    text: "Codex reached its plan limit. Continuing on qwen3:14b on this computer.",
  });
  expect(screen.getByRole("button", { name: RESUME_ON_CODEX })).toBeTruthy();
});

it("rewords Resume at midnight and drops it once the reset has passed", () => {
  vi.useFakeTimers();
  try {
    vi.setSystemTime(new Date(2026, 8, 29, 23, 0));
    const at = new Date(2026, 8, 30, 3, 40).getTime() / 1000;
    render(
      <LimitFallbackItem
        item={{ ...ask, jobId: "job-1", resetsAt: at }}
        fallback={null}
        onContinue={vi.fn()}
        onChoose={vi.fn()}
        onOpenLocal={vi.fn()}
        onScheduleResume={vi.fn()}
      />,
    );
    expect(
      screen.getByRole("button", { name: /^Resume on Codex tomorrow at / }),
    ).toBeTruthy();
    act(() => vi.advanceTimersByTime(61 * 60 * 1000));
    const button = screen.getByRole("button", { name: RESUME_ON_CODEX });
    expect(button.textContent).not.toContain("tomorrow");
    // 3:40 passes with the window open: the offer is gone.
    act(() => vi.advanceTimersByTime(3 * 3600 * 1000 + 40 * 60 * 1000));
    expect(screen.queryByRole("button", { name: /^Resume on/ })).toBeNull();
  } finally {
    vi.useRealTimers();
  }
});
