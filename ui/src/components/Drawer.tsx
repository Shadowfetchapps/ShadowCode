import { X } from "lucide-react";
import { lazy, Suspense, useEffect, useState } from "react";
import { api, type Session } from "../api";
import { Empty } from "./cards";
import { ConfirmDialog } from "./ConfirmDialog";
import { relativeTime } from "../lib/picker";
import { TerminalPanel } from "./TerminalPanel";
import type { ToolsView } from "./ToolsTab";
import { exportSession } from "../lib/transport";
import type { PickerTarget } from "../lib/picker";
import {
  type DrawerMemory,
  type DrawerMemoryUpdate,
} from "../hooks/useDrawerMemory";

const FileEditor = lazy(() =>
  import("./FileEditor").then((module) => ({ default: module.FileEditor })),
);
const ChangesTab = lazy(() =>
  import("./ChangesTab").then((module) => ({ default: module.ChangesTab })),
);
const BranchPanel = lazy(() =>
  import("./BranchPanel").then((module) => ({ default: module.BranchPanel })),
);
const PreviewPanel = lazy(() =>
  import("./PreviewPanel").then((module) => ({ default: module.PreviewPanel })),
);
const ToolsTab = lazy(() =>
  import("./ToolsTab").then((module) => ({ default: module.ToolsTab })),
);

function PanelFallback() {
  return <p role="status">Opening panel…</p>;
}

/** The optional right-hand drawer: the readable diff of current changes,
 * Git (commit, push, pull requests), the user's own terminals, files,
 * conversations, the Preview of the project's dev server (with element
 * picking), and Tools used while working (goals, automations, issues,
 * background processes, worktrees). Work in a tab (the open file, a commit message, the terminal in
 * front) lives in `memory`, owned by the app, so switching tabs keeps it; the
 * terminals themselves run in the engine. */
export type DrawerTab =
  "changes" | "git" | "terminal" | "preview" | "files" | "sessions" | ToolsView;
const TOOLS: readonly DrawerTab[] = [
  "goals",
  "automations",
  "issues",
  "background",
  "worktrees",
];
export const isToolTab = (tab: DrawerTab): tab is ToolsView =>
  TOOLS.includes(tab);
export const DRAWER_TABS: { id: DrawerTab | "tools"; label: string }[] = [
  { id: "changes", label: "Changes" },
  { id: "git", label: "Git" },
  { id: "terminal", label: "Terminal" },
  { id: "preview", label: "Preview" },
  { id: "files", label: "Files" },
  { id: "sessions", label: "Tasks" },
  { id: "tools", label: "Tools" },
];

type Toast = (text: string, kind?: "ok" | "err" | "info") => void;

export function Drawer({
  tab,
  onTab,
  onClose,
  workspace,
  sessions,
  sessionId,
  onOpenSession,
  onNewSession,
  onRefreshSessions,
  diffPath,
  onDiffPath,
  busy,
  toast,
  onAskAgent,
  onOpenProject,
  memory,
  onMemory,
  onDiscardFileDraft,
  onResolveFileDraftConflict,
  targets = [],
}: {
  tab: DrawerTab;
  onTab: (t: DrawerTab) => void;
  onClose: () => void;
  workspace: string;
  sessions: Session[];
  sessionId: string;
  onOpenSession: (id: string) => void;
  onNewSession: () => void;
  onRefreshSessions: () => Promise<void>;
  diffPath: string;
  onDiffPath: (path: string) => void;
  busy: boolean;
  toast: Toast;
  onAskAgent?: (prompt: string) => void;
  onOpenProject?: (path: string) => void;
  memory: DrawerMemory;
  onMemory: DrawerMemoryUpdate;
  onDiscardFileDraft: (path: string) => Promise<void>;
  onResolveFileDraftConflict: (
    path: string,
    choice: "mine" | "saved",
  ) => Promise<void>;
  /** Picker rows, for the Git tab's reviewer. */
  targets?: PickerTarget[];
}) {
  const tool = isToolTab(tab) ? tab : null;
  useEffect(() => {
    if (tool) onMemory("toolsView", tool);
  }, [tool, onMemory]);
  return (
    <aside className="drawer" aria-label="Drawer">
      <div className="drawer-tabs">
        {/* The tabs wrap inside their own group so Close stays on the first
            row, at the top right, whatever the drawer's width. */}
        <div className="drawer-tab-list" role="group" aria-label="Panels">
          {DRAWER_TABS.map((t) => {
            const on = t.id === "tools" ? Boolean(tool) : tab === t.id;
            return (
              <button
                type="button"
                key={t.id}
                className={on ? "on" : ""}
                aria-pressed={on}
                onClick={() =>
                  onTab(t.id === "tools" ? memory.toolsView || "goals" : t.id)
                }
              >
                {t.label}
              </button>
            );
          })}
        </div>
        <button
          type="button"
          className="icon-btn drawer-close"
          aria-label="Close drawer"
          title="Close (Esc)"
          onClick={onClose}
        >
          <X size={16} aria-hidden="true" />
        </button>
      </div>
      <div className="drawer-body">
        {tab === "sessions" && (
          <SessionsTab
            sessions={sessions}
            sessionId={sessionId}
            onOpen={onOpenSession}
            onNew={onNewSession}
            onRefresh={onRefreshSessions}
            toast={toast}
          />
        )}
        {tab === "terminal" && (
          <TerminalPanel
            workspace={workspace}
            toast={toast}
            memory={memory}
            onMemory={onMemory}
          />
        )}
        {tab === "preview" && (
          <Suspense fallback={<PanelFallback />}>
            <PreviewPanel
              workspace={workspace}
              toast={toast}
              memory={memory}
              onMemory={onMemory}
            />
          </Suspense>
        )}
        {tab === "git" && (
          <Suspense fallback={<PanelFallback />}>
            <BranchPanel
              busy={busy}
              toast={toast}
              memory={memory}
              onMemory={onMemory}
              onOpenTerminal={() => onTab("terminal")}
              workspace={workspace}
              sessionId={sessionId}
              targets={targets}
            />
          </Suspense>
        )}
        {tool && (
          <Suspense fallback={<PanelFallback />}>
            <ToolsTab
              view={tool}
              onView={onTab}
              sessionId={sessionId}
              busy={busy}
              toast={toast}
              onOpenSession={onOpenSession}
              onOpenProject={onOpenProject}
              onAskAgent={onAskAgent}
              onIssueLinked={(link) => onMemory("issueTask", link)}
              onOpenTerminal={() => onTab("terminal")}
            />
          </Suspense>
        )}
        {tab === "files" && (
          <Suspense fallback={<p role="status">Opening the file editor…</p>}>
            <FileEditor
              workspace={workspace}
              memory={memory}
              onMemory={onMemory}
              onDiscardFileDraft={onDiscardFileDraft}
              onResolveFileDraftConflict={onResolveFileDraftConflict}
              toast={toast}
              onShowDiff={(p) => {
                onDiffPath(p);
                onTab("changes");
              }}
            />
          </Suspense>
        )}
        {tab === "changes" && (
          <Suspense fallback={<PanelFallback />}>
            <ChangesTab
              path={diffPath}
              busy={busy}
              toast={toast}
              onAskAgent={onAskAgent}
              memory={memory}
              onMemory={onMemory}
            />
          </Suspense>
        )}
      </div>
    </aside>
  );
}

// --- Tasks: search · rename · fork · export · delete ------------------------

function SessionsTab({
  sessions,
  sessionId,
  onOpen,
  onNew,
  onRefresh,
  toast,
}: {
  sessions: Session[];
  sessionId: string;
  onOpen: (id: string) => void;
  onNew: () => void;
  onRefresh: () => Promise<void>;
  toast: Toast;
}) {
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<Session[] | null>(null);
  const [renaming, setRenaming] = useState<{
    id: string;
    title: string;
  } | null>(null);
  const [deleting, setDeleting] = useState<Session | null>(null);

  useEffect(() => {
    if (!q.trim()) {
      setHits(null);
      return;
    }
    const handle = setTimeout(
      () =>
        void api
          .sessions(q)
          .then((d) => setHits(d.sessions))
          .catch(() => setHits([])),
      180,
    );
    return () => clearTimeout(handle);
  }, [q]);

  const rows = hits ?? sessions;

  async function afterChange() {
    await onRefresh();
    if (q) setHits((await api.sessions(q)).sessions);
  }

  async function rename() {
    if (!renaming) return;
    try {
      await api.renameSession(renaming.id, renaming.title);
      setRenaming(null);
      await afterChange();
    } catch (err) {
      toast(String(err), "err");
    }
  }

  async function remove(id: string) {
    try {
      await api.deleteSession(id);
      toast("Task deleted", "ok");
      await afterChange();
    } catch (err) {
      toast(String(err), "err");
    }
  }

  // The same Fork and Export as the sidebar's task menu, with the same words.
  async function fork(id: string) {
    try {
      const copy = await api.branchSession(id);
      await afterChange();
      onOpen(copy.id);
      toast("Forked. The copy continues from the same history.", "ok");
    } catch (err) {
      toast(String(err), "err");
    }
  }

  async function exportTask(id: string) {
    try {
      const saved = await exportSession(id);
      if (saved) toast(`Exported to ${saved}`, "ok");
    } catch (err) {
      toast(String(err), "err");
    }
  }

  return (
    <>
      <div className="drawer-toolbar">
        <input
          className="search"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder="Search tasks…"
          aria-label="Search tasks"
        />
        <button type="button" className="mini" onClick={onNew}>
          New task
        </button>
      </div>
      {rows.length === 0 && (
        <Empty
          title={q ? "No matches" : "No tasks yet"}
          body={q ? undefined : "Run a task and it shows up here."}
        />
      )}
      <div className="list">
        {rows.map((s) => {
          const title = s.title || "New task";
          return (
            <div
              key={s.id}
              className={`item ${s.id === sessionId ? "active" : ""}`}
            >
              {renaming?.id === s.id ? (
                <input
                  autoFocus
                  className="rename"
                  aria-label={`Rename ${title}`}
                  value={renaming.title}
                  onChange={(e) =>
                    setRenaming({ id: s.id, title: e.target.value })
                  }
                  onKeyDown={(e) => {
                    if (e.key === "Enter") void rename();
                    if (e.key === "Escape") {
                      e.stopPropagation();
                      setRenaming(null);
                    }
                  }}
                  onBlur={() => void rename()}
                />
              ) : (
                // One button opens the task; its actions are separate
                // buttons beside it, never nested inside it.
                <button
                  type="button"
                  className="item-open"
                  aria-current={s.id === sessionId ? "page" : undefined}
                  onClick={() => onOpen(s.id)}
                >
                  <strong>
                    {title}
                    {s.parent_id ? " ↳" : ""}
                  </strong>
                  <span>
                    {s.updated_at
                      ? `Updated ${relativeTime(s.updated_at)}`
                      : "Not started"}
                    {" · "}
                    {s.workspace.split("/").pop()}
                  </span>
                </button>
              )}
              <div className="item-actions">
                <button
                  type="button"
                  className="mini"
                  aria-label={`Rename ${title}`}
                  onClick={() =>
                    setRenaming({ id: s.id, title: s.title || "" })
                  }
                >
                  Rename
                </button>
                <button
                  type="button"
                  className="mini"
                  aria-label={`Fork ${title}`}
                  title="Copy this task and continue from the same history"
                  onClick={() => void fork(s.id)}
                >
                  Fork
                </button>
                <button
                  type="button"
                  className="mini"
                  aria-label={`Export ${title}`}
                  title="Save as Markdown"
                  onClick={() => void exportTask(s.id)}
                >
                  Export
                </button>
                <button
                  type="button"
                  className="mini danger-text"
                  aria-label={`Delete ${title}`}
                  onClick={() => setDeleting(s)}
                >
                  Delete
                </button>
              </div>
            </div>
          );
        })}
      </div>
      {deleting && (
        <ConfirmDialog
          title="Delete this task?"
          confirmLabel="Delete"
          danger
          onCancel={() => setDeleting(null)}
          onConfirm={async () => {
            await remove(deleting.id);
            setDeleting(null);
          }}
        >
          <p>“{deleting.title || "New task"}” and its history are removed.</p>
        </ConfirmDialog>
      )}
    </>
  );
}
