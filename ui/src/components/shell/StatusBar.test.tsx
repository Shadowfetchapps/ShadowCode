import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { StatusBar } from "./StatusBar";

afterEach(cleanup);

const base = {
  busy: false,
  paused: false,
  reconnecting: false,
  branch: "",
  onBranch: () => undefined,
  allowance: null,
  allowanceOpen: false,
  onAllowance: () => undefined,
  version: "0.33.1",
};

it("shows a quiet update notice that opens About", () => {
  const open = vi.fn();
  render(<StatusBar {...base} update="0.34.0" onUpdate={open} />);
  fireEvent.click(
    screen.getByRole("button", { name: "Update available: 0.34.0" }),
  );
  expect(open).toHaveBeenCalledTimes(1);
  expect(screen.getByText("v0.33.1")).toBeTruthy();
});

it("shows no notice without a newer version", () => {
  render(<StatusBar {...base} update="" onUpdate={() => undefined} />);
  expect(screen.queryByRole("button", { name: /Update available/ })).toBeNull();
});

it("never says Ready when the engine did not answer", () => {
  render(<StatusBar {...base} branch="main" connected={false} version="" />);
  const bar = screen.getByRole("contentinfo");
  expect(bar.textContent).toContain("Not connected");
  expect(bar.textContent).not.toContain("Ready");
  expect(bar.textContent).not.toContain("Connecting");
  expect(bar.querySelector(".status-dot.offline")).toBeTruthy();
  cleanup();
  render(<StatusBar {...base} connected />);
  expect(screen.getByRole("contentinfo").textContent).toContain("Ready");
  expect(screen.getByRole("contentinfo").textContent).toContain("v0.33.1");
});
