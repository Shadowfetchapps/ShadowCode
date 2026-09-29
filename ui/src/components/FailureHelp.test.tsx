import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { FailureHelp } from "./FailureHelp";

afterEach(() => cleanup());

it("explains a rejected key and offers the next steps", () => {
  const onStep = vi.fn();
  render(
    <FailureHelp
      text="Model provider returned HTTP 401; check the API key: No auth credentials found"
      can={() => true}
      onStep={onStep}
    />,
  );
  const help = screen.getByRole("region", { name: "What went wrong" });
  expect(help.textContent).toContain("The provider didn't accept the key");
  fireEvent.click(screen.getByRole("button", { name: "Choose a model" }));
  expect(onStep).toHaveBeenCalledWith("choose-model");
});

it("hides steps the row can't take and stays out of the way otherwise", () => {
  render(
    <FailureHelp
      text="Model provider returned HTTP 429; provider rate limit reached"
      can={(step) => step !== "retry"}
      onStep={() => undefined}
    />,
  );
  expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  expect(
    screen.getByRole("button", { name: "Continue on another model…" }),
  ).toBeTruthy();
  cleanup();
  const { container } = render(
    <FailureHelp
      text="The tests still fail."
      can={() => true}
      onStep={() => undefined}
    />,
  );
  expect(container.textContent).toBe("");
});
