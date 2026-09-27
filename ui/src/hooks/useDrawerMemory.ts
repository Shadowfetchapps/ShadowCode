import { useCallback, useEffect, useState, type SetStateAction } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import type { PreviewDevice } from "../components/PreviewPanel";
import type { ToolsView } from "../components/ToolsTab";
import type { IssueLink } from "../lib/issues";

/** A pull request being written in the Git tab. */
export type PrDraft = {
  title: string;
  body: string;
  /** Empty: the repository's default branch. */
  base: string;
  draft: boolean;
};

export type FileBuffer = {
  path: string;
  /** The exact bytes and revision last accepted from the workspace. */
  base: string;
  hash: string;
  /** User input remains here until an explicit save or discard. */
  draft: string;
  /** Another writer changed the file while a draft was open. A null content
   * means the replacement is no longer readable as UTF-8 text; a null disk
   * means the path was deleted. */
  disk?: { content: string | null; hash: string } | null;
};

/** Drawer work that must survive switching tabs (and closing the drawer):
 * the terminal tab in front (the shells themselves live in the engine), the
 * folder and open file in Files, the commit message and pull request drafts,
 * the last Tools view, and the page and device width in Preview. A different
 * project starts fresh. */
export type DrawerMemory = {
  terminalActive: string;
  filesDir: string;
  filesActive: string | null;
  filesBuffers: Record<string, FileBuffer>;
  commitMessage: string;
  prDraft: PrDraft;
  toolsView: ToolsView;
  previewUrl: string;
  previewDevice: PreviewDevice;
  /** The issue the latest task was started from (Tools › Issues). */
  issueTask: IssueLink | null;
};
export type DrawerMemoryUpdate = <K extends keyof DrawerMemory>(
  key: K,
  value: SetStateAction<DrawerMemory[K]>,
) => void;

export const emptyDrawerMemory = (): DrawerMemory => ({
  terminalActive: "",
  filesDir: ".",
  filesActive: null,
  filesBuffers: {},
  commitMessage: "",
  prDraft: { title: "", body: "", base: "", draft: false },
  toolsView: "goals",
  previewUrl: "",
  previewDevice: "desktop",
  issueTask: null,
});

export function useDrawerMemory(workspace: string) {
  const [workspaces, setWorkspaces] = useState<Record<string, DrawerMemory>>(
    () => ({ [workspace]: emptyDrawerMemory() }),
  );
  const memory = workspaces[workspace] ?? emptyDrawerMemory();
  if (!workspaces[workspace])
    setWorkspaces((current) => ({
      ...current,
      [workspace]: current[workspace] ?? emptyDrawerMemory(),
    }));
  const update: DrawerMemoryUpdate = useCallback(
    (key, value) => {
      setWorkspaces((current) => {
        const selected = current[workspace] ?? emptyDrawerMemory();
        const previous = selected[key];
        const next =
          typeof value === "function"
            ? (value as (prev: typeof previous) => typeof previous)(previous)
            : value;
        return next === previous
          ? current
          : { ...current, [workspace]: { ...selected, [key]: next } };
      });
    },
    [workspace],
  );
  const hasUnsavedFiles = Object.values(workspaces).some((item) =>
    Object.values(item.filesBuffers).some((file) => file.draft !== file.base),
  );
  useEffect(() => {
    if (!hasUnsavedFiles) return;
    const warning =
      "You have unsaved file edits. Close ShadowCode and lose those drafts?";
    const beforeUnload = (event: BeforeUnloadEvent) => {
      event.preventDefault();
      event.returnValue = warning;
    };
    if (isTauri()) {
      let disposed = false;
      let fallback = false;
      let unlisten: (() => void) | undefined;
      void getCurrentWindow()
        .onCloseRequested((event) => {
          if (!window.confirm(warning)) event.preventDefault();
        })
        .then((stop) => {
          if (disposed) stop();
          else unlisten = stop;
        })
        .catch(() => {
          if (!disposed) {
            fallback = true;
            window.addEventListener("beforeunload", beforeUnload);
          }
        });
      return () => {
        disposed = true;
        unlisten?.();
        if (fallback) window.removeEventListener("beforeunload", beforeUnload);
      };
    }
    window.addEventListener("beforeunload", beforeUnload);
    return () => window.removeEventListener("beforeunload", beforeUnload);
  }, [hasUnsavedFiles]);
  return { memory, update };
}

/** `useState`-shaped access to one remembered drawer value. */
export function remembered<K extends keyof DrawerMemory>(
  memory: DrawerMemory,
  update: DrawerMemoryUpdate,
  key: K,
): [DrawerMemory[K], (value: SetStateAction<DrawerMemory[K]>) => void] {
  return [memory[key], (value) => update(key, value)];
}
