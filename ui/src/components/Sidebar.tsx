import { useEffect, useState } from "react";
import {
  ArrowUpRight,
  ChevronDown,
  CircleAlert,
  Clock3,
  FolderOpen,
  GitBranch,
  Hand,
  MessageSquare,
  PanelLeftClose,
  Pin,
  Plus,
  Search,
  Settings2,
  SquarePen,
} from "lucide-react";
import { api, type Job, type Project, type Session } from "../api";
import { BADGE_LABELS, projectOf, type Badge } from "../lib/badges";
import { readPins, writePins } from "../lib/storage";
import { ConversationMenu, type ConversationAction } from "./ConversationMenu";

/** Projects and their recent conversations, with per-conversation badges
 * (running, needs approval, failed, finished-unread) and a right-click menu
 * (Rename, Pin, Fork, Export, Delete). */
export function Sidebar({
  sessions,
  projects,
  selected,
  workspace,
  jobs,
  badges,
  onSelect,
  onNew,
  onProject,
  onSettings,
  onHide,
  onAction,
}: {
  sessions: Session[];
  projects: Project[];
  selected: string;
  workspace: string;
  jobs: Job[];
  badges?: Record<string, Badge>;
  onSelect: (id: string) => void;
  onNew: () => void;
  onProject: (path?: string) => void;
  onSettings: () => void;
  onHide: () => void;
  /** Rename (with the new title), Fork, Export or Delete a conversation. */
  onAction?: (
    action: Exclude<ConversationAction, "pin">,
    session: Session,
    title?: string,
  ) => void;
}) {
  const [renaming, setRenaming] = useState("");
  const [menu, setMenu] = useState<{ id: string; x: number; y: number } | null>(
    null,
  );
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<Session[] | null>(null);
  const [searchError, setSearchError] = useState("");
  const [collapsed, setCollapsed] = useState<string[]>([]);
  const [pins, setPins] = useState<string[]>(readPins);
  useEffect(() => {
    let alive = true;
    if (!query.trim()) {
      setHits(null);
      setSearchError("");
      return;
    }
    const timer = setTimeout(
      () =>
        api
          .sessions(query)
          .then((r) => {
            if (alive) {
              setHits(r.sessions);
              setSearchError("");
            }
          })
          .catch(() => {
            if (alive) setSearchError("Search unavailable. Try again.");
          }),
      200,
    );
    return () => {
      alive = false;
      clearTimeout(timer);
    };
  }, [query, sessions]);
  const rows = hits ?? sessions;
  const groups = [
    ...new Set([
      workspace,
      ...projects.map((p) => p.path),
      ...rows.map(projectOf),
    ]),
  ].filter(Boolean);
  function pin(id: string) {
    const next = pins.includes(id)
      ? pins.filter((p) => p !== id)
      : [...pins, id];
    setPins(next);
    writePins(next);
  }
  const menuSession = menu && rows.find((s) => s.id === menu.id);
  function task(s: Session) {
    const badge =
      badges?.[s.id] ??
      (jobs.some(
        (j) =>
          j.session_id === s.id && ["running", "cancelling"].includes(j.status),
      )
        ? "running"
        : jobs.some((j) => j.session_id === s.id && j.status === "queued")
          ? "queued"
          : null);
    const title = s.title || "New task";
    if (renaming === s.id)
      return (
        <div className="task-row selected" key={s.id}>
          <input
            className="task-rename"
            aria-label={`Rename ${title}`}
            defaultValue={s.title || ""}
            autoFocus
            onFocus={(e) => e.currentTarget.select()}
            onKeyDown={(e) => {
              if (e.key === "Enter") {
                const next = e.currentTarget.value.trim();
                setRenaming("");
                if (next && next !== s.title) onAction?.("rename", s, next);
              } else if (e.key === "Escape") {
                e.stopPropagation();
                setRenaming("");
              }
            }}
            onBlur={() => setRenaming("")}
          />
        </div>
      );
    return (
      <div
        className={`task-row ${s.id === selected ? "selected" : ""}${badge ? ` badge-${badge}` : ""}`}
        key={s.id}
      >
        <button
          type="button"
          className="task-link"
          data-session-id={s.id}
          data-badge={badge || undefined}
          aria-current={s.id === selected ? "page" : undefined}
          aria-haspopup="menu"
          title={badge ? `${title} · ${BADGE_LABELS[badge]}` : title}
          onClick={() => onSelect(s.id)}
          onContextMenu={(e) => {
            if (!onAction) return;
            e.preventDefault();
            setMenu({ id: s.id, x: e.clientX, y: e.clientY });
          }}
          onKeyDown={(e) => {
            if (
              onAction &&
              (e.key === "ContextMenu" || (e.shiftKey && e.key === "F10"))
            ) {
              e.preventDefault();
              const box = e.currentTarget.getBoundingClientRect();
              setMenu({ id: s.id, x: box.left + 24, y: box.bottom });
            }
          }}
        >
          {badge === "running" ? (
            <span
              className="running-dot"
              role="img"
              aria-label={BADGE_LABELS.running}
            />
          ) : badge === "queued" ? (
            <Clock3 size={14} aria-label={BADGE_LABELS.queued} />
          ) : badge === "approval" ? (
            <Hand
              size={14}
              className="badge-approval-icon"
              aria-label={BADGE_LABELS.approval}
            />
          ) : badge === "failed" ? (
            <CircleAlert
              size={14}
              className="badge-failed-icon"
              aria-label={BADGE_LABELS.failed}
            />
          ) : s.worktree_task ? (
            <GitBranch size={14} aria-label="Runs in its own worktree" />
          ) : (
            <MessageSquare size={14} />
          )}
          <span className="task-title">{title}</span>
          {badge === "unread" && (
            <span
              className="unread-dot"
              role="img"
              aria-label={BADGE_LABELS.unread}
            />
          )}
        </button>
        <button
          type="button"
          className={`pin-task ${pins.includes(s.id) ? "pinned" : ""}`}
          aria-label={`${pins.includes(s.id) ? "Unpin" : "Pin"} ${s.title || "task"}`}
          onClick={() => pin(s.id)}
        >
          <Pin size={12} />
        </button>
      </div>
    );
  }
  return (
    <aside className="sidebar" aria-label="Projects and tasks">
      <div className="brand">
        <img src="/icon-192.png" alt="" />
        <strong>ShadowCode</strong>
        <span className="grow" />
        <button
          type="button"
          className="icon-btn"
          title="Hide sidebar (Ctrl+B)"
          aria-label="Hide sidebar"
          onClick={onHide}
        >
          <PanelLeftClose size={17} />
        </button>
      </div>
      <button type="button" className="new-task" onClick={onNew}>
        <SquarePen size={17} />
        <span>New task</span>
        <kbd>Ctrl N</kbd>
      </button>
      <label className="sidebar-search">
        <Search size={15} />
        <input
          aria-label="Search tasks"
          placeholder="Search tasks"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
        />
      </label>
      <div className="sidebar-section-title">
        Projects
        <button
          type="button"
          className="icon-btn"
          aria-label="Open project"
          title="Open project (Ctrl+P)"
          onClick={() => onProject()}
        >
          <Plus size={15} />
        </button>
      </div>
      <div className="project-list">
        {searchError && <p className="hint">{searchError}</p>}
        {pins.some((id) => rows.some((s) => s.id === id)) && (
          <div className="project-group">
            <div className="pinned-title">
              <Pin size={12} /> Pinned
            </div>
            {rows.filter((s) => pins.includes(s.id)).map(task)}
          </div>
        )}
        {groups.map((path) => {
          const tasks = rows.filter(
            (s) => projectOf(s) === path && !pins.includes(s.id),
          );
          if (query && !tasks.length) return null;
          const shut = collapsed.includes(path) && !query;
          return (
            <div className="project-group" key={path}>
              <div className="project-heading">
                <button
                  type="button"
                  title={path}
                  aria-expanded={!shut}
                  onClick={() =>
                    setCollapsed(
                      shut
                        ? collapsed.filter((p) => p !== path)
                        : [...collapsed, path],
                    )
                  }
                >
                  <ChevronDown size={13} className={shut ? "rotated" : ""} />
                  <FolderOpen size={15} />
                  <span>{path.split("/").pop() || path}</span>
                </button>
                <button
                  type="button"
                  className="project-open icon-btn"
                  aria-label={`Open ${path}`}
                  onClick={() => onProject(path)}
                >
                  <ArrowUpRight size={13} />
                </button>
              </div>
              {!shut &&
                (tasks.length ? (
                  tasks.map(task)
                ) : (
                  <button
                    type="button"
                    className="project-empty"
                    onClick={() => onProject(path)}
                  >
                    Start a task here <Plus size={12} />
                  </button>
                ))}
            </div>
          );
        })}
        {query && rows.length === 0 && (
          <div className="sidebar-empty">No tasks match “{query}”.</div>
        )}
        {!groups.length && (
          <div className="sidebar-empty">Open a project to get started.</div>
        )}
      </div>
      {menuSession && onAction && (
        <ConversationMenu
          x={menu.x}
          y={menu.y}
          title={menuSession.title || "New task"}
          pinned={pins.includes(menuSession.id)}
          onClose={() => setMenu(null)}
          onAction={(action) => {
            setMenu(null);
            if (action === "pin") pin(menuSession.id);
            else if (action === "rename") setRenaming(menuSession.id);
            else onAction(action, menuSession);
          }}
        />
      )}
      <div className="sidebar-bottom">
        <button type="button" onClick={onSettings}>
          <Settings2 size={16} />
          <span>Settings</span>
          <kbd>Ctrl ,</kbd>
        </button>
      </div>
    </aside>
  );
}
