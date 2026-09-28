import { afterEach, describe, expect, it, vi } from "vitest";
import { qrPath } from "../components/QrCode";
import {
  createRemoteBridge,
  pair,
  pairingCode,
  parseEvents,
  readToken,
  saveToken,
  sessionLink,
  TOKEN_KEY,
  viewId,
} from "./remote";
import { chooseTransport } from "./transport";

const TOKEN = "scr_" + "a".repeat(43);

function json(status: number, body: unknown) {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/** A fetch double that records calls and answers from a route table. */
function fakeFetch(
  answer: (url: string, init: RequestInit) => Response | Promise<Response>,
) {
  const calls: { url: string; init: RequestInit }[] = [];
  const fetcher = vi.fn(
    async (input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      calls.push({ url, init: init ?? {} });
      return answer(url, init ?? {});
    },
  ) as unknown as typeof fetch;
  return { fetcher, calls };
}

afterEach(() => {
  saveToken(null);
  vi.useRealTimers();
});

describe("transport selection", () => {
  it("prefers the test fake, then Tauri, then HTTP in a browser", () => {
    const env = {
      test: false,
      tauri: false,
      protocol: "https:",
      unitTest: false,
    };
    expect(chooseTransport({ ...env, test: true, tauri: true })).toBe("test");
    expect(chooseTransport({ ...env, tauri: true })).toBe("tauri");
    expect(chooseTransport(env)).toBe("remote");
    expect(chooseTransport({ ...env, protocol: "http:" })).toBe("remote");
    expect(chooseTransport({ ...env, protocol: "file:" })).toBe("none");
    expect(chooseTransport({ ...env, unitTest: true })).toBe("none");
  });
});

describe("pairing links", () => {
  it("reads the code from a link, a fragment or a bare code", () => {
    const code = "Abc_def-0123456789ghijklmnopqrstuvwxyzABCDE";
    expect(pairingCode(`http://100.64.0.2:7390/#pair=${code}`)).toBe(code);
    expect(pairingCode(`#pair=${code}`)).toBe(code);
    expect(pairingCode(`  ${code} `)).toBe(code);
    expect(pairingCode("#pair=short")).toBeNull();
    expect(pairingCode("http://host/?pair=x#y")).toBeNull();
    expect(pairingCode("")).toBeNull();
  });

  it("reads notification links", () => {
    expect(sessionLink("#session=abc-123")).toBe("abc-123");
    expect(sessionLink("#pair=x&session=abc")).toBe("abc");
    expect(sessionLink("#session=../x")).toBeNull();
    expect(sessionLink("")).toBeNull();
  });

  it("exchanges a code for a token and keeps it", async () => {
    const { fetcher, calls } = fakeFetch(() =>
      json(200, { token: TOKEN, device: { id: "d", name: "Phone" } }),
    );
    await pair("code-".padEnd(40, "x"), "Phone", fetcher);
    expect(readToken()).toBe(TOKEN);
    expect(calls[0].url).toBe("/_remote/pair");
    expect(calls[0].init.method).toBe("POST");
    expect(JSON.parse(String(calls[0].init.body))).toEqual({
      code: "code-".padEnd(40, "x"),
      name: "Phone",
    });
    const refused = fakeFetch(() =>
      json(401, { error: "This pairing link is not valid any more." }),
    );
    await expect(
      pair("x".repeat(40), "Phone", refused.fetcher),
    ).rejects.toThrow("not valid any more");
  });

  it("survives blocked browser storage", () => {
    const blocked = {
      getItem: () => {
        throw new Error("blocked");
      },
      setItem: () => {
        throw new Error("blocked");
      },
      removeItem: () => {
        throw new Error("blocked");
      },
    };
    expect(readToken(blocked)).toBeNull();
    expect(() => saveToken(TOKEN, blocked)).not.toThrow();
    expect(viewId(blocked)).toMatch(/^tab-[0-9a-f]{24}$/);
  });
});

describe("remote bridge", () => {
  it("downloads only the snapshot whose bytes match the reviewed preview", async () => {
    const content = '{"schema":1,"checks":[]}\n';
    const id = "a".repeat(32);
    const { fetcher, calls } = fakeFetch(() =>
      json(200, { id, filename: "shadowcode-diagnostics.json", content }),
    );
    const bridge = createRemoteBridge({
      fetcher,
      token: () => TOKEN,
      view: "tab-0123456789",
      location: { hash: "" },
    });
    const create = vi.fn((_blob: Blob) => "blob:diagnostics");
    const revoke = vi.fn();
    Object.defineProperty(URL, "createObjectURL", {
      configurable: true,
      value: create,
    });
    Object.defineProperty(URL, "revokeObjectURL", {
      configurable: true,
      value: revoke,
    });
    const click = vi
      .spyOn(HTMLAnchorElement.prototype, "click")
      .mockImplementation(() => {});
    expect(calls).toHaveLength(0);
    await expect(
      bridge.invoke("export_diagnostics", {
        snapshotId: id,
        expectedContent: "old",
      }),
    ).rejects.toThrow("preview changed");
    expect(create).not.toHaveBeenCalled();
    await expect(
      bridge.invoke("export_diagnostics", {
        snapshotId: id,
        expectedContent: content,
      }),
    ).resolves.toBe("shadowcode-diagnostics.json");
    expect(calls).toHaveLength(2);
    expect(calls[1].url).toBe(`/api/diagnostic-exports/${id}`);
    expect(click).toHaveBeenCalledTimes(1);
    expect(await (create.mock.calls[0][0] as Blob).text()).toBe(content);
    await vi.waitFor(
      () => expect(revoke).toHaveBeenCalledWith("blob:diagnostics"),
      { timeout: 2500 },
    );
    click.mockRestore();
    bridge.close();
  });

  it("sends the token, the tab's view and JSON bodies", async () => {
    const { fetcher, calls } = fakeFetch(() => json(200, { ok: true }));
    const bridge = createRemoteBridge({
      fetcher,
      token: () => TOKEN,
      view: "tab-0123456789",
      location: { hash: "" },
    });
    await expect(bridge.request("/api/feed", "GET", null)).resolves.toEqual({
      ok: true,
    });
    await bridge.request("/api/jobs/1/steer", "POST", { text: "hi" });
    const [get, post] = calls;
    const headers = get.init.headers as Record<string, string>;
    expect(headers.Authorization).toBe(`Bearer ${TOKEN}`);
    expect(headers["X-Shadow-View"]).toBe("tab-0123456789");
    expect(get.init.body).toBeUndefined();
    expect(get.init.credentials).toBe("omit");
    expect(post.init.method).toBe("POST");
    expect((post.init.headers as Record<string, string>)["Content-Type"]).toBe(
      "application/json",
    );
    expect(JSON.parse(String(post.init.body))).toEqual({ text: "hi" });
  });

  it("keeps structured errors and reports an unpaired device", async () => {
    localStorage.setItem(TOKEN_KEY, TOKEN);
    const onUnpaired = vi.fn();
    const { fetcher } = fakeFetch((url) =>
      url.includes("consent")
        ? json(400, { error: "Needs consent", needs_consent: true })
        : json(401, { error: "This device is not paired." }),
    );
    const bridge = createRemoteBridge({
      fetcher,
      onUnpaired,
      view: "tab-0123456789",
      location: { hash: "" },
    });
    const consent = await bridge
      .request("/api/consent", "POST", {})
      .catch((e: Error) => e.message);
    expect(JSON.parse(String(consent))).toMatchObject({ needs_consent: true });
    await expect(bridge.request("/api/feed", "GET", null)).rejects.toThrow(
      "not paired",
    );
    expect(onUnpaired).toHaveBeenCalledTimes(1);
    expect(readToken()).toBeNull();
  });

  it("delivers stream events to listeners and stops when they leave", async () => {
    const encoder = new TextEncoder();
    let abortSignal: AbortSignal | undefined;
    const { fetcher, calls } = fakeFetch((url, init) => {
      if (url !== "/_remote/stream") return json(404, {});
      abortSignal = init.signal ?? undefined;
      const body = new ReadableStream<Uint8Array>({
        start(controller) {
          controller.enqueue(
            encoder.encode(
              'retry: 3000\n\nevent: shadowcode:events\ndata: {}\n\n: keepalive\n\nevent: shadowcode:events\ndata: {"session_id":"s1",',
            ),
          );
          controller.enqueue(
            encoder.encode(
              '"type":"approval.requested"}\n\nevent: shadowcode:terminal\ndata: {"type":"terminal.output","terminal_id":"t"}\n\n',
            ),
          );
        },
      });
      return new Response(body, {
        status: 200,
        headers: { "Content-Type": "text/event-stream" },
      });
    });
    const bridge = createRemoteBridge({
      fetcher,
      token: () => TOKEN,
      view: "tab-0123456789",
      location: { hash: "" },
    });
    const events: unknown[] = [];
    const terminals: unknown[] = [];
    const stop = await bridge.listen("shadowcode:events", (p) =>
      events.push(p),
    );
    const stopTerminals = await bridge.listen("shadowcode:terminal", (p) =>
      terminals.push(p),
    );
    await vi.waitFor(() => expect(events).toHaveLength(2));
    expect(events[1]).toEqual({
      session_id: "s1",
      type: "approval.requested",
    });
    await vi.waitFor(() => expect(terminals).toHaveLength(1));
    // One stream serves every listener.
    expect(calls.filter((c) => c.url === "/_remote/stream")).toHaveLength(1);
    expect(
      (calls[0].init.headers as Record<string, string>).Authorization,
    ).toBe(`Bearer ${TOKEN}`);
    stop();
    expect(abortSignal?.aborted).toBe(false);
    stopTerminals();
    expect(abortSignal?.aborted).toBe(true);
    bridge.close();
  });

  it("opens the conversation named in a notification link", async () => {
    const { fetcher } = fakeFetch(() => json(200, {}));
    const bridge = createRemoteBridge({
      fetcher,
      token: () => TOKEN,
      view: "tab-0123456789",
      location: { hash: "#session=abc123" },
    });
    const opened: unknown[] = [];
    await bridge.listen("shadowcode:open-session", (p) => opened.push(p));
    await vi.waitFor(() => expect(opened).toEqual([{ session_id: "abc123" }]));
    bridge.close();
  });

  it("answers desktop-only commands without a desktop", async () => {
    const bridge = createRemoteBridge({
      fetcher: fakeFetch(() => json(200, {})).fetcher,
      token: () => TOKEN,
      view: "tab-0123456789",
      location: { hash: "" },
    });
    await expect(bridge.invoke("pick_directory")).resolves.toBeNull();
    await expect(
      bridge.invoke("set_visible_session", { sessionId: "s" }),
    ).resolves.toBeNull();
    await expect(bridge.invoke("unknown_command")).rejects.toThrow(
      "only available in the desktop app",
    );
    const open = vi.spyOn(window, "open").mockReturnValue(null);
    await bridge.invoke("open_external", { url: "https://example.com/x" });
    expect(open).toHaveBeenCalledWith(
      "https://example.com/x",
      "_blank",
      "noopener,noreferrer",
    );
    await expect(
      bridge.invoke("open_external", { url: "javascript:alert(1)" }),
    ).rejects.toThrow();
    open.mockRestore();
  });
});

describe("event parsing and QR drawing", () => {
  it("keeps partial events for the next chunk", () => {
    const first = parseEvents('event: a\ndata: {"x":1}\n\nevent: b\ndata: {');
    expect(first.events).toEqual([{ event: "a", data: { x: 1 } }]);
    const second = parseEvents(first.rest + '"y":2}\r\n\r\n');
    expect(second.events).toEqual([{ event: "b", data: { y: 2 } }]);
    expect(parseEvents(": comment\n\nretry: 5\n\n").events).toEqual([]);
  });

  it("draws runs of dark modules", () => {
    expect(qrPath(["110", "001"], 0)).toBe("M0 0h2v1h-2zM2 1h1v1h-1z");
    expect(qrPath(["1"])).toBe("M4 4h1v1h-1z");
  });
});
