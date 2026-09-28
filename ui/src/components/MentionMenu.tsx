import { useEffect, useState } from "react";
import { FileCode2, Folder, Bot } from "lucide-react";
import { api, type AgentInfo } from "../api";
import { fitsMentionQuery, type Mention } from "../lib/mentions";

/** One row of the @ menu: a subagent (W1-F's `@agent` at the start of a
 * message) or a project file or folder to attach. */
export type MentionOption =
  { kind: "agent"; agent: AgentInfo } | { kind: "file" | "dir"; path: string };

export const optionKey = (o: MentionOption) =>
  o.kind === "agent" ? `agent:${o.agent.name}` : `${o.kind}:${o.path}`;

/** Files and folders matching `query` (null: the menu is closed). Answers
 * arrive after a short pause in typing; errors leave the list empty. */
export function useFileMentions(query: string | null): {
  items: Mention[];
  loading: boolean;
} {
  const [found, setFound] = useState<{
    query: string | null;
    items: Mention[];
  }>({ query: null, items: [] });
  useEffect(() => {
    if (query === null) return;
    let live = true;
    const timer = setTimeout(() => {
      api
        .mentions(query, 12)
        .then((result) => {
          if (live) setFound({ query, items: result.items });
        })
        .catch(() => {
          if (live) setFound({ query, items: [] });
        });
    }, 120);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  }, [query]);
  if (query === null) return { items: [], loading: false };
  const loading = found.query !== query;
  // While the answer for the latest text is on its way, show only earlier
  // results that still fit it: Enter must never pick a file the text typed
  // since rules out (typing "@app" fast used to pick "README.md").
  return {
    items: loading
      ? found.items.filter((item) => fitsMentionQuery(item.path, query))
      : found.items,
    loading,
  };
}

/** The @ menu: subagents first (only for `@name` at the start of a
 * message), then files and folders. The caller owns keyboard selection. */
export function MentionMenu({
  options,
  index,
  onPick,
  loading,
}: {
  options: MentionOption[];
  index: number;
  onPick: (option: MentionOption) => void;
  loading: boolean;
}) {
  const agents = options.filter((o) => o.kind === "agent");
  const files = options.filter((o) => o.kind !== "agent");
  const row = (option: MentionOption) => {
    const i = options.indexOf(option);
    const on = i === index;
    return (
      <div
        role="option"
        id={`mention-${i}`}
        aria-selected={on}
        className={`slash-hit mention-hit ${on ? "on" : ""}`}
        key={optionKey(option)}
        onMouseDown={(e) => e.preventDefault()}
        onClick={() => onPick(option)}
      >
        {option.kind === "agent" ? (
          <>
            <Bot size={13} aria-hidden="true" />
            <strong>@{option.agent.name}</strong>
            <span>
              {option.agent.mode === "write" ? "Worktree · " : "Read-only · "}
              {option.agent.description}
            </span>
          </>
        ) : (
          <>
            {option.kind === "dir" ? (
              <Folder size={13} aria-hidden="true" />
            ) : (
              <FileCode2 size={13} aria-hidden="true" />
            )}
            <strong>
              {option.path}
              {option.kind === "dir" ? "/" : ""}
            </strong>
          </>
        )}
      </div>
    );
  };
  return (
    <div
      className="slash-menu mention-menu"
      role="listbox"
      aria-label="Mentions"
    >
      {agents.length > 0 && (
        <div role="group" aria-label="Subagents">
          <div className="mention-group" aria-hidden="true">
            Subagents
          </div>
          {agents.map(row)}
        </div>
      )}
      {files.length > 0 && (
        <div role="group" aria-label="Files and folders">
          <div className="mention-group" aria-hidden="true">
            Files and folders
          </div>
          {files.map(row)}
        </div>
      )}
      {!options.length && (
        <div className="slash-empty">
          {loading ? "Searching…" : "No matching files or folders"}
        </div>
      )}
    </div>
  );
}
