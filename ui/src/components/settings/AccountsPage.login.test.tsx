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
      const match = /^\/api\/accounts\/([^/]+)\/(login|cancel-login)$/.exec(
        path,
      );
      if (match?.[2] === "login") {
        progressCalls.push(match[1]);
        return progress[match[1]];
      }
      if (match?.[2] === "cancel-login") {
        cancelCalls.push(match[1]);
        return cancel.promise;
      }
      return request(path, method, body);
    },
  );
});
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
