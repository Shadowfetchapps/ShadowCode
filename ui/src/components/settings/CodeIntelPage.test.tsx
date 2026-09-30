import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api, type CodeIntelStatus } from "../../api";
import { CodeIntelPage } from "./CodeIntelPage";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function status(overrides: Partial<CodeIntelStatus> = {}): CodeIntelStatus {
  return {
    config: {
      lsp: true,
      diagnostics_on_edit: true,
      diagnostics_wait_ms: 3000,
      lsp_idle_minutes: 10,
      max_servers: 4,
      repo_map_tokens: 1024,
      semantic_search: true,
      embedding_model: "",
      servers: {},
    },
    offline: false,
    languages: [
      {
        language: "rust",
        label: "Rust",
        available: true,
        enabled: true,
        server: "rust-analyzer",
        source: "path",
      },
      {
        language: "python",
        label: "Python",
        available: false,
        enabled: true,
        managed_package: "python",
        note: "No Python language server found.",
      },
    ],
    servers: [],
    managed: [
      {
        id: "python",
        label: "Python (Pyright)",
        packages: ["pyright@1.1.414"],
        approx_bytes: 19_457_120,
        installed: false,
        installed_bytes: null,
        progress: null,
      },
    ],
    npm: { available: true, path: "/usr/bin/npm" },
    index: { files: 12, symbols: 80, chunks: 30, languages: { rust: 12 } },
    embeddings: {
      models: [
        {
          id: "bge-small-en-v1.5-q8",
          name: "BGE small (English) v1.5, Q8_0",
          summary: "Smallest and fastest.",
          bytes: 36_806_944,
          license: "MIT",
          installed: false,
          active: false,
          progress: null,
        },
      ],
      active: null,
      runtime: "/usr/lib/shadowcode/llama-server",
      coverage: null,
      backfill: null,
    },
    ...overrides,
  };
}

it("shows servers, sizes, and installs only on click", async () => {
  vi.spyOn(api, "codeIntelStatus").mockResolvedValue(status());
  const install = vi
    .spyOn(api, "installLanguageServer")
    .mockResolvedValue({ ok: true, started: true });
  const model = vi
    .spyOn(api, "installEmbeddingModel")
    .mockResolvedValue({ ok: true, started: true });
  render(<CodeIntelPage onToast={vi.fn()} />);
  const rust = await screen.findByRole("listitem", { name: "Rust" });
  expect(
    within(rust).getByText("rust-analyzer · found on this computer"),
  ).toBeTruthy();
  const python = screen.getByRole("listitem", { name: "Python" });
  expect(within(python).getByText("Not installed")).toBeTruthy();
  const button = within(python).getByRole("button", {
    name: "Install (about 19 MB)",
  });
  expect(install).not.toHaveBeenCalled();
  fireEvent.click(button);
  await waitFor(() => expect(install).toHaveBeenCalledWith("python"));
  expect(
    screen.getByText("12 files · 80 definitions · 30 chunks"),
  ).toBeTruthy();
  const bge = screen.getByRole("article", {
    name: "BGE small (English) v1.5, Q8_0",
  });
  fireEvent.click(within(bge).getByRole("button", { name: "Install (35 MB)" }));
  await waitFor(() =>
    expect(model).toHaveBeenCalledWith("bge-small-en-v1.5-q8"),
  );
});

it("disables downloads offline and saves settings", async () => {
  vi.spyOn(api, "codeIntelStatus").mockResolvedValue(status({ offline: true }));
  const save = vi
    .spyOn(api, "saveCodeIntel")
    .mockResolvedValue({ ok: true, config: status().config });
  render(<CodeIntelPage onToast={vi.fn()} />);
  const python = await screen.findByRole("listitem", { name: "Python" });
  expect(
    (
      within(python).getByRole("button", {
        name: /Install/,
      }) as HTMLButtonElement
    ).disabled,
  ).toBe(true);
  expect(screen.getByText(/Offline mode is on/)).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Map in the agent's prompt"), {
    target: { value: "0" },
  });
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith({ repo_map_tokens: 0 }),
  );
  fireEvent.click(
    screen.getByLabelText(/Tell the agent about errors its edits introduce/),
  );
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith({ diagnostics_on_edit: false }),
  );
});

it("points a project over the file limit to a focus folder", async () => {
  const index = { symbols: 80, chunks: 30, languages: { rust: 12 } };
  vi.spyOn(api, "codeIntelStatus").mockResolvedValue(
    status({
      index: {
        ...index,
        files: 250_000,
        total: 250_000,
        complete: false,
        capped: true,
      },
    }),
  );
  render(<CodeIntelPage onToast={vi.fn()} />);
  const progress = await screen.findByText(/Set a focus folder/);
  expect(progress.textContent).not.toContain("Reindex continues");
  cleanup();
  vi.spyOn(api, "codeIntelStatus").mockResolvedValue(
    status({
      index: { ...index, files: 2_000, total: 2_500, complete: false },
    }),
  );
  render(<CodeIntelPage onToast={vi.fn()} />);
  expect(
    await screen.findByText("Indexed 2,000 of 2,500 files; Reindex continues"),
  ).toBeTruthy();
});
