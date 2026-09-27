import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type SetStateAction,
} from "react";
import { isTauri } from "@tauri-apps/api/core";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api, type EditorRecoveryDraft } from "../api";
import { transportKind } from "../lib/transport";
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
  recoveryRevision?: string;
  recoveryStatus?: "saving" | "saved" | "error";
  recoveryError?: string;
  /** A competing window's current saved copy; null means it was deleted. */
  recoveryConflict?: EditorRecoveryDraft | null;
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
  filesRecoveryError: string;
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
  filesRecoveryError: "",
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
  const hydrated = useRef(new Set<string>());
  const loading = useRef(new Set<string>());
  const recovery = useRef(
    new Map<
      string,
      {
        revision: string;
        desired: string;
        chain: Promise<void>;
      }
    >(),
  );
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
  useEffect(() => {
    if (
      !["tauri", "test"].includes(transportKind()) ||
      hydrated.current.has(workspace) ||
      loading.current.has(workspace)
    )
      return;
    loading.current.add(workspace);
    void api
      .editorDrafts(workspace)
      .then(({ workspace: selectedWorkspace, drafts }) => {
        if (selectedWorkspace !== workspace)
          throw new Error(
            "Project selection changed while loading recovery drafts",
          );
        setWorkspaces((current) => {
          const selected = current[workspace] ?? emptyDrawerMemory();
          const buffers = { ...selected.filesBuffers };
          let keptNewerInput = false;
          for (const draft of drafts) {
            if (buffers[draft.path]?.draft !== buffers[draft.path]?.base) {
              // A person started typing before the recovery read finished.
              // Keep both copies; CAS will prevent overwriting the older one.
              keptNewerInput = true;
              continue;
            }
            const key = `${workspace}\0${draft.path}`;
            recovery.current.set(key, {
              revision: draft.revision,
              desired: JSON.stringify([
                draft.base,
                draft.base_hash,
                draft.draft,
              ]),
              chain: Promise.resolve(),
            });
            buffers[draft.path] = {
              path: draft.path,
              base: draft.base,
              draft: draft.draft,
              hash: draft.base_hash,
              recoveryRevision: draft.revision,
              recoveryStatus: "saved",
            };
          }
          return {
            ...current,
            [workspace]: {
              ...selected,
              filesBuffers: buffers,
              filesRecoveryError: keptNewerInput
                ? "A saved recovery copy also exists for a file you started editing. Your current text is kept in memory; reopen the project to review the saved copy before replacing it."
                : "",
            },
          };
        });
        hydrated.current.add(workspace);
      })
      .catch((reason) => {
        setWorkspaces((current) => {
          const selected = current[workspace] ?? emptyDrawerMemory();
          return {
            ...current,
            [workspace]: {
              ...selected,
              filesRecoveryError: `Could not load saved editor drafts: ${String(reason)}`,
            },
          };
        });
      })
      .finally(() => loading.current.delete(workspace));
  }, [workspace]);
  useEffect(() => {
    if (!hydrated.current.has(workspace)) return;
    const currentBuffers = memory.filesBuffers;
    const mark = (
      path: string,
      desired: string,
      status: FileBuffer["recoveryStatus"],
      reason = "",
      revision?: string,
      conflict?: EditorRecoveryDraft | null,
    ) => {
      setWorkspaces((current) => {
        const selected = current[workspace];
        const buffer = selected?.filesBuffers[path];
        if (
          !buffer ||
          JSON.stringify([buffer.base, buffer.hash, buffer.draft]) !== desired
        )
          return current;
        const next = {
          ...buffer,
          recoveryStatus: status,
          recoveryError: reason,
          recoveryRevision: revision ?? buffer.recoveryRevision,
          recoveryConflict: conflict,
        };
        return {
          ...current,
          [workspace]: {
            ...selected,
            filesBuffers: { ...selected.filesBuffers, [path]: next },
          },
        };
      });
    };
    const queueDelete = (
      path: string,
      item: { revision: string; desired: string; chain: Promise<void> },
    ) => {
      if (item.desired === "") return;
      item.desired = "";
      item.chain = item.chain
        .catch(() => undefined)
        .then(async () => {
          const removedRevision = item.revision;
          if (item.revision !== "missing")
            await api.deleteEditorDraft(workspace, path, item.revision);
          item.revision = "missing";
          if (
            item.desired === "" &&
            recovery.current.get(`${workspace}\0${path}`) === item
          ) {
            recovery.current.delete(`${workspace}\0${path}`);
            setWorkspaces((current) => {
              const selected = current[workspace];
              const buffer = selected?.filesBuffers[path];
              if (
                !buffer ||
                buffer.draft !== buffer.base ||
                (buffer.recoveryRevision &&
                  buffer.recoveryRevision !== removedRevision)
              )
                return current;
              return {
                ...current,
                [workspace]: {
                  ...selected,
                  filesBuffers: {
                    ...selected.filesBuffers,
                    [path]: {
                      ...buffer,
                      recoveryRevision: undefined,
                      recoveryStatus: undefined,
                      recoveryError: undefined,
                    },
                  },
                },
              };
            });
          }
        })
        .catch((reason) => {
          const buffer = currentBuffers[path];
          if (buffer)
            mark(
              path,
              JSON.stringify([buffer.base, buffer.hash, buffer.draft]),
              "error",
              `Could not clear saved draft: ${String(reason)}`,
            );
        });
    };
    for (const [path, buffer] of Object.entries(currentBuffers)) {
      const key = `${workspace}\0${path}`;
      let item = recovery.current.get(key);
      if (buffer.recoveryConflict !== undefined) continue;
      if (buffer.draft === buffer.base) {
        if (item) queueDelete(path, item);
        continue;
      }
      const desired = JSON.stringify([buffer.base, buffer.hash, buffer.draft]);
      if (!item) {
        item = {
          // An existing saved record has a live entry in the map (including
          // restored drafts). A newly created entry must start from missing:
          // the buffer may still carry the revision of a record just deleted.
          revision: "missing",
          desired: "",
          chain: Promise.resolve(),
        };
        recovery.current.set(key, item);
      }
      if (item.desired === desired) continue;
      item.desired = desired;
      mark(path, desired, "saving");
      const pending = item;
      pending.chain = pending.chain
        .catch(() => undefined)
        .then(async () => {
          // Keep the in-flight write, but skip snapshots superseded while
          // waiting behind it. The newest queued write uses its resulting CAS
          // revision, and a queued delete still waits for that write.
          if (pending.desired !== desired) return;
          const record = await api.saveEditorDraft(
            workspace,
            path,
            buffer.base,
            buffer.draft,
            buffer.hash,
            pending.revision,
          );
          pending.revision = record.revision;
          mark(path, desired, "saved", "", record.revision);
        })
        .catch(async (reason) => {
          const message = String(reason);
          if (
            message.includes("The recovery draft changed in another window")
          ) {
            try {
              const records = await api.editorDrafts(workspace);
              if (records.workspace !== workspace)
                throw new Error(
                  "Project selection changed while reviewing drafts",
                );
              mark(
                path,
                desired,
                "error",
                "A saved draft changed in another window. Review both versions before choosing one.",
                undefined,
                records.drafts.find((draft) => draft.path === path) ?? null,
              );
              return;
            } catch (readError) {
              mark(
                path,
                desired,
                "error",
                `Could not load the other draft: ${String(readError)}`,
              );
              return;
            }
          }
          mark(path, desired, "error", message);
        });
    }
    for (const [key, item] of recovery.current) {
      if (!key.startsWith(`${workspace}\0`)) continue;
      const path = key.slice(workspace.length + 1);
      if (!currentBuffers[path]) queueDelete(path, item);
    }
  }, [workspace, memory.filesBuffers]);
  const discardFileDraft = useCallback(
    async (path: string) => {
      if (
        ["tauri", "test"].includes(transportKind()) &&
        !hydrated.current.has(workspace)
      )
        throw new Error(
          "Wait for saved editor drafts to finish loading before discarding",
        );
      const item = recovery.current.get(`${workspace}\0${path}`);
      if (item) {
        await item.chain;
        if (item.revision !== "missing")
          await api.deleteEditorDraft(workspace, path, item.revision);
        item.revision = "missing";
        item.desired = "";
      }
      setWorkspaces((current) => {
        const selected = current[workspace];
        const buffer = selected?.filesBuffers[path];
        if (!buffer) return current;
        return {
          ...current,
          [workspace]: {
            ...selected,
            filesBuffers: {
              ...selected.filesBuffers,
              [path]: {
                ...buffer,
                draft: buffer.base,
                recoveryRevision: undefined,
                recoveryStatus: undefined,
                recoveryError: undefined,
                recoveryConflict: undefined,
              },
            },
          },
        };
      });
    },
    [workspace],
  );
  const resolveFileDraftConflict = useCallback(
    async (path: string, choice: "mine" | "saved") => {
      const buffer = workspaces[workspace]?.filesBuffers[path];
      if (!buffer || buffer.recoveryConflict === undefined)
        throw new Error("There is no saved-draft conflict to resolve");
      const key = `${workspace}\0${path}`;
      const item = recovery.current.get(key);
      if (!item) throw new Error("The draft write is no longer available");
      await item.chain;
      const records = await api.editorDrafts(workspace);
      if (records.workspace !== workspace)
        throw new Error("Project selection changed while reviewing drafts");
      const latest =
        records.drafts.find((draft) => draft.path === path) ?? null;
      if (latest?.revision !== buffer.recoveryConflict?.revision) {
        setWorkspaces((current) => {
          const selected = current[workspace];
          const draft = selected?.filesBuffers[path];
          if (!draft) return current;
          return {
            ...current,
            [workspace]: {
              ...selected,
              filesBuffers: {
                ...selected.filesBuffers,
                [path]: {
                  ...draft,
                  recoveryStatus: "error",
                  recoveryError:
                    "The other saved draft changed again. Review its latest version.",
                  recoveryConflict: latest,
                },
              },
            },
          };
        });
        return;
      }
      if (choice === "saved" && !latest)
        throw new Error(
          "The other saved draft was removed; keep your local draft instead",
        );
      item.revision = latest?.revision ?? "missing";
      item.desired =
        choice === "mine"
          ? ""
          : JSON.stringify([latest!.base, latest!.base_hash, latest!.draft]);
      setWorkspaces((current) => {
        const selected = current[workspace];
        const draft = selected?.filesBuffers[path];
        if (!draft) return current;
        return {
          ...current,
          [workspace]: {
            ...selected,
            filesBuffers: {
              ...selected.filesBuffers,
              [path]:
                choice === "mine"
                  ? {
                      ...draft,
                      recoveryStatus: undefined,
                      recoveryError: undefined,
                      recoveryConflict: undefined,
                    }
                  : {
                      ...draft,
                      base: latest!.base,
                      hash: latest!.base_hash,
                      draft: latest!.draft,
                      recoveryRevision: latest!.revision,
                      recoveryStatus: "saved",
                      recoveryError: undefined,
                      recoveryConflict: undefined,
                    },
            },
          },
        };
      });
    },
    [workspace, workspaces],
  );
  const hasUnsavedFiles = Object.values(workspaces).some((item) =>
    Object.values(item.filesBuffers).some((file) => file.draft !== file.base),
  );
  const hasUnprotectedFiles = Object.values(workspaces).some((item) =>
    Object.values(item.filesBuffers).some(
      (file) => file.draft !== file.base && file.recoveryStatus !== "saved",
    ),
  );
  useEffect(() => {
    if (!hasUnsavedFiles) return;
    const warning = hasUnprotectedFiles
      ? "Some unsaved file edits do not have a finished recovery copy. Close ShadowCode and risk losing them?"
      : "Your unsaved file edits have a local recovery copy. Close ShadowCode and restore them next time?";
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
  }, [hasUnsavedFiles, hasUnprotectedFiles]);
  return { memory, update, discardFileDraft, resolveFileDraftConflict };
}

/** `useState`-shaped access to one remembered drawer value. */
export function remembered<K extends keyof DrawerMemory>(
  memory: DrawerMemory,
  update: DrawerMemoryUpdate,
  key: K,
): [DrawerMemory[K], (value: SetStateAction<DrawerMemory[K]>) => void] {
  return [memory[key], (value) => update(key, value)];
}
