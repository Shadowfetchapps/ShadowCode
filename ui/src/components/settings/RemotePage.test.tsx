import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import { api, type RemoteStatus } from "../../api";
import { RemotePage, randomTopic } from "./RemotePage";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function status(overrides: Partial<RemoteStatus> = {}): RemoteStatus {
  return {
    enabled: false,
    running: false,
    address: "127.0.0.1",
    port: 7390,
    bound: null,
    url: null,
    public_url: "",
    exposed: false,
    allow_terminals: false,
    error: null,
    addresses: [
      { address: "127.0.0.1", interface: "lo", kind: "loopback" },
      {
        address: "100.101.102.103",
        interface: "tailscale0",
        kind: "tailscale",
      },
      { address: "192.168.1.5", interface: "wlan0", kind: "lan" },
    ],
    devices: [],
    ntfy: {
      server: "",
      topic: "",
      details: false,
      events: { approval: true, finished: true, failed: true, limit: true },
      token_saved: false,
      configured: false,
      restored: false,
      error: null,
    },
    ...overrides,
  };
}

it("is off by default and turns on through the API", async () => {
  vi.spyOn(api, "remoteStatus").mockResolvedValue(status());
  const save = vi.spyOn(api, "saveRemote").mockResolvedValue(
    status({
      enabled: true,
      running: true,
      bound: "127.0.0.1:7390",
      url: "http://127.0.0.1:7390",
    }),
  );
  render(<RemotePage onToast={() => undefined} />);
  const toggle = await screen.findByLabelText<HTMLInputElement>(
    "Turn on remote access",
  );
  expect(toggle.checked).toBe(false);
  expect(
    screen.getByText("Turn on remote access to pair a phone."),
  ).toBeTruthy();
  expect(
    screen.getByLabelText<HTMLInputElement>(
      "Allow terminals over remote access",
    ).checked,
  ).toBe(false);
  fireEvent.click(toggle);
  await waitFor(() => expect(save).toHaveBeenCalledWith({ enabled: true }));
  expect(await screen.findByText("http://127.0.0.1:7390")).toBeTruthy();
});

it("warns about plain HTTP on a network address", async () => {
  vi.spyOn(api, "remoteStatus").mockResolvedValue(
    status({
      enabled: true,
      running: true,
      address: "192.168.1.5",
      exposed: true,
    }),
  );
  render(<RemotePage onToast={() => undefined} />);
  expect(await screen.findByText(/Plain HTTP is not encrypted/)).toBeTruthy();
});

it("pairs a device with a QR code and unpairs devices", async () => {
  vi.spyOn(api, "remoteStatus").mockResolvedValue(
    status({
      enabled: true,
      running: true,
      url: "http://127.0.0.1:7390",
      devices: [
        {
          id: "d1",
          name: "Safari on iPhone or iPad",
          created_at: 1,
          last_seen: null,
          restored: false,
        },
      ],
    }),
  );
  vi.spyOn(api, "pairRemote").mockResolvedValue({
    link: "http://127.0.0.1:7390/#pair=abc",
    base: "http://127.0.0.1:7390",
    expires_in: 600,
    qr: { size: 2, rows: ["10", "01"] },
  });
  const revoke = vi
    .spyOn(api, "revokeRemote")
    .mockResolvedValue(status({ enabled: true, running: true }));
  render(<RemotePage onToast={() => undefined} />);
  fireEvent.click(await screen.findByRole("button", { name: "Pair a device" }));
  expect(
    await screen.findByRole("img", { name: "Pairing QR code" }),
  ).toBeTruthy();
  expect(screen.getByText("http://127.0.0.1:7390/#pair=abc")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Unpair" }));
  await waitFor(() => expect(revoke).toHaveBeenCalledWith("d1"));
});

it("saves ntfy settings only when entered, and never shows the token", async () => {
  vi.spyOn(api, "remoteStatus").mockResolvedValue(status());
  const save = vi.spyOn(api, "saveNtfy").mockResolvedValue(
    status({
      ntfy: {
        ...status().ntfy,
        server: "https://ntfy.example",
        topic: "t1",
        token_saved: true,
        configured: true,
      },
    }),
  );
  render(<RemotePage onToast={() => undefined} />);
  const test = await screen.findByRole<HTMLButtonElement>("button", {
    name: "Send a test",
  });
  expect(test.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Server"), {
    target: { value: "https://ntfy.example" },
  });
  fireEvent.change(screen.getByLabelText("Topic"), { target: { value: "t1" } });
  fireEvent.change(screen.getByLabelText("Access token (optional)"), {
    target: { value: "tk_secret" },
  });
  const buttons = screen.getAllByRole("button", { name: "Save" });
  fireEvent.click(buttons[buttons.length - 1]);
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith({
      server: "https://ntfy.example",
      topic: "t1",
      token: "tk_secret",
    }),
  );
  await waitFor(() =>
    expect(
      screen.getByLabelText<HTMLInputElement>("Access token (optional)").value,
    ).toBe(""),
  );
  expect(randomTopic()).toMatch(/^shadowcode-[0-9a-z]{20}$/);
});

it("marks what came back with a restore until it is confirmed", async () => {
  const restored = status({
    devices: [
      {
        id: "d1",
        name: "Lost phone",
        created_at: 1,
        last_seen: null,
        restored: true,
      },
    ],
    ntfy: {
      ...status().ntfy,
      server: "https://ntfy.example",
      topic: "old-topic",
      configured: true,
      restored: true,
    },
  });
  vi.spyOn(api, "remoteStatus").mockResolvedValue(restored);
  const save = vi.spyOn(api, "saveNtfy").mockResolvedValue(
    status({
      ...restored,
      ntfy: { ...restored.ntfy, restored: false },
    }),
  );
  render(<RemotePage onToast={() => undefined} />);
  expect(await screen.findByText(/^From a backup · Paired/)).toBeTruthy();
  expect(
    screen.getByText(/They can’t connect until you turn on remote access/),
  ).toBeTruthy();
  expect(
    screen.getByText(/Nothing is sent until you check them and choose Save/),
  ).toBeTruthy();
  // Saving the unchanged server and topic confirms them.
  const buttons = screen.getAllByRole<HTMLButtonElement>("button", {
    name: "Save",
  });
  const saveButton = buttons[buttons.length - 1];
  expect(saveButton.disabled).toBe(false);
  fireEvent.click(saveButton);
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith({
      server: "https://ntfy.example",
      topic: "old-topic",
    }),
  );
  await waitFor(() =>
    expect(
      screen.queryByText(/Nothing is sent until you check them/),
    ).toBeNull(),
  );
});
