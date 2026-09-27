import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { HealthTab } from "./AdvancedPanels";
import { api } from "../../api";
import { exportDiagnostics } from "../../lib/transport";

vi.mock("../../api", () => ({ api: { doctor: vi.fn() } }));
vi.mock("../../lib/transport", () => ({ isNative: vi.fn(() => true), exportDiagnostics: vi.fn() }));

const content = '{"schema":1,"checks":[{"id":"runtime","status":"pass"}]}\n';
const report = () => ({
  ok: true,
  version: "fixture",
  suggestions: [],
  checks: [{ id: "runtime", ok: true, status: "pass" as const, label: "Native runtime" }],
  diagnostic_export: {
    id: "a".repeat(32),
    filename: "shadowcode-diagnostics.json",
    content,
    mime: "application/json",
    captured_at: "2026-01-01T00:00:00Z",
    byte_length: content.length,
  },
});

beforeEach(() => {
  vi.mocked(api.doctor).mockResolvedValue(report());
  vi.mocked(exportDiagnostics).mockResolvedValue("shadowcode-diagnostics.json");
});
afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("shows an exact preview before the only save action", async () => {
  render(<HealthTab health={null} />);
  await screen.findByText("Native runtime");
  expect(exportDiagnostics).not.toHaveBeenCalled();
  expect(screen.queryByRole("button", { name: "Save diagnostics…" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Preview diagnostics export" }));
  expect(screen.getByLabelText("Diagnostics export preview").textContent).toBe(content);
  expect(exportDiagnostics).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole("button", { name: "Save diagnostics…" }));
  await waitFor(() =>
    expect(exportDiagnostics).toHaveBeenCalledWith("a".repeat(32), content),
  );
  expect(await screen.findByText("Diagnostics saved.")).toBeTruthy();
});

it("hides the old preview while a new Doctor result is pending", async () => {
  render(<HealthTab health={null} />);
  await screen.findByText("Native runtime");
  fireEvent.click(screen.getByRole("button", { name: "Preview diagnostics export" }));
  vi.mocked(api.doctor).mockImplementationOnce(() => new Promise(() => {}));
  fireEvent.click(screen.getByRole("button", { name: "Run again" }));
  expect(screen.queryByRole("button", { name: "Save diagnostics…" })).toBeNull();
  expect(exportDiagnostics).not.toHaveBeenCalled();
});
