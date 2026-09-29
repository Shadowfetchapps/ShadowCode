import { useCallback, useEffect, useRef, useState } from "react";
import { api, type RolesChange, type RolesView } from "../api";
import type { ToastKind } from "./useToasts";

/** The project's roles, resolved for the open conversation and the model
 * chosen in the composer (GET /api/roles). `version` reloads them when
 * settings that affect them change (network mode, accounts). */
export function useRoles({
  workspace,
  sessionId,
  model,
  version,
  toast,
}: {
  workspace: string;
  sessionId: string;
  model: string;
  version?: unknown;
  toast: (text: string, kind?: ToastKind) => void;
}) {
  const [view, setView] = useState<RolesView | null>(null);
  const [saving, setSaving] = useState(false);
  // Only the newest request may set the view.
  const ticket = useRef(0);
  const reload = useCallback(async () => {
    const mine = ++ticket.current;
    if (!workspace) {
      setView(null);
      return;
    }
    try {
      const next = await api.roles(workspace, sessionId, model);
      if (mine === ticket.current) setView(next);
    } catch {
      if (mine === ticket.current) setView(null);
    }
  }, [workspace, sessionId, model]);
  useEffect(() => {
    void reload();
  }, [reload, version]);
  const save = useCallback(
    async (change: RolesChange) => {
      const mine = ++ticket.current;
      setSaving(true);
      try {
        const next = await api.saveRoles({
          ...(workspace ? { workspace } : {}),
          ...(sessionId ? { session_id: sessionId } : {}),
          ...(model ? { model } : {}),
          ...change,
        });
        if (mine === ticket.current) setView(next);
        return next;
      } catch (e) {
        toast(String(e), "err");
        return null;
      } finally {
        setSaving(false);
      }
    },
    [workspace, sessionId, model, toast],
  );
  return {
    view,
    saving,
    reload,
    save,
    /** Code and Plan tasks run as Plan → Implement → Review. */
    pipeline: Boolean(view?.setup.pipeline),
  };
}

export type Roles = ReturnType<typeof useRoles>;
