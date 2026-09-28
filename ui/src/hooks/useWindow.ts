import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { readStore, writeStore } from "../lib/storage";
import { isNative, listen, openExternal } from "../lib/transport";
import type { ToastKind } from "./useToasts";

export type Shutdown = { status: string; message?: string };

/** Desktop shell integration: the close sequence's progress, and web links
 * opened in the system browser instead of the app window. */
export function useDesktopEvents(
  toast: (text: string, kind?: ToastKind) => void,
) {
  const [shutdown, setShutdown] = useState<Shutdown | null>(null);
  useEffect(() => {
    let stopped = false;
    let unsubscribe: (() => void) | undefined;
    if (isNative())
      void listen("shadowcode:shutdown", (payload) =>
        setShutdown(payload as Shutdown),
      )
        .then((stop) => {
          if (stopped) stop();
          else unsubscribe = stop;
        })
        .catch((error) => {
          if (!stopped) toast(String(error), "err");
        });
    const external = (event: MouseEvent) => {
      const anchor = (
        event.target as Element | null
      )?.closest<HTMLAnchorElement>("a[href]");
      if (!anchor || anchor.getAttribute("href")?.startsWith("#")) return;
      event.preventDefault();
      void openExternal(anchor.href).catch((error) =>
        toast(String(error), "err"),
      );
    };
    document.addEventListener("click", external, true);
    return () => {
      stopped = true;
      unsubscribe?.();
      document.removeEventListener("click", external, true);
    };
  }, [toast]);
  return shutdown;
}

/** The sidebar is remembered, and closes when the window gets narrow. Only
 * the user's own choice is remembered: a sidebar the narrow window closed
 * comes back when the window is wide again (and after a restart). */
export function useSidebar() {
  const chosen = useRef(readStore("shadow:sidebar") !== "closed");
  const [sidebar, setOpen] = useState(
    () => chosen.current && window.innerWidth > 760,
  );
  const setSidebar = useCallback((next: SetStateAction<boolean>) => {
    setOpen((current) => {
      const open = typeof next === "function" ? next(current) : next;
      chosen.current = open;
      writeStore("shadow:sidebar", open ? "open" : "closed");
      return open;
    });
  }, []);
  useEffect(() => {
    const compact = window.matchMedia("(max-width: 760px)");
    const resize = (event: MediaQueryListEvent) =>
      setOpen(event.matches ? false : chosen.current);
    compact.addEventListener("change", resize);
    return () => compact.removeEventListener("change", resize);
  }, []);
  return [sidebar, setSidebar] as const;
}
