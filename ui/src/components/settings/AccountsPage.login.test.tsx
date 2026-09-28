import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { installFakeBackend } from "../../../e2e/fakeBackend";
import { AccountsPage } from "./AccountsPage";
import { api } from "../../api";
import { Settings } from "../Settings";

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => {
    resolve = yes;
    reject = no;
  });
  return { promise, resolve, reject };
}

let fake: ReturnType<typeof installFakeBackend>;
let wake: () => void;
let progress: Record<string, unknown>;
let cancel: ReturnType<typeof deferred<unknown>>;
let cancelCalls: string[];
let progressCalls: string[];
let connected: Set<string>;
beforeEach(() => {
  vi.stubEnv("VITE_SHADOW_TEST_TRANSPORT", "1");
  fake = installFakeBackend({ stepMs: 5 });
  Object.assign(fake.state.vendors.grok, {
    state: "not_logged_in",
    availability: "sign_in",
    availability_label: "Sign in",
  });
  progress = {
    claude: { running: true, lines: [], done: null },
    grok: { running: true, lines: [], done: null },
  };
  cancel = deferred<unknown>();
  cancelCalls = [];
  progressCalls = [];
  connected = new Set();
  const listeners = new Set<() => void>();
  vi.spyOn(fake.bridge, "listen").mockImplementation(
    async (_event, handler) => {
      const callback = () => handler({});
      listeners.add(callback);
      return () => {
        listeners.delete(callback);
      };
    },
  );
  wake = () => {
    for (const listener of listeners) listener();
  };
  const request = fake.bridge.request.bind(fake.bridge);
  vi.spyOn(fake.bridge, "request").mockImplementation(
    async (path, method, body) => {
      const match =
        /^\/api\/accounts\/([^/]+)\/(login|cancel-login|connect)$/.exec(path);
      if (match?.[2] === "login") {
        progressCalls.push(match[1]);
        return connected.has(match[1])
          ? progress[match[1]]
          : { running: false, lines: [], done: null };
      }
      if (match?.[2] === "connect") connected.add(match[1]);
      if (match?.[2] === "cancel-login") {
        cancelCalls.push(match[1]);
        return cancel.promise;
      }
      return request(path, method, body);
    },
  );
});

it("recovers two active vendor logins independently without another Connect request", async () => {
  connected.add("claude");
  connected.add("grok");
  progress.claude = {
    running: true,
    lines: ["Claude device code: AB-CD"],
    done: null,
  };
  progress.grok = {
    running: true,
    lines: ["Grok device code: EF-GH"],
    done: null,
  };
  const onChanged = vi.fn();
  render(<AccountsPage onChanged={onChanged} onToast={vi.fn()} />);
  const claude = await screen.findByRole("article", { name: "Claude Code" });
  const grok = await screen.findByRole("article", { name: "Grok" });
  await within(claude).findByText("Claude device code: AB-CD");
  await within(grok).findByText("Grok device code: EF-GH");
  expect(
    within(claude).getByRole("button", { name: "Cancel sign-in" }),
  ).toBeTruthy();
  expect(
    within(grok).getByRole("button", { name: "Cancel sign-in" }),
  ).toBeTruthy();
  fireEvent.click(
    within(claude).getByRole("button", { name: "Cancel sign-in" }),
  );
  expect(cancelCalls).toEqual(["claude"]);
  expect(
    within(grok).getByRole("button", { name: "Cancel sign-in" }),
  ).toHaveProperty("disabled", false);
  progress.grok = {
    running: false,
    lines: [],
    done: { ok: true, availability: "ready" },
  };
  await act(async () => wake());
  await within(grok).findByText("Signed in.");
  expect(within(claude).getByText("Stopping sign-in…")).toBeTruthy();
  expect(onChanged).not.toHaveBeenCalled();
  progress.claude = {
    running: false,
    lines: [],
    done: { ok: false, detail: "Claude stopped" },
  };
  await act(async () => wake());
  await within(claude).findByText("Claude stopped");
  await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  expect(
    fake.log.filter((r) => r.method === "POST" && r.path.endsWith("/connect")),
  ).toHaveLength(0);
});

it("recovers buffered sign-in instructions after Accounts unmounts and remounts", async () => {
  const { card } = await start();
  progress.claude = {
    running: true,
    lines: ["Continue at https://auth.example.test/device"],
    done: null,
  };
  await act(async () => wake());
  await within(card).findByRole("button", { name: "Cancel sign-in" });
  cleanup();
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const reopened = await screen.findByRole("article", { name: "Claude Code" });
  await within(reopened).findByRole("button", { name: "Cancel sign-in" });
  expect(within(reopened).getByText(/auth.example.test/)).toBeTruthy();
  expect(
    fake.log.filter((r) => r.method === "POST" && r.path.endsWith("/connect")),
  ).toHaveLength(1);
});

it("does not let a late recovery read replace a newly started attempt", async () => {
  const recovery = deferred<Awaited<ReturnType<typeof api.loginProgress>>>();
  const read = api.loginProgress;
  let first = true;
  vi.spyOn(api, "loginProgress").mockImplementation((vendor) => {
    if (vendor === "claude" && first) {
      first = false;
      return recovery.promise;
    }
    return read(vendor);
  });
  const { card, onChanged } = await start();
  progress.claude = {
    running: true,
    lines: ["New attempt instructions"],
    done: null,
  };
  await act(async () => wake());
  await within(card).findByText("New attempt instructions");
  await act(async () =>
    recovery.resolve({
      running: true,
      lines: ["Obsolete recovery instructions"],
      done: null,
    }),
  );
  expect(within(card).queryByText("Obsolete recovery instructions")).toBeNull();
  expect(within(card).getByText("New attempt instructions")).toBeTruthy();
  expect(onChanged).not.toHaveBeenCalled();
});

it("restores Stopping after remount and waits for terminal account evidence without probing", async () => {
  connected.add("claude");
  progress.claude = {
    running: true,
    cancellation_requested: true,
    lines: ["Waiting for owned login cleanup"],
    done: null,
  };
  const onChanged = vi.fn();
  render(<AccountsPage onChanged={onChanged} onToast={vi.fn()} />);
  const card = await screen.findByRole("article", { name: "Claude Code" });
  await within(card).findByText("Stopping sign-in…");
  expect(
    within(card).getByRole("button", { name: "Stopping…" }),
  ).toHaveProperty("disabled", true);
  expect(
    within(card).getByRole("button", { name: "Cancel sign-in" }),
  ).toHaveProperty("disabled", true);
  expect(cancelCalls).toEqual([]);
  expect(
    fake.log.some(
      (r) => r.path === "/api/accounts" || r.path.endsWith("/refresh"),
    ),
  ).toBe(false);
  progress.claude = {
    running: false,
    cancellation_requested: false,
    lines: [],
    done: {
      ok: false,
      detail: "Stopped after cleanup",
      availability: "unavailable",
    },
  };
  await act(async () => wake());
  await within(card).findByText("Stopped after cleanup");
  expect(within(card).getByRole("button", { name: "Connect" })).toHaveProperty(
    "disabled",
    false,
  );
  expect(onChanged).not.toHaveBeenCalled();
});

it("recovers sign-in controls through rendered Settings section navigation", async () => {
  connected.add("claude");
  progress.claude = {
    running: true,
    lines: ["Device code: KEEP-ME"],
    done: null,
  };
  render(
    <Settings
      cfg={{}}
      health={null}
      sessionId="s1"
      busy={false}
      onClose={vi.fn()}
      onSave={vi.fn(async () => {})}
      onCatalogChanged={vi.fn()}
      onToast={vi.fn()}
      onOpenSession={vi.fn()}
      onSkillsChanged={vi.fn(async () => {})}
      onUseSkill={vi.fn()}
    />,
  );
  const settings = screen.getByRole("dialog", { name: "Settings" });
  const card = await within(settings).findByRole("article", {
    name: "Claude Code",
  });
  await within(card).findByText("Device code: KEEP-ME");
  fireEvent.click(within(settings).getByRole("button", { name: "Appearance" }));
  expect(
    within(settings).queryByRole("article", { name: "Claude Code" }),
  ).toBeNull();
  fireEvent.click(within(settings).getByRole("button", { name: "Accounts" }));
  const restored = await within(settings).findByRole("article", {
    name: "Claude Code",
  });
  await within(restored).findByRole("button", { name: "Cancel sign-in" });
  expect(within(restored).getByText("Device code: KEEP-ME")).toBeTruthy();
  expect(
    fake.log.some((r) => r.method === "POST" && r.path.endsWith("/connect")),
  ).toBe(false);
});

it("ignores an old mounted page's recovery response after it is closed", async () => {
  const recovery = deferred<Awaited<ReturnType<typeof api.loginProgress>>>();
  const read = api.loginProgress;
  let first = true;
  vi.spyOn(api, "loginProgress").mockImplementation((vendor) => {
    if (vendor === "claude" && first) {
      first = false;
      return recovery.promise;
    }
    return read(vendor);
  });
  const oldChanged = vi.fn();
  const firstPage = render(
    <AccountsPage onChanged={oldChanged} onToast={vi.fn()} />,
  );
  await screen.findByRole("article", { name: "Claude Code" });
  firstPage.unmount();
  connected.add("claude");
  progress.claude = {
    running: true,
    lines: ["Current page instructions"],
    done: null,
  };
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  await screen.findByText("Current page instructions");
  await act(async () =>
    recovery.resolve({
      running: true,
      lines: ["Old page instructions"],
      done: null,
    }),
  );
  expect(screen.queryByText("Old page instructions")).toBeNull();
  expect(screen.getByText("Current page instructions")).toBeTruthy();
  expect(oldChanged).not.toHaveBeenCalled();
});

it("keeps cached account cards without probing when a recovery read is unavailable", async () => {
  const read = api.loginProgress;
  vi.spyOn(api, "loginProgress").mockImplementation((vendor) =>
    vendor === "claude"
      ? Promise.reject(new Error("Login status temporarily unavailable"))
      : read(vendor),
  );
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const card = await screen.findByRole("article", { name: "Claude Code" });
  await act(async () => {});
  expect(within(card).getByText("Sign in")).toBeTruthy();
  expect(fake.log.some((r) => r.path === "/api/accounts?cached=1")).toBe(true);
  expect(fake.log.some((r) => r.path === "/api/accounts")).toBe(false);
  fireEvent.click(within(card).getByRole("button", { name: "Refresh" }));
  await waitFor(() =>
    expect(
      fake.log.some((r) => r.path === "/api/accounts/claude/refresh"),
    ).toBe(true),
  );
});

it.each(["failed", "finished"])(
  "defers a completed vendor's catalog refresh while another Connect is pending (%s)",
  async (outcome) => {
    const { card, onChanged } = await start();
    const pending = deferred<Awaited<ReturnType<typeof api.connectAccount>>>();
    const connect = api.connectAccount;
    vi.spyOn(api, "connectAccount").mockImplementation((vendor) =>
      vendor === "grok" ? pending.promise : connect(vendor),
    );
    const grok = screen.getByRole("article", { name: "Grok" });
    fireEvent.click(within(grok).getByRole("button", { name: "Connect" }));
    progress.claude = {
      running: false,
      lines: [],
      done: { ok: true, availability: "ready" },
    };
    await act(async () => wake());
    await within(card).findByText("Signed in.");
    expect(onChanged).not.toHaveBeenCalled();
    if (outcome === "failed")
      await act(async () => pending.reject(new Error("Grok startup failed")));
    else {
      connected.add("grok");
      progress.grok = {
        running: false,
        lines: [],
        done: { ok: false, detail: "Grok stopped" },
      };
      await act(async () => pending.resolve({ ok: true, state: "started" }));
      await within(grok).findByText("Grok stopped");
    }
    await waitFor(() => expect(onChanged).toHaveBeenCalledTimes(1));
  },
);
afterEach(() => {
  cleanup();
  cancel.resolve({ ok: true });
  vi.restoreAllMocks();
  vi.unstubAllEnvs();
  delete window.__SHADOW_TEST_TRANSPORT__;
});

async function start() {
  const onToast = vi.fn();
  const onChanged = vi.fn();
  render(<AccountsPage onChanged={onChanged} onToast={onToast} />);
  const card = await screen.findByRole("article", { name: "Claude Code" });
  fireEvent.click(within(card).getByRole("button", { name: "Connect" }));
  await within(card).findByRole("button", { name: "Cancel sign-in" });
  await waitFor(() => expect(progressCalls).toContain("claude"));
  return { card, onToast, onChanged };
}

it("keeps a cancelled login stopping and polls until the engine reports its terminal state", async () => {
  const { card, onChanged } = await start();
  const button = within(card).getByRole("button", { name: "Cancel sign-in" });
  fireEvent.click(button);
  expect(await within(card).findByText("Stopping sign-in…")).toBeTruthy();
  expect(button).toHaveProperty("disabled", true);
  fireEvent.click(button);
  expect(cancelCalls).toEqual(["claude"]);
  await act(async () => cancel.resolve({ ok: true }));
  const before = progressCalls.length;
  await act(async () => wake());
  await waitFor(() => expect(progressCalls.length).toBeGreaterThan(before));
  expect(within(card).getByText("Stopping sign-in…")).toBeTruthy();
  expect(within(card).queryByText("Sign-in cancelled.")).toBeNull();
  expect(
    within(card).getByRole("button", { name: "Stopping…" }),
  ).toHaveProperty("disabled", true);
  progress.claude = {
    running: false,
    lines: [],
    done: { ok: false, detail: "Sign-in cancelled after cleanup" },
  };
  await act(async () => wake());
  expect(
    await within(card).findByText("Sign-in cancelled after cleanup"),
  ).toBeTruthy();
  await waitFor(() =>
    expect(
      within(card).getByRole("button", { name: "Connect" }),
    ).toHaveProperty("disabled", false),
  );
  expect(onChanged).not.toHaveBeenCalled();
});

it("restores cancellation controls when the request fails and keeps following the login", async () => {
  const { card, onToast } = await start();
  fireEvent.click(within(card).getByRole("button", { name: "Cancel sign-in" }));
  expect(await within(card).findByText("Stopping sign-in…")).toBeTruthy();
  await act(async () => cancel.reject(new Error("Cancel transport failed")));
  await waitFor(() =>
    expect(onToast).toHaveBeenCalledWith(
      expect.stringContaining("Cancel transport failed"),
      "err",
    ),
  );
  expect(
    within(card).getByRole("button", { name: "Cancel sign-in" }),
  ).toHaveProperty("disabled", false);
  expect(within(card).queryByText("Stopping sign-in…")).toBeNull();
  progress.claude = {
    running: false,
    lines: [],
    done: { ok: false, detail: "Sign-in timed out" },
  };
  await act(async () => wake());
  expect(await within(card).findByText("Sign-in timed out")).toBeTruthy();
});

it("a late cancel response cannot overwrite a terminal login result", async () => {
  const { card } = await start();
  fireEvent.click(within(card).getByRole("button", { name: "Cancel sign-in" }));
  progress.claude = {
    running: false,
    lines: [],
    done: { ok: false, detail: "Sign-in timed out" },
  };
  await act(async () => wake());
  expect(await within(card).findByText("Sign-in timed out")).toBeTruthy();
  await act(async () => cancel.resolve({ ok: true }));
  expect(within(card).getByText("Sign-in timed out")).toBeTruthy();
});

it("a late cancel response for another vendor cannot replace the current login", async () => {
  const { card } = await start();
  fireEvent.click(within(card).getByRole("button", { name: "Cancel sign-in" }));
  const grok = screen.getByRole("article", { name: "Grok" });
  fireEvent.click(within(grok).getByRole("button", { name: "Connect" }));
  await within(grok).findByRole("button", { name: "Cancel sign-in" });
  await act(async () => cancel.resolve({ ok: true }));
  expect(
    within(grok).getByRole("button", { name: "Cancel sign-in" }),
  ).toBeTruthy();
  expect(within(grok).queryByText("Sign-in cancelled.")).toBeNull();
});

it.each(["sign_in", "unavailable", undefined])(
  "does not claim signed in when command success reports %s account availability",
  async (availability) => {
    progress.claude = {
      running: false,
      lines: [],
      done: { ok: true, detail: "The sign-in command finished", availability },
    };
    render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
    const card = await screen.findByRole("article", { name: "Claude Code" });
    fireEvent.click(within(card).getByRole("button", { name: "Connect" }));
    expect(
      await within(card).findByText(
        "Sign-in command finished. Account status is not confirmed.",
      ),
    ).toBeTruthy();
    expect(within(card).queryByText("Signed in.")).toBeNull();
  },
);

it("confirms sign-in when the final account observation is Ready", async () => {
  progress.claude = {
    running: false,
    lines: [],
    done: { ok: true, availability: "ready" },
  };
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const card = await screen.findByRole("article", { name: "Claude Code" });
  fireEvent.click(within(card).getByRole("button", { name: "Connect" }));
  expect(await within(card).findByText("Signed in.")).toBeTruthy();
});

it.each([false, true])(
  "finishes an unconfirmed login (command ok=%s) without starting another provider probe",
  async (ok) => {
    const heldRefresh =
      deferred<Awaited<ReturnType<typeof api.refreshAccount>>>();
    const refresh = vi
      .spyOn(api, "refreshAccount")
      .mockImplementation(() => heldRefresh.promise);
    const { card, onChanged } = await start();
    progress.claude = {
      running: false,
      lines: [],
      done: {
        ok,
        detail: "Sign-in cancelled after cleanup",
        availability: "unavailable",
      },
    };
    await act(async () => wake());
    await within(card).findByText(
      ok
        ? "Sign-in command finished. Account status is not confirmed."
        : "Sign-in cancelled after cleanup",
    );
    expect(refresh).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(
        fake.log.some((request) => request.path === "/api/accounts?cached=1"),
      ).toBe(true),
    );
    expect(
      within(card).getByRole("button", { name: "Connect" }),
    ).toHaveProperty("disabled", false);
    expect(onChanged).not.toHaveBeenCalled();
  },
);

it("the actual fake account handlers preserve cancelled completion for the polling UI", async () => {
  vi.restoreAllMocks();
  fake = installFakeBackend({ stepMs: 5 });
  render(<AccountsPage onChanged={vi.fn()} onToast={vi.fn()} />);
  const card = await screen.findByRole("article", { name: "Claude Code" });
  fireEvent.click(within(card).getByRole("button", { name: "Connect" }));
  const button = await within(card).findByRole("button", {
    name: "Cancel sign-in",
  });
  fireEvent.click(button);
  expect(
    await within(card).findByText("Sign-in cancelled", {}, { timeout: 2500 }),
  ).toBeTruthy();
  await waitFor(() =>
    expect(
      within(card).getByRole("button", { name: "Connect" }),
    ).toHaveProperty("disabled", false),
  );
  expect(
    fake.log.filter((request) => request.path.endsWith("/cancel-login")),
  ).toHaveLength(1);
  expect(fake.log.some((request) => request.path.endsWith("/refresh"))).toBe(
    false,
  );
});
