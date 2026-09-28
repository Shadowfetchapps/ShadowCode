import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type DownloadCatalog,
  type DownloadModel,
  type DownloadState,
} from "../api";
import { RUNNING } from "../lib/downloads";

/** How often progress is read while a download or resume check runs. */
export const DOWNLOAD_POLL_MS = 1000;

const message = (e: unknown) => (e instanceof Error ? e.message : String(e));

/** The free-model catalog and its downloads. Reads once, then only while
 * something downloads. `onInstalled` fires when a download this window saw
 * running finishes. */
export function useModelDownloads({
  onError,
  onInstalled,
  enabled = true,
}: {
  onError: (text: string) => void;
  onInstalled?: (model: DownloadModel) => void;
  enabled?: boolean;
}) {
  const [catalog, setCatalog] = useState<DownloadCatalog | null>(null);
  const [loadError, setLoadError] = useState("");
  const [pending, setPending] = useState("");
  const seen = useRef<Record<string, DownloadState>>({});
  const mounted = useRef(true);
  const callbacks = useRef({ onError, onInstalled });
  callbacks.current = { onError, onInstalled };
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const apply = useCallback((next: DownloadCatalog) => {
    if (!mounted.current) return;
    const before = seen.current;
    seen.current = Object.fromEntries(next.models.map((m) => [m.id, m.state]));
    setCatalog(next);
    for (const model of next.models)
      if (model.state === "installed" && RUNNING.has(before[model.id]))
        callbacks.current.onInstalled?.(model);
  }, []);

  const load = useCallback(async () => {
    try {
      apply(await api.modelDownloads());
      if (mounted.current) setLoadError("");
    } catch (e) {
      if (mounted.current) setLoadError(message(e));
    }
  }, [apply]);

  useEffect(() => {
    if (enabled) void load();
  }, [enabled, load]);

  // One read per second while a download runs; each read schedules the next.
  const running = Boolean(catalog?.models.some((m) => RUNNING.has(m.state)));
  useEffect(() => {
    if (!running || !enabled) return;
    const timer = setTimeout(() => void load(), DOWNLOAD_POLL_MS);
    return () => clearTimeout(timer);
  }, [catalog, running, enabled, load]);

  const act = useCallback(
    async (key: string, action: () => Promise<DownloadCatalog>) => {
      setPending(key);
      try {
        apply(await action());
        return true;
      } catch (e) {
        callbacks.current.onError(message(e));
        await load();
        return false;
      } finally {
        if (mounted.current) setPending("");
      }
    },
    [apply, load],
  );

  return {
    catalog,
    loadError,
    pending,
    reload: load,
    start: (id: string) => act(`start:${id}`, () => api.startModelDownload(id)),
    pause: (id: string) => act(`pause:${id}`, () => api.pauseModelDownload(id)),
    cancel: (id: string) =>
      act(`cancel:${id}`, () => api.cancelModelDownload(id)),
    remove: (id: string) =>
      act(`delete:${id}`, () => api.deleteModelDownload(id)),
  };
}

export type ModelDownloads = ReturnType<typeof useModelDownloads>;
