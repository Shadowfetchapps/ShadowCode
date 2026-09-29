import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { CrashScreen } from "./CrashScreen";

afterEach(() => cleanup());

function Broken(): never {
  throw new Error("render bug");
}

it("shows a plain message and a reload button instead of an empty window", () => {
  const quiet = vi.spyOn(console, "error").mockImplementation(() => {});
  const reload = vi.fn();
  render(
    <CrashScreen onReload={reload}>
      <Broken />
    </CrashScreen>,
  );
  const alert = screen.getByRole("alert");
  expect(alert.textContent).toContain("Something went wrong in this window");
  expect(alert.textContent).toContain("Your tasks keep running");
  expect(alert.textContent).not.toContain("render bug");
  fireEvent.click(screen.getByRole("button", { name: "Reload window" }));
  expect(reload).toHaveBeenCalledTimes(1);
  quiet.mockRestore();
});

it("renders its children when nothing fails", () => {
  render(
    <CrashScreen>
      <p>working</p>
    </CrashScreen>,
  );
  expect(screen.getByText("working")).toBeTruthy();
  expect(screen.queryByRole("alert")).toBeNull();
});
