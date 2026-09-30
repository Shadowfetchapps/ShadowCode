import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";

const mocks = vi.hoisted(() => ({ explainChange: vi.fn() }));
vi.mock("../api", () => ({ api: mocks }));

import { ExplainChange } from "./ExplainChange";

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("drops an explanation that arrives after another file was opened", async () => {
  let answer: (value: unknown) => void = () => undefined;
  mocks.explainChange.mockReturnValue(
    new Promise((resolve) => {
      answer = resolve;
    }),
  );
  const view = render(<ExplainChange taskId="t1" path="a.ts" />);
  fireEvent.click(screen.getByRole("button", { name: "Explain this change" }));
  expect(mocks.explainChange).toHaveBeenCalledWith("t1", "a.ts");
  view.rerender(<ExplainChange taskId="t1" path="b.ts" />);
  await act(async () => {
    answer({ ok: true, text: "What a.ts changed", model: "m" });
  });
  expect(screen.queryByText("What a.ts changed")).toBeNull();
  const button = screen.getByRole("button", { name: "Explain this change" });
  expect((button as HTMLButtonElement).disabled).toBe(false);
});

it("shows the explanation for the file still open", async () => {
  mocks.explainChange.mockResolvedValue({
    ok: true,
    text: "What b.ts changed",
    model: "m",
  });
  render(<ExplainChange taskId="t1" path="b.ts" />);
  await act(async () => {
    fireEvent.click(
      screen.getByRole("button", { name: "Explain this change" }),
    );
  });
  expect(screen.getByText("What b.ts changed")).toBeTruthy();
  expect(screen.getByText("Explained by m.")).toBeTruthy();
});
