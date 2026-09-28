import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { api, type FileEntry } from "../api";
import {
  remembered,
  type DrawerMemory,
  type DrawerMemoryUpdate,
  type FileBuffer,
} from "../hooks/useDrawerMemory";
import { Empty } from "./cards";
import { notifyWorkspaceFilesChanged } from "../lib/workspaceChanges";
import { CodeEditor, hasMixedLineEndings } from "./CodeEditor";
import { plainEditorEdit, plainEditorRange } from "../lib/plainEditorEdit";

type Toast = (text: string, kind?: "ok" | "err" | "info") => void;

/** File drafts are owned by the app, not the drawer tab. Every save is checked
 * against the revision originally opened or explicitly reviewed by the user. */
export function FileEditor({
  workspace,
  memory,
  onMemory,
  onDiscardFileDraft,
  onResolveFileDraftConflict,
  onShowDiff,
  toast,
}: {
  workspace: string;
  memory: DrawerMemory;
  onMemory: DrawerMemoryUpdate;
  onDiscardFileDraft: (path: string) => Promise<void>;
  onResolveFileDraftConflict: (
    path: string,
    choice: "mine" | "saved",
  ) => Promise<void>;
  onShowDiff: (path: string) => void;
  toast: Toast;
}) {
  const [dir, setDir] = remembered(memory, onMemory, "filesDir");
  const [active, setActive] = remembered(memory, onMemory, "filesActive");
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const [discarding, setDiscarding] = useState(false);
  const [resolving, setResolving] = useState(false);
  const exitEditor = useRef<HTMLButtonElement>(null);
  const openRequest = useRef(0);
  const savingRevision = useRef<{ path: string; hash: string } | null>(null);
  const buffer = active ? memory.filesBuffers[active] : undefined;
  const dirty = Boolean(buffer && buffer.draft !== buffer.base);
  const plainEditing = Boolean(
    buffer &&
    (hasMixedLineEndings(buffer.base) || hasMixedLineEndings(buffer.draft)),
  );

  const updateBuffer = (
    path: string,
    edit: (current: FileBuffer) => FileBuffer,
  ) =>
    onMemory("filesBuffers", (previous) => {
      const current = previous[path];
      if (!current) return previous;
      const next = edit(current);
      return next === current ? previous : { ...previous, [path]: next };
    });

  useEffect(() => {
    setLoading(null);
    return () => {
      ++openRequest.current;
    };
  }, [workspace]);

  function selectFile(path: string | null) {
    ++openRequest.current;
    setLoading(null);
    setActive(path);
  }

  useEffect(() => {
    let cancelled = false;
    void api
      .files(dir)
      .then((result) => {
        if (!cancelled) setFiles(result.entries);
      })
      .catch((reason) => {
        if (!cancelled) setError(String(reason));
      });
    return () => {
      cancelled = true;
    };
  }, [dir, workspace]);

  useEffect(() => {
    if (!active || !buffer) return;
    const path = active;
    let stopped = false;
    let checking = false;
    let observedHash = buffer.hash;
    const observed = (hash: string) => {
      if (hash === observedHash) return;
      observedHash = hash;
      notifyWorkspaceFilesChanged(workspace);
    };
    const check = async () => {
      if (checking) return;
      checking = true;
      let revision: { hash: string } | undefined;
      try {
        revision = await api.fileRevision(path);
        if (stopped) return;
        observed(revision.hash);
        // A save response and the polling read may cross in flight. If the
        // observed disk revision is exactly the one our own save produced,
        // reconcile it as ours instead of surfacing a false external conflict.
        if (savingRevision.current?.path === path) {
          if (savingRevision.current.hash === revision.hash) {
            updateBuffer(path, (current) =>
              current.disk === undefined
                ? current
                : { ...current, disk: undefined },
            );
            return;
          }
          // A later external edit supersedes the just-saved revision; compare
          // it with the current buffer below and surface it if needed.
          savingRevision.current = null;
        }
        if (revision.hash === buffer.hash) {
          updateBuffer(path, (current) =>
            current.disk === undefined
              ? current
              : { ...current, disk: undefined },
          );
          return;
        }
        const disk = await api.file(path, true);
        if (stopped) return;
        updateBuffer(path, (current) => {
          if (
            savingRevision.current?.path === path &&
            savingRevision.current.hash === disk.hash
          )
            return current.disk === undefined
              ? current
              : { ...current, disk: undefined };
          if (disk.hash === current.hash)
            return current.disk === undefined
              ? current
              : { ...current, disk: undefined };
          if (current.draft === current.base || current.draft === disk.content)
            return {
              ...current,
              base: disk.content,
              draft: disk.content,
              hash: disk.hash,
              disk: undefined,
            };
          if (current.disk?.hash === disk.hash) return current;
          return {
            ...current,
            disk: { content: disk.content, hash: disk.hash },
          };
        });
      } catch (reason) {
        if (!stopped) {
          if (String(reason).includes("File not found")) observed("missing");
          if (String(reason).includes("File not found"))
            updateBuffer(path, (current) =>
              current.disk === null ? current : { ...current, disk: null },
            );
          else if (revision)
            updateBuffer(path, (current) => ({
              ...current,
              disk: { content: null, hash: revision!.hash },
            }));
          else
            updateBuffer(path, (current) => ({
              ...current,
              disk: { content: null, hash: "unavailable" },
            }));
        }
      } finally {
        checking = false;
      }
    };
    void check();
    const timer = window.setInterval(() => void check(), 3000);
    window.addEventListener("focus", check);
    return () => {
      stopped = true;
      window.clearInterval(timer);
      window.removeEventListener("focus", check);
    };
    // Reopening an already-held draft must check its disk revision immediately.
  }, [active, buffer?.hash, onMemory, workspace]);

  async function open(path: string) {
    const request = ++openRequest.current;
    setError("");
    if (memory.filesBuffers[path]) {
      setLoading(null);
      setActive(path);
      return;
    }
    setLoading(path);
    try {
      const file = await api.file(path, true);
      if (request !== openRequest.current) return;
      if (file.truncated)
        throw new Error("The file could not be opened completely");
      onMemory("filesBuffers", (previous) =>
        previous[path]
          ? previous
          : {
              ...previous,
              [path]: {
                path: file.path,
                base: file.content,
                draft: file.content,
                hash: file.hash,
              },
            },
      );
      setActive(path);
    } catch (reason) {
      if (request === openRequest.current) setError(String(reason));
    } finally {
      if (request === openRequest.current) setLoading(null);
    }
  }

  async function save(expectedHash?: string, submittedDraft?: string) {
    if (!active || !buffer || saving || discarding) return;
    const path = active;
    const submitted = submittedDraft ?? buffer.draft;
    setSaving(true);
    setError("");
    try {
      const result = await api.saveFile(
        path,
        submitted,
        expectedHash ?? buffer.hash,
      );
      notifyWorkspaceFilesChanged(workspace);
      savingRevision.current = { path, hash: result.hash };
      updateBuffer(path, (current) => {
        // The user can keep typing while the write is in flight. Advance the
        // saved base to the bytes we submitted, but retain that newer draft so
        // a successful save cannot eat edits made after Ctrl+S/click.
        const draftChangedWhileSaving = current.draft !== submitted;
        return {
          ...current,
          base: submitted,
          draft: draftChangedWhileSaving ? current.draft : submitted,
          hash: result.hash,
          disk: undefined,
        };
      });
      toast(`Saved ${path}`, "ok");
    } catch (reason) {
      const message = String(reason);
      if (message.includes("File changed")) {
        let conflictMessage =
          "The file changed on disk. Review both versions before saving.";
        try {
          const current = await api.file(path, true);
          updateBuffer(path, (draft) => ({
            ...draft,
            disk: { content: current.content, hash: current.hash },
          }));
        } catch (readError) {
          if (String(readError).includes("File not found")) {
            updateBuffer(path, (draft) => ({ ...draft, disk: null }));
          } else {
            try {
              const revision = await api.fileRevision(path);
              updateBuffer(path, (draft) => ({
                ...draft,
                disk: { content: null, hash: revision.hash },
              }));
            } catch {
              conflictMessage =
                "The file changed, but its current revision could not be read. Your draft is preserved.";
            }
          }
        }
        setError(conflictMessage);
      } else {
        setError(message);
      }
    } finally {
      setSaving(false);
    }
  }

  async function discardDraft() {
    if (!active || !buffer || discarding) return;
    setDiscarding(true);
    setError("");
    try {
      await onDiscardFileDraft(active);
    } catch (reason) {
      setError(`The draft was kept: ${String(reason)}`);
    } finally {
      setDiscarding(false);
    }
  }

  async function resolveRecovery(choice: "mine" | "saved") {
    if (!active || resolving) return;
    setResolving(true);
    setError("");
    try {
      await onResolveFileDraftConflict(active, choice);
    } catch (reason) {
      setError(`The draft was kept: ${String(reason)}`);
    } finally {
      setResolving(false);
    }
  }

  async function closeFile() {
    if (!active || !buffer || discarding || saving) return;
    if (dirty) {
      selectFile(null);
      return;
    }
    const request = ++openRequest.current;
    setLoading(null);
    setDiscarding(true);
    setError("");
    try {
      // A successful disk save can still have a queued recovery-record
      // deletion. Confirm it before hiding the only visible retry control.
      await onDiscardFileDraft(buffer.path);
      onMemory("filesBuffers", (previous) => {
        const next = { ...previous };
        delete next[buffer.path];
        return next;
      });
      if (request === openRequest.current) selectFile(null);
    } catch (reason) {
      setError(`The file stayed open: ${String(reason)}`);
    } finally {
      setDiscarding(false);
    }
  }

  function editKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      exitEditor.current?.focus();
    } else if (
      (event.ctrlKey || event.metaKey) &&
      event.key.toLowerCase() === "s"
    ) {
      event.preventDefault();
      if (
        dirty &&
        buffer?.disk === undefined &&
        buffer?.recoveryConflict === undefined
      )
        void save();
    } else if (event.key === "Tab" && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      const input = event.currentTarget;
      const start = input.selectionStart;
      const end = input.selectionEnd;
      const text = input.value;
      let selectionStart = start;
      let selectionEnd = end;
      if (active) {
        if (start === end && !event.shiftKey) {
          updateBuffer(active, (current) => ({
            ...current,
            draft: plainEditorRange(current.draft, start, end, "  "),
          }));
          selectionStart = selectionEnd = start + 2;
        } else {
          const lineStart = text.lastIndexOf("\n", start - 1) + 1;
          const lastSelected =
            end > start && text[end - 1] === "\n" ? end - 1 : end;
          const nextBreak = text.indexOf("\n", lastSelected);
          const lineEnd = nextBreak === -1 ? text.length : nextBreak;
          const before = text.slice(lineStart, lineEnd);
          const after = before
            .split("\n")
            .map((line) =>
              event.shiftKey ? line.replace(/^ {1,2}/, "") : `  ${line}`,
            )
            .join("\n");
          const outdent = event.shiftKey;
          updateBuffer(active, (current) => ({
            ...current,
            draft: plainEditorRange(
              current.draft,
              lineStart,
              lineEnd,
              (selected) =>
                selected
                  .split(/(\r\n|\r|\n)/)
                  .map((part, index) =>
                    index % 2
                      ? part
                      : outdent
                        ? part.replace(/^ {1,2}/, "")
                        : `  ${part}`,
                  )
                  .join(""),
            ),
          }));
          if (start === end) {
            selectionStart = selectionEnd = Math.max(
              lineStart,
              start - (before.length - after.length),
            );
          } else {
            selectionStart = lineStart;
            selectionEnd = lineStart + after.length;
          }
        }
      }
      requestAnimationFrame(() => {
        input.setSelectionRange(selectionStart, selectionEnd);
      });
    }
  }

  return (
    <>
      <div className="crumb">
        <button
          type="button"
          className="mini"
          disabled={dir === "."}
          onClick={() => setDir(dir.split("/").slice(0, -1).join("/") || ".")}
        >
          ↑
        </button>
        <span>
          {workspace.split("/").pop()}
          {dir === "." ? "" : `/${dir}`}
        </span>
      </div>
      <div className="list">
        {files.length === 0 && <Empty title="Empty folder" />}
        {files.map((file) => (
          <button
            type="button"
            key={file.path}
            className="file"
            onClick={() =>
              file.type === "dir" ? setDir(file.path) : void open(file.path)
            }
          >
            <span className="file-icon" aria-hidden="true">
              {file.type === "dir" ? "▸" : "·"}
            </span>
            {file.name}
            {memory.filesBuffers[file.path]?.draft !==
              memory.filesBuffers[file.path]?.base &&
              memory.filesBuffers[file.path] &&
              " •"}
          </button>
        ))}
      </div>
      {Object.keys(memory.filesBuffers).length > 0 && (
        <div className="file-editor-tabs" aria-label="Open files">
          {Object.values(memory.filesBuffers).map((item) => (
            <button
              type="button"
              key={item.path}
              aria-current={item.path === active ? "page" : undefined}
              aria-label={`Open ${item.path}${item.draft !== item.base ? ", unsaved" : ""}`}
              title={item.path}
              onClick={() => selectFile(item.path)}
            >
              {item.path.split("/").pop()}
              {item.draft !== item.base ? " •" : ""}
            </button>
          ))}
        </div>
      )}
      {loading !== null && <p role="status">Opening {loading}…</p>}
      {error && (
        <p className="file-editor-error" role="alert">
          {error}
        </p>
      )}
      {memory.filesRecoveryError && (
        <p className="file-editor-error" role="alert">
          {memory.filesRecoveryError}
        </p>
      )}
      {buffer && (
        <div className="file-view file-editor">
          <div className="crumb">
            <span title={buffer.path}>{buffer.path}</span>
            <button
              type="button"
              className="mini"
              ref={exitEditor}
              onClick={() => onShowDiff(buffer.path)}
            >
              Diff
            </button>
            <button
              type="button"
              className="mini"
              aria-label={dirty ? "Hide editor and keep draft" : "Close file"}
              disabled={discarding || saving}
              onClick={() => void closeFile()}
              title={
                dirty ? "Hide editor; your draft stays open" : "Close file"
              }
            >
              ×
            </button>
          </div>
          <div className="file-editor-toolbar">
            <span role="status">
              {buffer.disk !== undefined
                ? "Conflict on disk · draft preserved"
                : dirty
                  ? "Unsaved changes"
                  : "Saved"}
            </span>
            <button
              type="button"
              className="mini"
              disabled={
                !dirty ||
                saving ||
                discarding ||
                buffer.disk !== undefined ||
                buffer.recoveryConflict !== undefined
              }
              onClick={() => void save()}
            >
              {saving ? "Saving…" : "Save"}
            </button>
            <button
              type="button"
              className="mini"
              disabled={
                !dirty ||
                discarding ||
                saving ||
                buffer.recoveryConflict !== undefined
              }
              onClick={() => void discardDraft()}
            >
              {discarding ? "Discarding…" : "Discard draft"}
            </button>
            {!dirty && buffer.recoveryStatus === "error" && (
              <button
                type="button"
                className="mini"
                disabled={discarding}
                onClick={() => void discardDraft()}
              >
                Retry recovery cleanup
              </button>
            )}
          </div>
          {!dirty && buffer.recoveryStatus === "error" && (
            <p className="file-editor-error" role="alert">
              {buffer.recoveryError}
            </p>
          )}
          {dirty && (
            <p className="file-editor-hint" role="status">
              {buffer.recoveryStatus === "saved"
                ? "Recovery copy saved on this computer"
                : buffer.recoveryStatus === "saving"
                  ? "Saving recovery copy…"
                  : buffer.recoveryStatus === "error"
                    ? `Recovery copy not saved: ${buffer.recoveryError}`
                    : "Unsaved draft is in memory"}
            </p>
          )}
          {buffer.recoveryConflict !== undefined && (
            <div
              className="file-editor-conflict"
              role="group"
              aria-label="Saved draft conflict"
            >
              <strong>Saved draft changed in another window</strong>
              <p>
                Your text remains in the editor. Review the other saved copy
                before choosing which draft to keep.
              </p>
              {buffer.recoveryConflict ? (
                <details>
                  <summary>Show other window's saved draft</summary>
                  <pre>{buffer.recoveryConflict.draft}</pre>
                </details>
              ) : (
                <p>The other saved copy was removed.</p>
              )}
              <div className="file-editor-toolbar">
                <button
                  type="button"
                  className="mini"
                  disabled={resolving}
                  onClick={() => void resolveRecovery("mine")}
                >
                  Keep this window's draft
                </button>
                {buffer.recoveryConflict && (
                  <button
                    type="button"
                    className="mini"
                    disabled={resolving}
                    onClick={() => void resolveRecovery("saved")}
                  >
                    Use other saved draft
                  </button>
                )}
              </div>
            </div>
          )}
          {buffer.disk !== undefined && (
            <div
              className="file-editor-conflict"
              role="group"
              aria-label="File conflict"
            >
              <strong>
                {buffer.disk === null
                  ? "File deleted on disk"
                  : buffer.disk.content === null
                    ? "Current file is unavailable or no longer readable as text"
                    : "File changed on disk"}
              </strong>
              <p>
                Your draft is still open. Review the current file before
                replacing it.
              </p>
              <details>
                <summary>Show version before your edits</summary>
                <pre>{buffer.base}</pre>
              </details>
              {buffer.disk && buffer.disk.content !== null && (
                <details>
                  <summary>Show current disk version</summary>
                  <pre>{buffer.disk.content}</pre>
                </details>
              )}
              <div className="file-editor-toolbar">
                {buffer.disk && buffer.disk.content !== null ? (
                  <>
                    <button
                      type="button"
                      className="mini"
                      onClick={() =>
                        updateBuffer(buffer.path, (current) => ({
                          ...current,
                          base: buffer.disk!.content!,
                          draft: buffer.disk!.content!,
                          hash: buffer.disk!.hash,
                          disk: undefined,
                        }))
                      }
                    >
                      Use disk version
                    </button>
                    <button
                      type="button"
                      className="mini"
                      onClick={() =>
                        updateBuffer(buffer.path, (current) => ({
                          ...current,
                          base: buffer.disk!.content!,
                          hash: buffer.disk!.hash,
                          disk: undefined,
                        }))
                      }
                    >
                      Use disk revision as save base
                    </button>
                  </>
                ) : buffer.disk === null ? (
                  <button
                    type="button"
                    className="mini"
                    onClick={() => void save("missing")}
                  >
                    Recreate from draft
                  </button>
                ) : null}
              </div>
            </div>
          )}
          <CodeEditor
            suspended={plainEditing}
            workspace={workspace}
            path={buffer.path}
            openPaths={Object.keys(memory.filesBuffers)}
            value={buffer.draft}
            disabled={discarding || resolving}
            onChange={(draft) =>
              updateBuffer(buffer.path, (current) => ({ ...current, draft }))
            }
            onSave={(draft) => {
              if (
                draft !== buffer.base &&
                buffer.disk === undefined &&
                buffer.recoveryConflict === undefined
              )
                void save(undefined, draft);
            }}
            onEscape={() => exitEditor.current?.focus()}
          />
          {plainEditing && (
            <textarea
              className="file-editor-input"
              aria-label={`Edit ${buffer.path}`}
              disabled={discarding || resolving}
              spellCheck={false}
              value={buffer.draft.replace(/\r\n|\r/g, "\n")}
              onChange={(event) =>
                updateBuffer(buffer.path, (current) => ({
                  ...current,
                  draft: plainEditorEdit(current.draft, event.target.value),
                }))
              }
              onKeyDown={editKey}
            />
          )}
          <p className="file-editor-hint">
            {plainEditing && "Mixed line endings · plain editing · "}
            Ctrl/⌘+S to save · Tab indents · Shift+Tab outdents · Esc leaves the
            editor{!plainEditing && " · Ctrl/⌘+F to find"}
          </p>
        </div>
      )}
    </>
  );
}
