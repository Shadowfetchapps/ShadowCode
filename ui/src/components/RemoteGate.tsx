import { useCallback, useEffect, useState, type ReactNode } from "react";
import {
  pair,
  pairingCode,
  remoteSession,
  setRemoteSession,
  Unpaired,
  type RemoteSession,
} from "../lib/remote";
import { onUnpaired } from "../lib/transport";

type GateState =
  | { kind: "checking" }
  | { kind: "ready"; session: RemoteSession }
  | { kind: "unpaired"; message?: string }
  | { kind: "offline"; message: string };

/** In a browser, the interface needs a paired device token first. A pairing
 * link (`#pair=…`) is exchanged for one at once and removed from the
 * address bar; otherwise the user pastes a link from the desktop. */
export function RemoteGate({ children }: { children: ReactNode }) {
  const [state, setState] = useState<GateState>({ kind: "checking" });
  const [link, setLink] = useState("");

  const connect = useCallback(async (given?: string) => {
    setState({ kind: "checking" });
    const code = pairingCode(given ?? window.location.hash);
    try {
      if (code) {
        // The code works once; keep it out of history and bookmarks.
        if (!given)
          history.replaceState(
            null,
            "",
            window.location.pathname + window.location.search,
          );
        await pair(code);
      }
      const session = await remoteSession();
      setRemoteSession(session);
      setState({ kind: "ready", session });
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error);
      if (error instanceof Unpaired || code)
        setState({ kind: "unpaired", message: code ? message : undefined });
      else setState({ kind: "offline", message });
    }
  }, []);

  useEffect(() => {
    void connect();
    // Opening another pairing link in this tab only changes the fragment.
    const changed = () => {
      if (pairingCode(window.location.hash)) void connect();
    };
    window.addEventListener("hashchange", changed);
    return () => window.removeEventListener("hashchange", changed);
  }, [connect]);
  useEffect(
    () =>
      onUnpaired(() =>
        setState({
          kind: "unpaired",
          message:
            "This device is no longer paired. Pair it again from the computer running ShadowCode.",
        }),
      ),
    [],
  );

  if (state.kind === "ready") return <>{children}</>;
  return (
    <main className="remote-gate" aria-labelledby="remote-gate-title">
      <img src="/icon-192.png" alt="" className="welcome-mark" />
      {state.kind === "checking" && (
        <>
          <h1 id="remote-gate-title">ShadowCode</h1>
          <p role="status">Connecting…</p>
        </>
      )}
      {state.kind === "offline" && (
        <>
          <h1 id="remote-gate-title">Can’t reach ShadowCode</h1>
          <p className="hint">
            The computer running ShadowCode did not answer. Check that it is on,
            that remote access is turned on, and that this device is on the same
            network or tailnet.
          </p>
          <p className="health-bad" role="alert">
            {state.message}
          </p>
          <button
            type="button"
            className="primary"
            onClick={() => void connect()}
          >
            Try again
          </button>
        </>
      )}
      {state.kind === "unpaired" && (
        <form
          onSubmit={(event) => {
            event.preventDefault();
            void connect(link);
          }}
        >
          <h1 id="remote-gate-title">Pair this device</h1>
          <p className="hint">
            On the computer running ShadowCode, open Settings › Remote access
            and choose <strong>Pair a device</strong>. Scan the QR code with
            this device, or paste the link here. Each link works once, for 10
            minutes.
          </p>
          {state.message && (
            <p className="health-bad" role="alert">
              {state.message}
            </p>
          )}
          <div className="field">
            <label htmlFor="remote-pair-link">Pairing link</label>
            <input
              id="remote-pair-link"
              value={link}
              autoComplete="off"
              spellCheck={false}
              onChange={(event) => setLink(event.target.value)}
              placeholder="http://…/#pair=…"
            />
          </div>
          <div className="row end">
            <button
              type="submit"
              className="primary"
              disabled={!pairingCode(link)}
            >
              Pair
            </button>
          </div>
          <p className="hint">
            Pairing stores an access key in this browser. Anyone who can use
            this browser can then control ShadowCode, so only pair devices you
            trust.
          </p>
        </form>
      )}
    </main>
  );
}
