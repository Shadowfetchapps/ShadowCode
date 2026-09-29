import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { installFakeBackend } from "../../../e2e/fakeBackend";
import { describeSummary, formatBytes } from "../../lib/data";
import { DataPage } from "./DataPage";

let fake: ReturnType<typeof installFakeBackend>;
beforeEach(() => {
  vi.stubEnv("VITE_SHADOW_TEST_TRANSPORT", "1");
  fake = installFakeBackend({ stepMs: 5 });
});
afterEach(() => {
  cleanup();
  vi.unstubAllEnvs();
  delete window.__SHADOW_TEST_TRANSPORT__;
});

const posted = (path: string) =>
  fake.log.filter((r) => r.path === path && r.method !== "GET");

it("shows backups, upgrade copies and the permanent folder names", async () => {
  render(<DataPage onToast={vi.fn()} />);
  const backups = await screen.findByRole("list", { name: "Backups" });
  expect(
    within(backups).getByText(/ShadowCode 0\.34\.2 · 2\.5 MB/),
  ).toBeTruthy();
  const copies = screen.getByRole("list", {
    name: "Copies made before upgrades",
  });
  expect(within(copies).getByText(/pre-native-0a1b2c/)).toBeTruthy();
  expect(screen.getByText("/home/dev/.config/shadow-agent")).toBeTruthy();
  expect(screen.getByText(/stay the same in all 1\.x versions/)).toBeTruthy();
  expect(screen.getByText(/format 27/)).toBeTruthy();
});

it("backs up without API keys unless asked, and warns when they are included", async () => {
  const onToast = vi.fn();
  render(<DataPage onToast={onToast} />);
  fireEvent.click(await screen.findByRole("button", { name: /Back up now/ }));
  await waitFor(() => expect(posted("/api/data/backups")).toHaveLength(1));
  expect(posted("/api/data/backups")[0].body).toEqual({
    include_secrets: false,
    folder: "",
  });
  await waitFor(() =>
    expect(onToast).toHaveBeenCalledWith(
      expect.stringContaining("Backup saved to"),
      "ok",
    ),
  );
  expect(screen.queryByRole("note")).toBeNull();
  fireEvent.click(
    screen.getByLabelText("Include API keys and remote-access pairing"),
  );
  expect(screen.getByRole("note").textContent).toMatch(
    /Anyone who gets this backup can use your API keys/,
  );
  fireEvent.click(screen.getByRole("button", { name: /Back up now/ }));
  await waitFor(() => expect(posted("/api/data/backups")).toHaveLength(2));
  expect(posted("/api/data/backups")[1].body.include_secrets).toBe(true);
  // The new backup is listed.
  await waitFor(() =>
    expect(
      within(screen.getByRole("list", { name: "Backups" })).getAllByRole(
        "listitem",
      ),
    ).toHaveLength(3),
  );
});

it("previews a backup, schedules its restore and can cancel it", async () => {
  const onToast = vi.fn();
  render(<DataPage onToast={onToast} />);
  const backups = await screen.findByRole("list", { name: "Backups" });
  fireEvent.click(
    within(backups).getByRole("button", { name: /^Restore the backup from/ }),
  );
  const dialog = await screen.findByRole("dialog", {
    name: "Restore this backup?",
  });
  expect(dialog.textContent).toMatch(/12 conversations, 31 tasks, 2 goals/);
  expect(dialog.textContent).toMatch(/Your current data is backed up first/);
  fireEvent.click(
    within(dialog).getByRole("button", { name: "Restore at next start" }),
  );
  await waitFor(() => expect(posted("/api/data/restore")).toHaveLength(1));
  expect(posted("/api/data/restore")[0].body.include_secrets).toBe(false);
  const banner = await screen.findByText(/A restore from .* is scheduled/);
  expect(banner.textContent).toMatch(/next time ShadowCode starts/);
  expect(
    screen.getByRole("button", { name: "Quit ShadowCode now" }),
  ).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel the restore" }));
  await waitFor(() => expect(posted("/api/data/pending")).toHaveLength(1));
  await waitFor(() =>
    expect(screen.queryByText(/A restore from .* is scheduled/)).toBeNull(),
  );
});

it("offers API keys only when the backup has them, and refuses a damaged one", async () => {
  fake.state.data.pick = "/media/usb/shadowcode-backup-20260926-080000";
  render(<DataPage onToast={vi.fn()} />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Restore from another folder…" }),
  );
  const dialog = await screen.findByRole("dialog", {
    name: "Restore this backup?",
  });
  fireEvent.click(
    within(dialog).getByLabelText("Also restore the API keys in this backup"),
  );
  fireEvent.click(
    within(dialog).getByRole("button", { name: "Restore at next start" }),
  );
  await waitFor(() => expect(posted("/api/data/restore")).toHaveLength(1));
  expect(posted("/api/data/restore")[0].body).toEqual({
    path: "/media/usb/shadowcode-backup-20260926-080000",
    include_secrets: true,
  });

  fake.state.data.pick = "/media/usb/damaged-backup";
  fireEvent.click(
    await screen.findByRole("button", { name: "Restore from another folder…" }),
  );
  const refused = await screen.findByRole("dialog", {
    name: "This backup can't be restored",
  });
  expect(
    within(refused).getByRole("list", { name: "Problems" }).textContent,
  ).toMatch(/config\/config.yaml is missing/);
  fireEvent.click(within(refused).getByRole("button", { name: "Close" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  expect(posted("/api/data/restore")).toHaveLength(1);
});

it("schedules a reset only after confirming, and shows repair results", async () => {
  render(<DataPage onToast={vi.fn()} />);
  fireEvent.click(
    await screen.findByRole("button", { name: "Reset ShadowCode…" }),
  );
  const dialog = await screen.findByRole("dialog", {
    name: "Reset ShadowCode?",
  });
  expect(dialog.textContent).toMatch(/Nothing is deleted/);
  expect(dialog.textContent).toMatch(/managed-worktrees/);
  fireEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
  expect(posted("/api/data/reset")).toHaveLength(0);
  fireEvent.click(screen.getByRole("button", { name: "Reset ShadowCode…" }));
  fireEvent.click(
    within(
      await screen.findByRole("dialog", { name: "Reset ShadowCode?" }),
    ).getByRole("button", { name: "Reset at next start" }),
  );
  await waitFor(() => expect(posted("/api/data/reset")).toHaveLength(1));
  expect(posted("/api/data/reset")[0].body).toEqual({ confirm: "reset" });
  expect(await screen.findByText(/A reset is scheduled/)).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: /Check and repair/ }));
  const results = await screen.findByRole("list", {
    name: "Check and repair results",
  });
  expect(within(results).getByText("Database integrity")).toBeTruthy();
  expect(within(results).getByText("No damage found")).toBeTruthy();
});

it("formats sizes and summaries in plain words", () => {
  expect(formatBytes(512)).toBe("512 bytes");
  expect(formatBytes(2_480_000)).toBe("2.5 MB");
  expect(formatBytes(12_400_000)).toBe("12 MB");
  expect(
    describeSummary({
      conversations: 1,
      tasks: 3,
      jobs: 3,
      goals: 0,
      automations: 0,
      comparisons: 0,
      last_activity: null,
    }),
  ).toBe("It holds 1 conversation, 3 tasks.");
  expect(describeSummary(null)).toBe("Its contents could not be read.");
});
