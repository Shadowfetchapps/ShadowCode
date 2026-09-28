import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { installFakeBackend, type FakeOptions } from "../../e2e/fakeBackend";
import { useModelDownloads } from "../hooks/useModelDownloads";
import { DownloadList } from "./ModelDownloads";
import { ModelSetup, ModelSetupPanel } from "./ModelSetup";
import { Onboarding, anyModelReady } from "./Onboarding";
import { api } from "../api";

let fake: ReturnType<typeof installFakeBackend>;
function start(options: FakeOptions = {}) {
  fake = installFakeBackend({ stepMs: 5, firstRun: true, ...options });
}
beforeEach(() => {
  vi.stubEnv("VITE_SHADOW_TEST_TRANSPORT", "1");
});
afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
  vi.useRealTimers();
  delete window.__SHADOW_TEST_TRANSPORT__;
});

const posts = (path: string) =>
  fake.log.filter((r) => r.method === "POST" && r.path === path);

function Setup(props: {
  onStarted?: () => void;
  onOpenRouter?: () => void;
  onSubscription?: () => void;
  onBrowse?: () => void;
  onError?: (text: string) => void;
}) {
  const downloads = useModelDownloads({ onError: props.onError || vi.fn() });
  return (
    <ModelSetup
      downloads={downloads}
      onStarted={props.onStarted || vi.fn()}
      onOpenRouter={props.onOpenRouter || vi.fn()}
      onSubscription={props.onSubscription || vi.fn()}
      onBrowse={props.onBrowse || vi.fn()}
    />
  );
}

it("preselects the recommended free model with its size, time and fit", async () => {
  start();
  const onStarted = vi.fn();
  render(<Setup onStarted={onStarted} />);
  const download = await screen.findByRole("radio", {
    name: /Download a free model to run on this computer/,
  });
  await waitFor(() => expect(download).toHaveProperty("checked", true));
  expect(
    screen.getByText(
      "Gemma 4 E4B by Google: 4.8 GB, about 14 minutes on a 50 Mbit/s connection. Runs on your graphics card.",
    ),
  ).toBeTruthy();
  expect(
    screen.getByText(
      "Picked for this computer: Intel Arc A750 (8.0 GB) · 16 GB memory.",
    ),
  ).toBeTruthy();
  // Nothing is fetched before the click.
  expect(posts("/api/local-models/downloads/start")).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Download Gemma 4 E4B" }));
  await waitFor(() => expect(onStarted).toHaveBeenCalledTimes(1));
  expect(posts("/api/local-models/downloads/start")[0].body).toEqual({
    id: "gemma-4-e4b",
  });
  // No select control: the picker stays the one place a model is chosen.
  expect(document.querySelector("select")).toBeNull();
});

it("routes OpenRouter, subscriptions and other models to their pages", async () => {
  start();
  const onOpenRouter = vi.fn();
  const onSubscription = vi.fn();
  const onBrowse = vi.fn();
  render(
    <Setup
      onOpenRouter={onOpenRouter}
      onSubscription={onSubscription}
      onBrowse={onBrowse}
    />,
  );
  fireEvent.click(
    await screen.findByRole("button", { name: "See other free models" }),
  );
  expect(onBrowse).toHaveBeenCalled();
  fireEvent.click(screen.getByRole("radio", { name: /Use an OpenRouter key/ }));
  fireEvent.click(
    screen.getByRole("button", { name: "Add an OpenRouter key" }),
  );
  expect(onOpenRouter).toHaveBeenCalled();
  fireEvent.click(
    screen.getByRole("radio", { name: /Sign in to a subscription/ }),
  );
  fireEvent.click(
    screen.getByRole("button", { name: "Choose a subscription" }),
  );
  expect(onSubscription).toHaveBeenCalled();
  expect(posts("/api/local-models/downloads/start")).toHaveLength(0);
});

it("offline: the download choice is off and says how to turn it on", async () => {
  start();
  fake.state.config.network = { mode: "offline" };
  render(<Setup />);
  const download = await screen.findByRole("radio", {
    name: /Download a free model/,
  });
  await waitFor(() =>
    expect(
      screen.getAllByText(/Offline mode is on. Switch to Online/).length,
    ).toBeGreaterThan(0),
  );
  expect(download).toHaveProperty("disabled", true);
  expect(download).toHaveProperty("checked", false);
  expect(screen.getByRole("button", { name: "Continue" })).toHaveProperty(
    "disabled",
    true,
  );
});

it("too little memory: explains and offers the other two choices", async () => {
  start();
  for (const m of fake.state.downloads.models) {
    m.fit = "no";
    m.recommended = false;
  }
  fake.state.downloads.hardware = {
    ram_bytes: 4 * 1024 ** 3,
    vram_bytes: null,
    gpu: null,
  };
  const original = api.modelDownloads;
  vi.spyOn(api, "modelDownloads").mockImplementation(async () => ({
    ...(await original()),
    recommended: null,
    recommended_fit: null,
  }));
  render(<Setup />);
  expect(
    await screen.findByText(
      /This computer has 4.0 GB of memory; the smallest free model needs about 5.2 GB/,
    ),
  ).toBeTruthy();
  expect(
    screen.getByRole("radio", { name: /Download a free model/ }),
  ).toHaveProperty("disabled", true);
  vi.restoreAllMocks();
});

it("a refused start (disk space) is shown and nothing is marked started", async () => {
  start();
  const onError = vi.fn();
  const onStarted = vi.fn();
  vi.spyOn(api, "startModelDownload").mockRejectedValue(
    new Error(
      "Not enough disk space: this download needs 5.7 GB more and 2.0 GB is free in /data. Free up space and try again.",
    ),
  );
  render(<Setup onError={onError} onStarted={onStarted} />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Download Gemma 4 E4B" }),
  );
  await waitFor(() =>
    expect(onError).toHaveBeenCalledWith(
      expect.stringMatching(/^Not enough disk space/),
    ),
  );
  expect(onStarted).not.toHaveBeenCalled();
  vi.restoreAllMocks();
});

it("onboarding: with no ready model, a second step offers the three choices", async () => {
  start();
  const onDone = vi.fn();
  render(<Onboarding onDone={onDone} />);
  await waitFor(() =>
    expect(
      (screen.getByLabelText("Project folder") as HTMLInputElement).value,
    ).toBe("/work/demo"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Trust and open" }));
  const dialog = await screen.findByRole("dialog", { name: "Choose a model" });
  expect(
    within(dialog).getByRole("heading", {
      name: "Choose how ShadowCode thinks",
    }),
  ).toBeTruthy();
  expect(onDone).not.toHaveBeenCalled();
  expect(fake.state.onboarded).toBe(true);
  fireEvent.click(
    within(dialog).getByRole("radio", { name: /Sign in to a subscription/ }),
  );
  fireEvent.click(
    within(dialog).getByRole("button", { name: "Choose a subscription" }),
  );
  expect(onDone).toHaveBeenCalledWith("subscription");
});

it("onboarding: Download starts the recommended model and closes", async () => {
  start();
  const onDone = vi.fn();
  render(<Onboarding onDone={onDone} />);
  await waitFor(() =>
    expect(
      (screen.getByLabelText("Project folder") as HTMLInputElement).value,
    ).toBe("/work/demo"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Trust and open" }));
  fireEvent.click(
    await screen.findByRole("button", { name: "Download Gemma 4 E4B" }),
  );
  await waitFor(() => expect(onDone).toHaveBeenCalledWith());
  expect(fake.state.downloads.models[1].state).toBe("downloading");
});

it("onboarding: a ready model skips the model step", async () => {
  start({ firstRun: false, onboarding: true });
  const onDone = vi.fn();
  render(<Onboarding onDone={onDone} />);
  await waitFor(() =>
    expect(
      (screen.getByLabelText("Project folder") as HTMLInputElement).value,
    ).toBe("/work/demo"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Trust and open" }));
  await waitFor(() => expect(onDone).toHaveBeenCalledWith());
  expect(screen.queryByRole("dialog", { name: "Choose a model" })).toBeNull();
  expect(fake.log.some((r) => r.path === "/api/local-models/downloads")).toBe(
    false,
  );
});

it("the ready check falls back to cached rows when accounts are slow", async () => {
  start();
  vi.spyOn(api, "picker").mockReturnValue(new Promise(() => undefined));
  const cached = vi.spyOn(api, "pickerCached");
  expect(await anyModelReady(50)).toBe(false);
  expect(cached).toHaveBeenCalled();
  vi.restoreAllMocks();
  start({ firstRun: false });
  vi.spyOn(api, "picker").mockReturnValue(new Promise(() => undefined));
  expect(await anyModelReady(5000)).toBe(true);
  vi.restoreAllMocks();
});

it("the empty conversation follows the download and selects nothing itself", async () => {
  start({ downloadSteps: 2 });
  function Panel() {
    const downloads = useModelDownloads({ onError: vi.fn() });
    return (
      <ModelSetupPanel
        downloads={downloads}
        hasReadyModel={false}
        onChooseModel={vi.fn()}
        onOpenRouter={vi.fn()}
        onSubscription={vi.fn()}
        onBrowse={vi.fn()}
      />
    );
  }
  render(<Panel />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Download Gemma 4 E4B" }),
  );
  const row = await screen.findByRole("article", { name: "Gemma 4 E4B" });
  expect(
    screen.getByText(
      "Downloading Gemma 4 E4B. It is selected here as soon as it is ready.",
    ),
  ).toBeTruthy();
  expect(within(row).getByRole("button", { name: "Pause" })).toBeTruthy();
  fireEvent.click(within(row).getByRole("button", { name: "Pause" }));
  await waitFor(() =>
    expect(within(row).getByRole("status").textContent).toMatch(/^Paused at/),
  );
  expect(
    screen.getByText("Gemma 4 E4B isn't finished downloading yet."),
  ).toBeTruthy();
  fireEvent.click(within(row).getByRole("button", { name: "Cancel download" }));
  // Cancelled: back to the three choices.
  expect(
    await screen.findByRole("radio", { name: /Download a free model/ }),
  ).toBeTruthy();
  expect(fake.state.downloads.models[1].done).toBe(0);
});

it("Settings list: progress, a dropped connection, Resume, then Delete", async () => {
  start({ downloadSteps: 3, downloadDrops: true });
  const onInstalled = vi.fn();
  function List() {
    const downloads = useModelDownloads({ onError: vi.fn(), onInstalled });
    return <DownloadList downloads={downloads} />;
  }
  render(<List />);
  const row = await screen.findByRole("article", { name: "Gemma 4 E4B" });
  expect(within(row).getByText("Recommended for this computer")).toBeTruthy();
  expect(
    within(row).getByRole("link", { name: "Apache-2.0" }).getAttribute("href"),
  ).toBe("https://www.apache.org/licenses/LICENSE-2.0");
  const qwen = screen.getByRole("article", { name: "Qwen3.6 35B-A3B" });
  expect(within(qwen).getByRole("button", { name: /Download/ })).toHaveProperty(
    "disabled",
    true,
  );
  expect(
    within(qwen).getAllByText(/Needs more memory than this computer has/)
      .length,
  ).toBeGreaterThan(0);
  fireEvent.click(
    within(row).getByRole("button", { name: "Download (4.8 GB)" }),
  );
  // The first attempt drops a third of the way: Resume continues.
  expect(
    await within(row).findByText(
      /The connection dropped at 1.6 GB of 4.8 GB/,
      {},
      { timeout: 4000 },
    ),
  ).toBeTruthy();
  expect(within(row).getByRole("status").textContent).toBe(
    "Stopped at 1.6 GB of 4.8 GB",
  );
  fireEvent.click(within(row).getByRole("button", { name: "Resume" }));
  await waitFor(
    () => expect(within(row).getByText("Downloaded")).toBeTruthy(),
    { timeout: 6000 },
  );
  expect(onInstalled).toHaveBeenCalledTimes(1);
  expect(onInstalled.mock.calls[0][0].model_id).toBe(
    "local:gguf:dl-gemma-4-e4b",
  );
  fireEvent.click(within(row).getByRole("button", { name: "Delete" }));
  expect(
    within(row).getByText(
      /Delete Gemma 4 E4B \(4.8 GB\)\? You can download it again later./,
    ),
  ).toBeTruthy();
  fireEvent.click(within(row).getByRole("button", { name: "Delete" }));
  await waitFor(() =>
    expect(posts("/api/local-models/downloads/delete")).toHaveLength(1),
  );
  expect(
    await within(row).findByRole("button", { name: "Download (4.8 GB)" }),
  ).toBeTruthy();
});
