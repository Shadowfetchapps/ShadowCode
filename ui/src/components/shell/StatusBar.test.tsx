import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { StatusBar } from "./StatusBar";

afterEach(cleanup);

const base = {
  busy: false,
  paused: false,
  reconnecting: false,
  branch: "main",
  onBranch: vi.fn(),
  allowance: null,
  allowanceOpen: false,
  onAllowance: vi.fn(),
};

it("never says Ready when the engine did not answer", () => {
  render(<StatusBar {...base} connected={false} version="" />);
  const bar = screen.getByRole("contentinfo");
  expect(bar.textContent).toContain("Not connected");
  expect(bar.textContent).not.toContain("Ready");
  expect(bar.querySelector(".status-dot.offline")).toBeTruthy();
  cleanup();
  render(<StatusBar {...base} connected version="0.33.1" />);
  expect(screen.getByRole("contentinfo").textContent).toContain("Ready");
  expect(screen.getByRole("contentinfo").textContent).toContain("v0.33.1");
});
