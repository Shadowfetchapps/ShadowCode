import { invoke as tauriInvoke, isTauri } from "@tauri-apps/api/core";
import { listen as tauriListen } from "@tauri-apps/api/event";
import { createRemoteBridge } from "./remote";
import { createSharedNativeEvents } from "./nativeEvents";

/** The window talks to the in-process engine through one bridge: IPC requests,
 * native commands (pickers, external links) and engine wake-up events. */
export type Bridge = {
  request(path: string, method: string, body: unknown): Promise<unknown>;
  invoke(command: string, args?: Record<string, unknown>): Promise<unknown>;
  listen(
    event: string,
    handler: (payload: unknown) => void,
  ): Promise<() => void>;
};

declare global {
  interface Window {
    __SHADOW_TEST_TRANSPORT__?: Bridge;
  }
}

const tauriBridge: Bridge = {
  request: (path, method, body) =>
    tauriInvoke("api", { request: { method, path, body: body ?? null } }),
  invoke: (command, args) => tauriInvoke(command, args),
  listen: createSharedNativeEvents((event, handler) =>
    tauriListen(event, (message) => handler(message.payload)),
  ),
};

/** A deterministic fake backend is honoured only in builds made with
 * VITE_SHADOW_TEST_TRANSPORT=1 (the Playwright suite). Production builds
 * replace the condition with `false`, so the global is never read. */
function testBridge(): Bridge | undefined {
  if (import.meta.env.VITE_SHADOW_TEST_TRANSPORT === "1")
    return window.__SHADOW_TEST_TRANSPORT__;
  return undefined;
}

export type TransportKind = "test" | "tauri" | "remote" | "none";

/** Which bridge the interface uses: the Playwright fake (test builds only),
 * the desktop's IPC, or HTTP to a ShadowCode remote access server when the
 * page was loaded over http(s) in an ordinary browser. Unit tests get none
 * unless they install one. */
export function chooseTransport(env: {
  test: boolean;
  tauri: boolean;
  protocol: string;
  unitTest: boolean;
}): TransportKind {
  if (env.test) return "test";
  if (env.tauri) return "tauri";
  if (!env.unitTest && (env.protocol === "http:" || env.protocol === "https:"))
    return "remote";
  return "none";
}

export function transportKind(): TransportKind {
  return chooseTransport({
    test: Boolean(testBridge()),
    tauri: isTauri(),
    protocol: typeof location === "undefined" ? "" : location.protocol,
    unitTest: import.meta.env.MODE === "test",
  });
}

const unpairedListeners = new Set<() => void>();
let remoteBridge: ReturnType<typeof createRemoteBridge> | undefined;

/** Called when the remote server stops accepting this device's token. */
export function onUnpaired(listener: () => void) {
  unpairedListeners.add(listener);
  return () => void unpairedListeners.delete(listener);
}

function bridge(): Bridge {
  const fake = testBridge();
  if (fake) return fake;
  switch (transportKind()) {
    case "tauri":
      return tauriBridge;
    case "remote":
      remoteBridge ??= createRemoteBridge({
        onUnpaired: () => unpairedListeners.forEach((listener) => listener()),
      });
      return remoteBridge;
    default:
      throw new Error(
        "ShadowCode's interface runs inside the desktop app or from `shadowcode serve --remote`.",
      );
  }
}

/** An engine is reachable (desktop, remote access, or the test fake). */
export const isNative = () => transportKind() !== "none";
/** Running in a browser against a remote access server. */
export const isRemote = () => transportKind() === "remote";
/** Native file and folder pickers exist (the desktop app). */
export const canPickFiles = () => ["tauri", "test"].includes(transportKind());

/** Errors keep the backend body so callers can read structured answers such as
 * a consent request (`needs_consent`). */
export class ApiError extends Error {
  body: Record<string, unknown> | null;
  constructor(message: string, body: Record<string, unknown> | null = null) {
    super(message);
    this.name = "ApiError";
    this.body = body;
  }
  /** `String(error)` is what the window shows, so it reads as the engine's
   * sentence without a class-name prefix ("ApiError: …"). */
  override toString() {
    return this.message;
  }
}

/** Text for an error shown to people: the message alone, without a
 * JavaScript class prefix such as "Error: " or "ApiError: ". */
export function readableError(error: unknown): string {
  const text = error instanceof Error ? error.message : String(error ?? "");
  return text.replace(/^(?:Api|Type|Range|Syntax)?Error: /, "").trim();
}

function parseErrorBody(text: string): Record<string, unknown> | null {
  const start = text.indexOf("{");
  if (start < 0) return null;
  try {
    const value = JSON.parse(text.slice(start));
    return value && typeof value === "object" ? value : null;
  } catch {
    return null;
  }
}

export async function request<T>(
  path: string,
  method = "GET",
  body?: unknown,
): Promise<T> {
  try {
    return (await bridge().request(path, method, body)) as T;
  } catch (error) {
    if (error instanceof ApiError) throw error;
    const text = error instanceof Error ? error.message : String(error);
    const parsed = parseErrorBody(text);
    throw new ApiError(
      parsed && typeof parsed.error === "string" ? parsed.error : text,
      parsed,
    );
  }
}

export const invoke = <T>(command: string, args?: Record<string, unknown>) =>
  bridge().invoke(command, args) as Promise<T>;

export const listen = (event: string, handler: (payload: unknown) => void) =>
  bridge().listen(event, handler);

export const pickDirectory = () => invoke<string | null>("pick_directory");

export const pickLocalModel = (folder = false) =>
  invoke<string | null>("pick_local_model", { folder });

export const exportSession = (
  sessionId: string,
  format: "md" | "json" = "md",
) => invoke<string | null>("export_session", { sessionId, format });

export const exportDiagnostics = (
  snapshotId: string,
  expectedContent: string,
) =>
  invoke<string | null>("export_diagnostics", { snapshotId, expectedContent });

export async function openExternal(url: string) {
  const parsed = new URL(url);
  if (!["https:", "http:"].includes(parsed.protocol))
    throw new Error("Only web links can be opened externally");
  await invoke("open_external", { url: parsed.href });
}
