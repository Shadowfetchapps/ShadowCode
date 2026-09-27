import { useEffect, useRef, useState, type KeyboardEvent } from "react";
import { api, type FileEntry } from "../api";
import {
  remembered,
  type DrawerMemory,
  type DrawerMemoryUpdate,
  type FileBuffer,
} from "../hooks/useDrawerMemory";
import { Empty } from "./cards";

type Toast = (text: string, kind?: "ok" | "err" | "info") => void;

/** File drafts are owned by the app, not the drawer tab. Every save is checked
 * against the revision originally opened or explicitly reviewed by the user. */
export function FileEditor({
  workspace,
  memory,
  onMemory,
  onShowDiff,
  toast,
}: {
  workspace: string;
  memory: DrawerMemory;
  onMemory: DrawerMemoryUpdate;
  onShowDiff: (path: string) => void;
  toast: Toast;
}) {
  const [dir, setDir] = remembered(memory, onMemory, "filesDir");
  const [active, setActive] = remembered(memory, onMemory, "filesActive");
  const [files, setFiles] = useState<FileEntry[]>([]);
  const [loading, setLoading] = useState<string | null>(null);
  const [error, setError] = useState("");
  const [saving, setSaving] = useState(false);
  const exitEditor = useRef<HTMLButtonElement>(null);
  const buffer = active ? memory.filesBuffers[active] : undefined;
  const dirty = Boolean(buffer && buffer.draft !== buffer.base);

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
    const check = async () => {
      if (checking) return;
      checking = true;
      let revision: { hash: string } | undefined;
      try {
        revision = await api.fileRevision(path);
        if (stopped) return;
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
    setActive(path);
    setError("");
    if (memory.filesBuffers[path]) return;
    setLoading(path);
    try {
      const file = await api.file(path, true);
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
    } catch (reason) {
      setError(String(reason));
    } finally {
      setLoading(null);
    }
  }

  async function save(expectedHash?: string) {
    if (!active || !buffer || saving) return;
    const path = active;
    const submitted = buffer.draft;
    setSaving(true);
    setError("");
    try {
      const result = await api.saveFile(
        path,
        submitted,
        expectedHash ?? buffer.hash,
      );
      updateBuffer(path, (current) => ({
        ...current,
        base: submitted,
        hash: result.hash,
        disk: undefined,
      }));
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

  function editKey(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.key === "Escape") {
      event.preventDefault();
      exitEditor.current?.focus();
    } else if (
      (event.ctrlKey || event.metaKey) &&
      event.key.toLowerCase() === "s"
    ) {
      event.preventDefault();
      if (dirty && buffer?.disk === undefined) void save();
    } else if (event.key === "Tab" && !event.ctrlKey && !event.metaKey) {
      event.preventDefault();
      const input = event.currentTarget;
      const start = input.selectionStart;
      const end = input.selectionEnd;
      if (active)
        updateBuffer(active, (current) => ({
          ...current,
          draft:
            current.draft.slice(0, start) + "  " + current.draft.slice(end),
        }));
      requestAnimationFrame(() => {
        input.selectionStart = input.selectionEnd = start + 2;
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
              onClick={() => setActive(item.path)}
            >
              {item.path.split("/").pop()}
              {item.draft !== item.base ? " •" : ""}
            </button>
          ))}
        </div>
      )}
      {loading !== null && loading === active && (
        <p role="status">Opening {active}…</p>
      )}
      {error && (
        <p className="file-editor-error" role="alert">
          {error}
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
              onClick={() => {
                if (dirty) {
                  setActive(null);
                } else {
                  onMemory("filesBuffers", (previous) => {
                    const next = { ...previous };
                    delete next[buffer.path];
                    return next;
                  });
                  setActive(null);
                }
              }}
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
              disabled={!dirty || saving || buffer.disk !== undefined}
              onClick={() => void save()}
            >
              {saving ? "Saving…" : "Save"}
            </button>
            <button
              type="button"
              className="mini"
              disabled={!dirty}
              onClick={() =>
                updateBuffer(buffer.path, (current) => ({
                  ...current,
                  draft: current.base,
                }))
              }
            >
              Discard draft
            </button>
          </div>
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
          <textarea
            className="file-editor-input"
            aria-label={`Edit ${buffer.path}`}
            spellCheck={false}
            value={buffer.draft}
            onChange={(event) =>
              updateBuffer(buffer.path, (current) => ({
                ...current,
                draft: event.target.value,
              }))
            }
            onKeyDown={editKey}
          />
          <p className="file-editor-hint">
            Ctrl/⌘+S to save · Tab inserts two spaces · Esc leaves the editor
          </p>
        </div>
      )}
    </>
  );
}
