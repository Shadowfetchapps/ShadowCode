import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api, type AboutInfo, type UpdateStatus } from "../../api";
import { AboutPage, updateSummary } from "./AboutPage";
import { UPDATES_CHANGED } from "../../hooks/useUpdates";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

const repo = "https://github.com/Shadowfetchapps/ShadowCode";

function updates(overrides: Partial<UpdateStatus> = {}): UpdateStatus {
  return {
    current: "0.33.1",
    allowed: true,
    automatic: true,
    setting: null,
    default_on: true,
    offline: false,
    policy_message: null,
    policy_source: null,
    install: { kind: "appimage", label: "AppImage" },
    last_checked_at: null,
    last_attempt_at: null,
    error: null,
    latest: null,
    available: false,
    dismissed: false,
    next_step: null,
    releases_url: `${repo}/releases`,
    ...overrides,
  };
}

const newer = {
  available: true,
  latest: {
    version: "0.34.0",
    tag: "v0.34.0",
    url: `${repo}/releases/tag/v0.34.0`,
    published_at: "2026-10-02T09:15:00Z",
    signed: true,
  },
  next_step: {
    text: "Download ShadowCode_0.34.0_amd64.AppImage and its four signature files.",
    command:
      "bash /path/to/trusted-bundle/scripts/install-appimage.sh ~/Downloads/ShadowCode_0.34.0_amd64.AppImage",
    link: `${repo}/blob/v0.34.0/README.md#appimage-recommended`,
  },
} satisfies Partial<UpdateStatus>;

function about(status: UpdateStatus = updates()): AboutInfo {
  return {
    name: "ShadowCode",
    version: "0.33.1",
    commit: "e15c4480e65db5650af012bb2a9773dbe89acf84",
    install: status.install,
    license: {
      spdx: "Apache-2.0",
      name: "Apache License 2.0",
      holder: "Shadowfetch",
      notice:
        "ShadowCode\nCopyright 2026 Shadowfetch\n\nShadowCode was originally created by Shadowfetch.\n",
      third_party: "/usr/share/doc/shadowcode/notices",
    },
    links: {
      repository: repo,
      release_notes: `${repo}/releases/tag/v0.33.1`,
      releases: `${repo}/releases`,
      license: `${repo}/blob/v0.33.1/LICENSE`,
      notice: `${repo}/blob/v0.33.1/NOTICE`,
      issues: `${repo}/issues`,
      user_guide: `${repo}/blob/v0.33.1/docs/USER_GUIDE.md`,
    },
    updates: status,
  };
}

const noop = () => undefined;

it("shows version, commit, install type, license and links", async () => {
  vi.spyOn(api, "about").mockResolvedValue(about());
  render(<AboutPage onSave={async () => undefined} onToast={noop} />);
  expect(await screen.findByText("0.33.1")).toBeTruthy();
  const commit = screen.getByText("e15c4480e65d");
  expect(commit.getAttribute("title")).toBe(
    "e15c4480e65db5650af012bb2a9773dbe89acf84",
  );
  expect(screen.getByText("AppImage")).toBeTruthy();
  expect(screen.getByText("Apache License 2.0")).toBeTruthy();
  expect(
    screen.getByText(/originally created by Shadowfetch\. If you share/),
  ).toBeTruthy();
  expect(screen.getByText("/usr/share/doc/shadowcode/notices")).toBeTruthy();
  const links = within(
    screen.getByRole("navigation", { name: "ShadowCode links" }),
  );
  expect(
    links.getByRole("link", { name: "Release notes" }).getAttribute("href"),
  ).toBe(`${repo}/releases/tag/v0.33.1`);
  expect(
    links.getByRole("link", { name: "License" }).getAttribute("href"),
  ).toBe(`${repo}/blob/v0.33.1/LICENSE`);
  expect(
    links.getByRole("link", { name: "Report a problem" }).getAttribute("href"),
  ).toBe(`${repo}/issues`);
  expect(screen.getByRole("status").textContent).toBe(
    "Not checked yet. ShadowCode checks once a day.",
  );
});

it("says when a build has no recorded commit", async () => {
  vi.spyOn(api, "about").mockResolvedValue({ ...about(), commit: null });
  render(<AboutPage onSave={async () => undefined} onToast={noop} />);
  expect(await screen.findByText("Not recorded in this build")).toBeTruthy();
});

it("shows a newer release with its notes and the install steps", async () => {
  vi.spyOn(api, "about").mockResolvedValue(about(updates(newer)));
  const dismissed = updates({ ...newer, dismissed: true });
  const dismiss = vi.spyOn(api, "dismissUpdate").mockResolvedValue(dismissed);
  const announced = vi.fn();
  window.addEventListener(UPDATES_CHANGED, announced);
  render(<AboutPage onSave={async () => undefined} onToast={noop} />);
  const card = within(
    await screen.findByRole("region", { name: "ShadowCode 0.34.0" }),
  );
  expect(screen.getByRole("status").textContent).toBe(
    "ShadowCode 0.34.0 is available. You have 0.33.1.",
  );
  expect(
    card.getByRole("link", { name: "Release notes" }).getAttribute("href"),
  ).toBe(`${repo}/releases/tag/v0.34.0`);
  expect(card.getByText(/four signature files/)).toBeTruthy();
  expect(card.getByText(newer.next_step.command)).toBeTruthy();
  expect(
    card
      .getByRole("link", { name: "Install instructions" })
      .getAttribute("href"),
  ).toBe(newer.next_step.link);
  fireEvent.click(
    card.getByRole("button", {
      name: "Hide the notice until the next version",
    }),
  );
  await waitFor(() => expect(dismiss).toHaveBeenCalledWith("0.34.0"));
  await waitFor(() =>
    expect(
      card.queryByRole("button", {
        name: "Hide the notice until the next version",
      }),
    ).toBeNull(),
  );
  expect(announced).toHaveBeenCalled();
  window.removeEventListener(UPDATES_CHANGED, announced);
});

it("copies the install command", async () => {
  vi.spyOn(api, "about").mockResolvedValue(about(updates(newer)));
  const writeText = vi.fn().mockResolvedValue(undefined);
  Object.defineProperty(navigator, "clipboard", {
    value: { writeText },
    configurable: true,
  });
  const toast = vi.fn();
  render(<AboutPage onSave={async () => undefined} onToast={toast} />);
  fireEvent.click(await screen.findByRole("button", { name: "Copy command" }));
  await waitFor(() =>
    expect(writeText).toHaveBeenCalledWith(newer.next_step.command),
  );
  expect(toast).toHaveBeenCalledWith("Command copied", "ok");
});

it("turns the daily check off through the settings save", async () => {
  vi.spyOn(api, "about").mockResolvedValue(about());
  vi.spyOn(api, "updates").mockResolvedValue(
    updates({ automatic: false, setting: false }),
  );
  const save = vi.fn().mockResolvedValue(undefined);
  render(<AboutPage onSave={save} onToast={noop} />);
  const toggle = await screen.findByLabelText<HTMLInputElement>(
    "Check for updates once a day",
  );
  expect(toggle.checked).toBe(true);
  fireEvent.click(toggle);
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith({ updates: { check: false } }),
  );
  await waitFor(() => expect(toggle.checked).toBe(false));
});

it("checks on demand and reports what it found", async () => {
  vi.spyOn(api, "about").mockResolvedValue(about());
  const check = vi
    .spyOn(api, "checkUpdates")
    .mockResolvedValue(updates({ last_checked_at: Date.now() / 1000 - 5 }));
  render(<AboutPage onSave={async () => undefined} onToast={noop} />);
  fireEvent.click(await screen.findByRole("button", { name: "Check now" }));
  await waitFor(() => expect(check).toHaveBeenCalled());
  await waitFor(() =>
    expect(screen.getByRole("status").textContent).toBe(
      "You have the latest version (checked just now).",
    ),
  );
});

it("shows a failed manual check as a message", async () => {
  vi.spyOn(api, "about").mockResolvedValue(about());
  vi.spyOn(api, "checkUpdates").mockRejectedValue(
    new Error("ShadowCode is in Offline mode."),
  );
  const toast = vi.fn();
  render(<AboutPage onSave={async () => undefined} onToast={toast} />);
  fireEvent.click(await screen.findByRole("button", { name: "Check now" }));
  await waitFor(() =>
    expect(toast).toHaveBeenCalledWith("ShadowCode is in Offline mode.", "err"),
  );
});

it("offline mode disables Check now and says why", async () => {
  vi.spyOn(api, "about").mockResolvedValue(about(updates({ offline: true })));
  render(<AboutPage onSave={async () => undefined} onToast={noop} />);
  const button = await screen.findByRole<HTMLButtonElement>("button", {
    name: "Check now",
  });
  expect(button.disabled).toBe(true);
  expect(screen.getByRole("status").textContent).toBe(
    "Offline mode is on, so ShadowCode doesn't check for updates.",
  );
});

it("a distribution that turned checks off hides the switch and the button", async () => {
  const managed = updates({
    allowed: false,
    automatic: false,
    install: { kind: "deb", label: "Debian package" },
    policy_message:
      "ShadowCode updates arrive with Shadowfetch Linux system updates.",
    policy_source: "/etc/shadowcode/policy.yaml",
  });
  vi.spyOn(api, "about").mockResolvedValue(about(managed));
  render(<AboutPage onSave={async () => undefined} onToast={noop} />);
  expect(await screen.findByText("Debian package")).toBeTruthy();
  expect(screen.getByRole("status").textContent).toBe(
    "ShadowCode updates arrive with Shadowfetch Linux system updates.",
  );
  expect(screen.queryByLabelText("Check for updates once a day")).toBeNull();
  expect(screen.queryByRole("button", { name: "Check now" })).toBeNull();
  expect(
    screen.getByText("ShadowCode does not contact GitHub for updates."),
  ).toBeTruthy();
});

it("summarises every update state in one sentence", () => {
  const now = 1_800_000_000;
  expect(updateSummary(updates({ allowed: false }))).toBe(
    "Update checks are turned off for this installation.",
  );
  expect(updateSummary(updates({ error: "Couldn't reach GitHub" }))).toBe(
    "The last check didn't work: Couldn't reach GitHub.",
  );
  expect(updateSummary(updates({ last_checked_at: now - 7200 }), now)).toBe(
    "You have the latest version (checked 2h ago).",
  );
  expect(updateSummary(updates({ automatic: false }))).toBe("Not checked yet.");
  // A release found earlier is still shown while offline.
  expect(updateSummary(updates({ ...newer, offline: true }))).toBe(
    "ShadowCode 0.34.0 is available. You have 0.33.1.",
  );
});
