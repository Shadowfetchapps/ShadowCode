import { Dialog } from "./Dialog";
import { useEffect, useState } from "react";
import type { Project } from "../api";
import { canPickFiles, pickDirectory } from "../lib/transport";
import { GLOSSARY, GLOSSARY_WORDS } from "../lib/glossary";

const SHORTCUT_GROUPS: { label: string; rows: [string, string][] }[] = [
  {
    label: "Task control",
    rows: [
      ["Enter", "Send task"],
      ["Shift+Enter", "New line in composer"],
      ["Ctrl+Shift+Enter", "Run in a new worktree (beside other work)"],
      ["Ctrl+.", "Stop the agent"],
      ["/", "Slash commands (in composer)"],
      ["Ctrl+Shift+Space", "Dictate: hold to talk, or tap to start and stop"],
    ],
  },
  {
    label: "Navigation",
    rows: [
      ["Ctrl+K", "Command palette"],
      ["Ctrl+B", "Toggle sidebar"],
      ["Ctrl+Shift+B", "Toggle the Changes drawer"],
      ["Ctrl+`", "Toggle the Terminal (works inside it too)"],
      ["Ctrl+P", "Open project…"],
      ["Ctrl+N", "New task"],
      ["Alt+↑ / Alt+↓", "Previous / next conversation"],
      ["Ctrl+Tab", "Back to the last conversation"],
      ["Ctrl+L", "Focus composer"],
      ["Ctrl+M", "Choose a model"],
      ["Esc", "Close overlay or drawer"],
    ],
  },
  {
    label: "Other",
    rows: [
      ["Ctrl+,", "Settings"],
      ["Ctrl+Shift+E", "Export this task as Markdown"],
      ["?", "This cheat sheet"],
    ],
  },
];

const EXAMPLE_PROMPTS: string[] = [
  "Build a REST endpoint that does X",
  "Find and fix the bug where Y happens",
  "Write unit tests for the auth module",
  "Explain how the database layer works",
  "Refactor this function to be cleaner",
  "Review the git diff for issues",
];

export function Help({
  onClose,
  version,
}: {
  onClose: () => void;
  version: string;
}) {
  const [find, setFind] = useState("");
  const words = GLOSSARY_WORDS.filter((word) =>
    `${word} ${GLOSSARY[word]}`.toLowerCase().includes(find.toLowerCase()),
  );
  return (
    <Dialog
      label="Help & Shortcuts"
      className="modal modal-sm help-modal"
      onClose={onClose}
    >
      <h2>Keyboard shortcuts</h2>
      <div className="help-groups">
        {SHORTCUT_GROUPS.map((group) => (
          <div className="help-group" key={group.label}>
            <div className="help-group-label">{group.label}</div>
            <div className="help-grid">
              {group.rows.map(([k, label]) => (
                <div className="help-row" key={k}>
                  <span>{label}</span>
                  <span className="kbd">{k}</span>
                </div>
              ))}
            </div>
          </div>
        ))}
      </div>
      <h2 className="help-section-title">What can I ask?</h2>
      {/* Examples to type, not buttons: a plain list, so nothing looks
          clickable that is not. */}
      <ul className="help-examples">
        {EXAMPLE_PROMPTS.map((p) => (
          <li className="help-example" key={p}>
            “{p}”
          </li>
        ))}
      </ul>
      <h2 className="help-section-title">Words you&rsquo;ll see</h2>
      <input
        className="help-glossary-find"
        type="search"
        value={find}
        onChange={(e) => setFind(e.target.value)}
        aria-label="Find a word"
        placeholder="Find a word…"
      />
      <dl className="help-glossary">
        {words.map((word) => (
          <div key={word}>
            <dt>{word[0].toUpperCase() + word.slice(1)}</dt>
            <dd>{GLOSSARY[word]}</dd>
          </div>
        ))}
      </dl>
      {!words.length && <p className="hint">No word matches.</p>}
      <p className="hint">ShadowCode {version}</p>
      <div className="row end">
        <button type="button" className="ghost" onClick={onClose}>
          Close
        </button>
      </div>
    </Dialog>
  );
}

export type PaletteItem = {
  id: string;
  label: string;
  hint?: string;
  run: () => void;
};

export function Palette({
  items,
  onClose,
}: {
  items: PaletteItem[];
  onClose: () => void;
}) {
  const [q, setQ] = useState("");
  const [index, setIndex] = useState(0);
  const hits = items.filter((c) =>
    c.label.toLowerCase().includes(q.toLowerCase()),
  );
  useEffect(() => setIndex(0), [q]);
  return (
    <Dialog label="Command palette" className="palette" onClose={onClose}>
      <input
        autoFocus
        value={q}
        onChange={(e) => setQ(e.target.value)}
        aria-label="Search commands"
        placeholder="Type a command…"
        onKeyDown={(ev) => {
          if (ev.key === "ArrowDown") {
            ev.preventDefault();
            setIndex((i) => Math.min(i + 1, hits.length - 1));
          }
          if (ev.key === "ArrowUp") {
            ev.preventDefault();
            setIndex((i) => Math.max(i - 1, 0));
          }
          if (ev.key === "Enter" && hits[index]) {
            ev.preventDefault();
            onClose();
            hits[index].run();
          }
        }}
      />
      <div className="palette-list">
        {hits.map((item, i) => (
          <button
            type="button"
            key={item.id}
            className={`hit ${i === index ? "on" : ""}`}
            onMouseEnter={() => setIndex(i)}
            onClick={() => {
              onClose();
              item.run();
            }}
          >
            <span>{item.label}</span>
            {item.hint && <span className="kbd">{item.hint}</span>}
          </button>
        ))}
        {hits.length === 0 && (
          <div className="palette-empty">No matching commands.</div>
        )}
      </div>
    </Dialog>
  );
}

export function ProjectPicker({
  projects,
  current,
  onClose,
  onPick,
}: {
  projects: Project[];
  current: string;
  onClose: () => void;
  onPick: (path: string) => void;
}) {
  const [path, setPath] = useState(current);
  const [error, setError] = useState("");
  return (
    <Dialog label="Open project" className="modal modal-sm" onClose={onClose}>
      <h2>Open a project</h2>
      <div className="field">
        <label htmlFor="overlays-field-1">Folder path</label>
        <div className="input-action">
          <input
            id="overlays-field-1"
            autoFocus
            value={path}
            onChange={(e) => setPath(e.target.value)}
            placeholder="/path/to/project"
            onKeyDown={(ev) => {
              if (ev.key === "Enter") onPick(path);
            }}
          />
          {canPickFiles() && (
            <button
              type="button"
              className="ghost"
              onClick={() =>
                void pickDirectory()
                  .then((path) => {
                    if (path) setPath(path);
                  })
                  .catch((error) => setError(String(error)))
              }
            >
              Browse…
            </button>
          )}
        </div>
        {error && (
          <p role="alert" className="hint danger-text">
            {error}
          </p>
        )}
      </div>
      {projects.length > 0 && (
        <div className="list">
          <div className="list-h">Recent</div>
          {projects.map((p) => (
            <button
              type="button"
              key={p.id}
              className={`item ${p.path === current ? "active" : ""}`}
              onClick={() => onPick(p.path)}
            >
              <strong>{p.name}</strong>
              <span>{p.path}</span>
            </button>
          ))}
        </div>
      )}
      <div className="row end">
        <button type="button" className="ghost" onClick={onClose}>
          Cancel
        </button>
        <button type="button" className="primary" onClick={() => onPick(path)}>
          Open
        </button>
      </div>
    </Dialog>
  );
}

export function TrustDialog({
  req,
  onCancel,
  onConfirm,
}: {
  req: { path: string; name?: string; permissions?: Record<string, unknown> };
  onCancel: () => void;
  onConfirm: () => void;
}) {
  return (
    <Dialog label="Trust folder" className="modal modal-sm" onClose={onCancel}>
      <h2>Trust this folder?</h2>
      <p className="hint">
        The agent may read <code>{req.path}</code> and change files inside it.{" "}
        {req.permissions?.level === "read_only"
          ? "This project is read only: nothing is changed."
          : req.permissions?.mode === "allow_edits"
            ? "File edits run without asking; commands wait for your approval."
            : "File edits and commands wait for your approval."}
      </p>
      <div className="row end">
        <button type="button" className="ghost" onClick={onCancel}>
          Cancel
        </button>
        <button type="button" className="primary" onClick={onConfirm}>
          Trust and open
        </button>
      </div>
    </Dialog>
  );
}
