/** The interface in a browser (a phone or another computer) talks to the
 * engine over HTTP: `/api/*` with the device's access token in the
 * Authorization header, and a Server-Sent Events stream for the same
 * wake-ups the desktop shell forwards. The token comes from a one-time
 * pairing link (`#pair=…`) and is kept in this browser's storage. */
import type { Bridge } from "./transport";

export const TOKEN_KEY = "shadow:remote:token";
const VIEW_KEY = "shadow:remote:view";
export const STREAM_PATH = "/_remote/stream";
export const PAIR_PATH = "/_remote/pair";
export const SESSION_PATH = "/_remote/session";

export type RemoteSession = {
  device: { id: string; name: string };
  allow_terminals: boolean;
  version: string;
};

/** Raised when the server no longer accepts this device's token. */
export class Unpaired extends Error {
  constructor(message = "This device is not paired.") {
    super(message);
    this.name = "Unpaired";
  }
}

type Store = Pick<Storage, "getItem" | "setItem" | "removeItem">;

function safeStore(get: () => Store | undefined): Store | undefined {
  try {
    return get();
  } catch {
    return undefined;
  }
}

export function readToken(store = safeStore(() => localStorage)) {
  try {
    return store?.getItem(TOKEN_KEY) || null;
  } catch {
    return null;
  }
}

export function saveToken(
  token: string | null,
  store = safeStore(() => localStorage),
) {
  try {
    if (token) store?.setItem(TOKEN_KEY, token);
    else store?.removeItem(TOKEN_KEY);
  } catch {
    /* Private mode: the token lasts for this page only. */
  }
}

/** The pairing code in a link's fragment (`#pair=CODE`), or null. Accepts a
 * whole pasted link, a fragment, or a bare code. */
export function pairingCode(text: string): string | null {
  const trimmed = text.trim();
  const match = /(?:^|[#&])pair=([A-Za-z0-9_-]{16,128})(?:&|$)/.exec(
    trimmed.includes("#") ? trimmed.slice(trimmed.indexOf("#")) : trimmed,
  );
  if (match) return match[1];
  return /^[A-Za-z0-9_-]{32,128}$/.test(trimmed) ? trimmed : null;
}

/** The conversation a notification link opens (`#session=ID`), or null. */
export function sessionLink(hash: string): string | null {
  const match = /(?:^|[#&])session=([A-Za-z0-9-]{1,64})(?:&|$)/.exec(hash);
  return match ? match[1] : null;
}

/** One navigation state per browser tab: the server keeps each tab's open
 * project separately (and never changes the desktop's). */
export function viewId(store = safeStore(() => sessionStorage)): string {
  try {
    const known = store?.getItem(VIEW_KEY);
    if (known && /^[A-Za-z0-9_-]{8,64}$/.test(known)) return known;
  } catch {
    /* fall through */
  }
  const bytes = new Uint8Array(12);
  crypto.getRandomValues(bytes);
  const id = `tab-${Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join("")}`;
  try {
    store?.setItem(VIEW_KEY, id);
  } catch {
    /* one id per page load */
  }
  return id;
}

/** A readable name for this device in the desktop's device list. */
export function deviceName(agent = navigator.userAgent): string {
  const platform = /iPhone|iPad/.test(agent)
    ? "iPhone or iPad"
    : /Android/.test(agent)
      ? "Android"
      : /Mac OS X/.test(agent)
        ? "Mac"
        : /Windows/.test(agent)
          ? "Windows"
          : /Linux/.test(agent)
            ? "Linux"
            : "Browser";
  const browser = /Firefox\//.test(agent)
    ? "Firefox"
    : /Edg\//.test(agent)
      ? "Edge"
      : /Chrome\//.test(agent)
        ? "Chrome"
        : /Safari\//.test(agent)
          ? "Safari"
          : "";
  return browser ? `${browser} on ${platform}` : platform;
}

async function errorText(response: Response): Promise<{
  message: string;
  body: Record<string, unknown> | null;
}> {
  const text = await response.text().catch(() => "");
  try {
    const body = JSON.parse(text);
    if (body && typeof body === "object")
      return {
        message:
          typeof body.error === "string"
            ? body.error
            : `Request failed (${response.status})`,
        body,
      };
  } catch {
    /* not JSON */
  }
  return {
    message: text || `Request failed (${response.status})`,
    body: null,
  };
}

/** Exchange a pairing code for a device token (and keep it). */
export async function pair(
  code: string,
  name = deviceName(),
  fetcher: typeof fetch = fetch,
): Promise<string> {
  const response = await fetcher(PAIR_PATH, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ code, name }),
    credentials: "omit",
  });
  if (!response.ok) throw new Error((await errorText(response)).message);
  const body = (await response.json()) as { token?: string };
  if (!body.token) throw new Error("The server did not return a token");
  saveToken(body.token);
  return body.token;
}

/** Split a Server-Sent Events buffer into complete events; the rest waits
 * for more bytes. Comments (keep-alives) and `retry:` lines are ignored. */
export function parseEvents(buffer: string): {
  events: { event: string; data: unknown }[];
  rest: string;
} {
  const events: { event: string; data: unknown }[] = [];
  const normalized = buffer.replace(/\r\n/g, "\n");
  const blocks = normalized.split("\n\n");
  const rest = blocks.pop() ?? "";
  for (const block of blocks) {
    let event = "message";
    const data: string[] = [];
    for (const line of block.split("\n")) {
      if (line.startsWith("event:")) event = line.slice(6).trim();
      else if (line.startsWith("data:")) data.push(line.slice(5).trimStart());
    }
    if (!data.length) continue;
    try {
      events.push({ event, data: JSON.parse(data.join("\n")) });
    } catch {
      /* malformed event: skip it */
    }
  }
  return { events, rest };
}

export type RemoteOptions = {
  fetcher?: typeof fetch;
  token?: () => string | null;
  view?: string;
  /** Called when the server rejects the token (the device was unpaired). */
  onUnpaired?: () => void;
  /** Delay before reconnecting the event stream, by attempt. */
  retryMs?: (attempt: number) => number;
  location?: Pick<Location, "hash">;
};

type Handler = (payload: unknown) => void;

/** The HTTP bridge. Requests carry the token; the event stream runs while
 * something listens and reconnects after drops. */
export function createRemoteBridge(options: RemoteOptions = {}): Bridge & {
  close(): void;
} {
  const fetcher = options.fetcher ?? ((...args) => fetch(...args));
  const token = options.token ?? (() => readToken());
  const view = options.view ?? viewId();
  const retry =
    options.retryMs ?? ((attempt) => Math.min(1000 * 2 ** attempt, 15000));
  const place = options.location ?? window.location;
  const handlers = new Map<string, Set<Handler>>();
  let stream: AbortController | null = null;
  let stopped = false;

  const unpaired = () => {
    saveToken(null);
    options.onUnpaired?.();
  };

  const headers = (json: boolean): Record<string, string> => {
    const value = token();
    return {
      ...(value ? { Authorization: `Bearer ${value}` } : {}),
      "X-Shadow-View": view,
      ...(json ? { "Content-Type": "application/json" } : {}),
    };
  };

  const emit = (event: string, payload: unknown) =>
    handlers.get(event)?.forEach((handler) => handler(payload));

  const listening = () =>
    [...handlers.entries()].some(
      ([name, set]) => set.size && name !== "shadowcode:open-session",
    );

  async function run(signal: AbortSignal) {
    let attempt = 0;
    while (!signal.aborted && !stopped) {
      try {
        const response = await fetcher(STREAM_PATH, {
          headers: headers(false),
          signal,
          credentials: "omit",
          cache: "no-store",
        });
        if (response.status === 401) {
          unpaired();
          return;
        }
        if (!response.ok || !response.body)
          throw new Error(`Event stream failed (${response.status})`);
        attempt = 0;
        const reader = response.body.getReader();
        const decoder = new TextDecoder();
        let buffer = "";
        for (;;) {
          const { value, done } = await reader.read();
          if (done) break;
          buffer += decoder.decode(value, { stream: true });
          const parsed = parseEvents(buffer);
          buffer = parsed.rest;
          parsed.events.forEach(({ event, data }) => emit(event, data));
        }
      } catch {
        if (signal.aborted) return;
      }
      if (signal.aborted || stopped) return;
      // Something may have happened while disconnected: wake everyone.
      emit("shadowcode:events", {});
      await new Promise((resolve) => setTimeout(resolve, retry(attempt++)));
    }
  }

  const ensureStream = () => {
    if (stream || stopped || !listening()) return;
    stream = new AbortController();
    const current = stream;
    void run(current.signal).finally(() => {
      if (stream === current) stream = null;
    });
  };
  const maybeStop = () => {
    if (stream && !listening()) {
      stream.abort();
      stream = null;
    }
  };

  // Notification links (`#session=ID`) open their conversation.
  let pendingSession = sessionLink(place.hash);
  const deliverSession = () => {
    const id = pendingSession ?? sessionLink(place.hash);
    const listeners = handlers.get("shadowcode:open-session");
    if (!id || !listeners?.size) return;
    pendingSession = null;
    emit("shadowcode:open-session", { session_id: id });
  };
  if (typeof window !== "undefined")
    window.addEventListener("hashchange", () => {
      pendingSession = sessionLink(place.hash);
      deliverSession();
    });

  async function request(path: string, method: string, body: unknown) {
    const send = method !== "GET" && method !== "HEAD";
    const response = await fetcher(path, {
      method,
      headers: headers(send),
      body: send ? JSON.stringify(body ?? {}) : undefined,
      credentials: "omit",
      cache: "no-store",
    });
    if (response.status === 401) {
      unpaired();
      throw new Unpaired((await errorText(response)).message);
    }
    if (!response.ok) {
      const { message, body: detail } = await errorText(response);
      // Same shape the desktop bridge produces: the message carries the
      // JSON body so callers can read structured answers.
      throw new Error(detail ? JSON.stringify(detail) : message);
    }
    const text = await response.text();
    return text ? JSON.parse(text) : null;
  }

  async function exportSession(args?: Record<string, unknown>) {
    const id = String(args?.sessionId ?? "");
    const format = args?.format === "json" ? "json" : "md";
    if (!/^[A-Za-z0-9-]+$/.test(id)) throw new Error("Invalid export request");
    const result = (await request(
      `/api/sessions/${id}/export?format=${format}`,
      "GET",
      null,
    )) as { filename?: string; content?: string };
    const blob = new Blob([result.content ?? ""], {
      type: format === "json" ? "application/json" : "text/markdown",
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = result.filename || `conversation.${format}`;
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return link.download;
  }

  async function exportDiagnostics(args?: Record<string, unknown>) {
    const id = String(args?.snapshotId ?? "");
    if (!/^[a-fA-F0-9]{32}$/.test(id))
      throw new Error("Invalid diagnostic snapshot ID");
    const result = (await request(`/api/diagnostic-exports/${id}`, "GET", null)) as {
      content?: string;
    };
    const content = result.content;
    if (typeof content !== "string" || content !== args?.expectedContent)
      throw new Error("Diagnostic preview changed; run Doctor again before saving");
    const blob = new Blob([content], { type: "application/json" });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = "shadowcode-diagnostics.json";
    document.body.appendChild(link);
    link.click();
    link.remove();
    setTimeout(() => URL.revokeObjectURL(url), 1000);
    return link.download;
  }

  return {
    request,
    async invoke(command, args) {
      switch (command) {
        case "open_external": {
          const url = new URL(String(args?.url ?? ""));
          if (!["http:", "https:"].includes(url.protocol))
            throw new Error("Only web links can be opened");
          window.open(url.href, "_blank", "noopener,noreferrer");
          return null;
        }
        case "export_session":
          return exportSession(args);
        case "export_diagnostics":
          return exportDiagnostics(args);
        // Native file pickers, the desktop's own focus tracking and quitting
        // the desktop app do not apply to a browser.
        case "pick_directory":
        case "pick_local_model":
        case "set_visible_session":
        case "desktop_quit":
          return null;
        default:
          throw new Error(`${command} is only available in the desktop app`);
      }
    },
    async listen(event, handler) {
      const set = handlers.get(event) ?? new Set();
      set.add(handler);
      handlers.set(event, set);
      if (event === "shadowcode:open-session") setTimeout(deliverSession, 0);
      else ensureStream();
      return () => {
        set.delete(handler);
        maybeStop();
      };
    },
    close() {
      stopped = true;
      stream?.abort();
      stream = null;
    },
  };
}

let current: RemoteSession | null = null;
/** What the paired device may use (set once the gate connects). */
export const getRemoteSession = () => current;
export const setRemoteSession = (session: RemoteSession | null) => {
  current = session;
};

/** Is this token still accepted, and what may this device use? */
export async function remoteSession(
  fetcher: typeof fetch = fetch,
  token = readToken(),
): Promise<RemoteSession> {
  if (!token) throw new Unpaired();
  const response = await fetcher(SESSION_PATH, {
    headers: { Authorization: `Bearer ${token}` },
    credentials: "omit",
    cache: "no-store",
  });
  if (response.status === 401) {
    saveToken(null);
    throw new Unpaired((await errorText(response)).message);
  }
  if (!response.ok) throw new Error((await errorText(response)).message);
  return (await response.json()) as RemoteSession;
}
