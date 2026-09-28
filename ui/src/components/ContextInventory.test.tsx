import { afterEach, expect, it, vi } from "vitest";
import { cleanup, render, screen } from "@testing-library/react";
import { api } from "../api";
import { ContextInventory } from "./ContextInventory";

vi.mock("../api", () => ({ api: { contextPreview: vi.fn() } }));

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

it("labels the bounded attachment token count as an estimate", async () => {
  vi.mocked(api.contextPreview).mockResolvedValue({
    items: [
      {
        path: "src/main.rs",
        kind: "file",
        included: true,
        reason: "Explicit @mention; bounded file text is attached.",
        bytes: 27,
        total_bytes: 27,
        from_line: 1,
        to_line: 2,
        entries: [],
        truncated: false,
      },
    ],
    included_bytes: 27,
    estimated_tokens: 14,
    truncated: false,
  });

  render(
    <ContextInventory
      mentions={[{ path: "src/main.rs", kind: "file" }]}
      onClose={() => undefined}
    />,
  );

  expect(
    await screen.findByText("About 14 tokens estimated · 27 attached bytes"),
  ).toBeTruthy();
  expect(
    screen.getByText(/Files are read again when the task starts/),
  ).toBeTruthy();
});
