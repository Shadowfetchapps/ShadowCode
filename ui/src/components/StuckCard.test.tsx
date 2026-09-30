import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { StuckCard } from "./StuckCard";

afterEach(cleanup);

it("offers Keep going, a hint, another model and Stop", async () => {
  const onAction = vi.fn(async () => {});
  render(
    <StuckCard
      text="The agent seems stuck: it ran `npm test` 3 times and it failed the same way each time."
      onAction={onAction}
    />,
  );
  expect(
    screen.getByRole("region", { name: "The agent seems stuck" }),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Give a hint" }));
  fireEvent.change(screen.getByLabelText("What should it try instead?"), {
    target: { value: "The test expects rounding; fix round() instead" },
  });
  fireEvent.click(
    screen.getByRole("button", { name: "Send hint and continue" }),
  );
  await waitFor(() =>
    expect(onAction).toHaveBeenCalledWith(
      "hint",
      "The test expects rounding; fix round() instead",
    ),
  );
  expect(
    await screen.findByText("Your hint was sent and the task continued."),
  ).toBeTruthy();
});

it("shows an error and keeps the buttons when an action fails", async () => {
  const onAction = vi.fn(async () => {
    throw new Error("Task already finished");
  });
  render(<StuckCard text="Stuck." onAction={onAction} />);
  fireEvent.click(screen.getByRole("button", { name: "Keep going" }));
  expect(await screen.findByText(/Task already finished/)).toBeTruthy();
  expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
});

it("offers no choices when the task was not paused or has gone on", () => {
  const onAction = vi.fn(async () => {});
  const { rerender } = render(
    <StuckCard text="Stuck." paused={false} onAction={onAction} />,
  );
  expect(
    screen.getByText(
      "The task went on; the agent was asked to try another way.",
    ),
  ).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Keep going" })).toBeNull();
  rerender(<StuckCard text="Stuck." resolved="ended" onAction={onAction} />);
  expect(screen.getByText("The task has ended.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "Stop" })).toBeNull();
});
