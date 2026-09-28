import {
  ChevronRight,
  FolderOpen,
  GitCompareArrows,
  GitPullRequest,
  PanelLeft,
  Search,
} from "lucide-react";

/** Project crumb, conversation title and the Comparisons, Changes and
 * command palette buttons. */
export function TopBar({
  sidebar,
  onShowSidebar,
  projectPath,
  onProject,
  title,
  comparing,
  onComparisons,
  changesOpen,
  onChanges,
  changeCount,
  onPalette,
}: {
  sidebar: boolean;
  onShowSidebar: () => void;
  projectPath: string;
  onProject: () => void;
  title: string;
  comparing: boolean;
  onComparisons: () => void;
  changesOpen: boolean;
  onChanges: () => void;
  changeCount: number;
  onPalette: () => void;
}) {
  return (
    <header className="top">
      {!sidebar && (
        <button
          type="button"
          className="icon-btn"
          aria-label="Show sidebar"
          title="Show sidebar (Ctrl+B)"
          onClick={onShowSidebar}
        >
          <PanelLeft size={18} aria-hidden="true" />
        </button>
      )}
      <button
        type="button"
        className="project-crumb"
        title={projectPath || "Open project"}
        onClick={onProject}
      >
        <FolderOpen size={15} aria-hidden="true" />
        <span>{projectPath.split("/").pop() || "Open project"}</span>
      </button>
      <ChevronRight size={13} className="dim" aria-hidden="true" />
      {/* The page's one level-one heading: the open task (or Comparisons). */}
      <h1 className="top-title" title={title}>
        {comparing ? "Comparisons" : title}
      </h1>
      <div className="top-right">
        <button
          type="button"
          className={`top-action ${comparing ? "on" : ""}`}
          aria-label="Comparisons"
          aria-pressed={comparing}
          title="Comparisons in this project"
          onClick={onComparisons}
        >
          <GitCompareArrows size={15} aria-hidden="true" />
          <span>Comparisons</span>
        </button>
        <button
          type="button"
          className={`top-action ${changesOpen ? "on" : ""}`}
          aria-label="Review changes"
          title="Changes (Ctrl+Shift+B)"
          onClick={onChanges}
        >
          <GitPullRequest size={15} aria-hidden="true" />
          <span>Changes</span>
          {changeCount > 0 && <span className="count">{changeCount}</span>}
        </button>
        <span className="top-divider" />
        <button
          type="button"
          className="icon-btn"
          title="Command palette (Ctrl+K)"
          aria-label="Command palette"
          onClick={onPalette}
        >
          <Search size={16} aria-hidden="true" />
        </button>
      </div>
    </header>
  );
}
