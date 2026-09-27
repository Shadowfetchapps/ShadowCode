import { useCallback, useEffect, useRef, useState } from "react";
import { api, type AllowanceResponse } from "../api";
import type { PickerTarget } from "../lib/picker";
import type { ToastKind } from "./useToasts";

const PICKER_POLL_INTERVAL_MS = 1000;
const PICKER_POLL_LIFETIME_MS = 60000;

/** Publish local and already-cached rows while the full vendor refresh runs.
 * A full answer wins over its snapshots; a newer reload wins over both. */
export function usePickerTargets(
  toast: (text: string, kind?: ToastKind) => void,
) {
  const [targets, setTargets] = useState<PickerTarget[]>([]);
  const [loaded, setLoaded] = useState(false);
  const fetched = useRef(0);
  const seq = useRef(0);
  const mounted = useRef(false);
  const stopPolling = useRef<(() => void) | null>(null);
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      seq.current += 1;
      stopPolling.current?.();
      stopPolling.current = null;
    };
  }, []);
  const reload = useCallback(
    async (refresh = false) => {
      if (!mounted.current) return;
      stopPolling.current?.();
      fetched.current = Date.now();
      const ticket = ++seq.current;
      const current = () => mounted.current && ticket === seq.current;
      let polling = true;
      let next: ReturnType<typeof setTimeout> | undefined;
      let lifetime: ReturnType<typeof setTimeout> | undefined;
      const stop = () => {
        polling = false;
        clearTimeout(next);
        clearTimeout(lifetime);
      };
      stopPolling.current = stop;
      lifetime = setTimeout(stop, PICKER_POLL_LIFETIME_MS);
      const snapshot = async () => {
        try {
          const result = await api.pickerCached();
          if (!polling || !current()) return;
          setTargets(Array.isArray(result.targets) ? result.targets : []);
          setLoaded(true);
        } catch {
          // The full request owns error reporting. A snapshot can recover.
        } finally {
          // Never overlap snapshots, including when the bridge is slow.
          if (polling && current())
            next = setTimeout(() => void snapshot(), PICKER_POLL_INTERVAL_MS);
        }
      };
      void snapshot();
      try {
        const result = await api.picker(refresh);
        stop();
        if (!current()) return;
        setTargets(Array.isArray(result.targets) ? result.targets : []);
      } catch (e) {
        stop();
        if (current()) toast(`Could not load models: ${String(e)}`, "err");
      } finally {
        stop();
        if (stopPolling.current === stop) stopPolling.current = null;
        if (current()) setLoaded(true);
      }
    },
    [toast],
  );
  return { targets, loaded, reload, fetched };
}

/** Allowance rows for the status bar and its panel. `refresh` re-checks
 * vendor accounts (slow); otherwise the engine answers from its cache. */
export function useAllowance() {
  const [data, setData] = useState<AllowanceResponse | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");
  const seq = useRef(0);
  const reload = useCallback(async (refresh = false) => {
    const ticket = ++seq.current;
    setLoading(true);
    try {
      const result = await api.allowance(refresh);
      if (ticket !== seq.current) return;
      setData(result);
      setError("");
    } catch (e) {
      if (ticket === seq.current) setError(String(e));
    } finally {
      if (ticket === seq.current) setLoading(false);
    }
  }, []);
  return { data, loading, error, reload };
}

/** Readiness and usage change outside the app (sign-in in a browser, plan
 * resets): refresh rows when the window regains focus, at most every 15 s. */
export function useRefreshOnFocus(
  fetched: { current: number },
  reload: () => void,
) {
  const latest = useRef(reload);
  latest.current = reload;
  useEffect(() => {
    const onFocus = () => {
      if (Date.now() - fetched.current > 15000) latest.current();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [fetched]);
}
