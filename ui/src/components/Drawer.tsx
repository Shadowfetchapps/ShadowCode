import { X } from "lucide-react";
import { useEffect, useState } from "react";
import { api, type Session } from "../api";
import { Empty } from "./cards";
import { ChangesTab } from "./ChangesTab";
import { BranchPanel } from "./BranchPanel";
import { PreviewPanel } from "./PreviewPanel";
import { FileEditor } from "./FileEditor";
import { TerminalPanel } from "./TerminalPanel";
import { ToolsTab, type ToolsView } from "./ToolsTab";
import { exportSession } from "../lib/transport";
import {
  type DrawerMemory,
  type DrawerMemoryUpdate,
} from "../hooks/useDrawerMemory";

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
  { id: "sessions", label: "Sessions" },
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
}) {
  const tool = isToolTab(tab) ? tab : null;
  useEffect(() => {
    if (tool) onMemory("toolsView", tool);
  }, [tool, onMemory]);
  return (
    <aside className="drawer" aria-label="Drawer">
      <div className="drawer-tabs">
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
          <PreviewPanel
            workspace={workspace}
            toast={toast}
            memory={memory}
            onMemory={onMemory}
          />
        )}
        {tab === "git" && (
          <BranchPanel
            busy={busy}
            toast={toast}
            memory={memory}
            onMemory={onMemory}
            onOpenTerminal={() => onTab("terminal")}
          />
        )}
        {tool && (
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
        )}
        {tab === "files" && (
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
        )}
        {tab === "changes" && (
          <ChangesTab
            path={diffPath}
            busy={busy}
            toast={toast}
            onAskAgent={onAskAgent}
            memory={memory}
            onMemory={onMemory}
          />
        )}
      </div>
    </aside>
  );
}

// --- Sessions: search · rename · delete · branch ----------------------------

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

  async function rename() {
    if (!renaming) return;
    try {
      await api.renameSession(renaming.id, renaming.title);
      setRenaming(null);
      await onRefresh();
      if (q) setHits((await api.sessions(q)).sessions);
    } catch (err) {
      toast(String(err), "err");
    }
  }

  async function remove(id: string) {
    if (!window.confirm("Delete this session and its transcript?")) return;
    try {
      await api.deleteSession(id);
      toast("Session deleted", "ok");
      await onRefresh();
      if (q) setHits((await api.sessions(q)).sessions);
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
          placeholder="Search sessions…"
        />
        <button type="button" className="mini" onClick={onNew}>
          New
        </button>
      </div>
      {rows.length === 0 && (
        <Empty
          title={q ? "No matches" : "No sessions yet"}
          body={q ? undefined : "Run a task and it shows up here."}
        />
      )}
      <div className="list">
        {rows.map((s) => (
          <div
            key={s.id}
            className={`item ${s.id === sessionId ? "active" : ""}`}
            role="button"
            tabIndex={0}
            onKeyDown={(e) => {
              if (
                e.target === e.currentTarget &&
                (e.key === "Enter" || e.key === " ")
              )
                onOpen(s.id);
            }}
            onClick={() => onOpen(s.id)}
          >
            {renaming?.id === s.id ? (
              <input
                autoFocus
                className="rename"
                value={renaming.title}
                onClick={(e) => e.stopPropagation()}
                onChange={(e) =>
                  setRenaming({ id: s.id, title: e.target.value })
                }
                onKeyDown={(e) => {
                  if (e.key === "Enter") void rename();
                  if (e.key === "Escape") setRenaming(null);
                }}
                onBlur={() => void rename()}
              />
            ) : (
              <strong>
                {s.title || "Untitled"}
                {s.parent_id ? " ↳" : ""}
              </strong>
            )}
            <span>
              {s.status} · {s.workspace.split("/").pop()}
            </span>
            <div className="item-actions" onClick={(e) => e.stopPropagation()}>
              <button
                type="button"
                className="mini"
                onClick={() => setRenaming({ id: s.id, title: s.title || "" })}
              >
                Rename
              </button>
              <button
                type="button"
                className="mini"
                title="Fork this session"
                onClick={() =>
                  void api.branchSession(s.id).then(() => onRefresh())
                }
              >
                Branch
              </button>
              <button
                type="button"
                className="mini"
                title="Export as Markdown"
                onClick={() =>
                  void exportSession(s.id).catch((e) => toast(String(e), "err"))
                }
              >
                Export
              </button>
              <button
                type="button"
                className="mini danger-text"
                onClick={() => void remove(s.id)}
              >
                Delete
              </button>
            </div>
          </div>
        ))}
      </div>
    </>
  );
}
