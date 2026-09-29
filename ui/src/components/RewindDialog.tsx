import { useState } from "react";
import type { RewindKept } from "../api";
import { ConfirmDialog } from "./ConfirmDialog";

/** Confirms a rewind and names every file it changes. Files you saved in the
 * editor while a subscription turn ran are listed as kept; the ones the agent
 * edited too can be rewound as well on request. Changes the agent did not
 * report are marked. */
export function RewindDialog({
  paths,
  kept = [],
  unreported = [],
  onConfirm,
  onCancel,
}: {
  paths: string[];
  kept?: RewindKept[];
  unreported?: string[];
  onConfirm: (includeUserEdits: boolean) => void | Promise<void>;
  onCancel: () => void;
}) {
  const [includeShared, setIncludeShared] = useState(false);
  const shared = kept.filter(
    (file) => file.reason === "edited_by_you_and_agent",
  );
  const count = paths.length + (includeShared ? shared.length : 0);
  const shown = paths.slice(0, 12);
  const notReported = new Set(unreported);
  return (
    <ConfirmDialog
      title="Rewind this task's changes?"
      confirmLabel={`Rewind ${count} file${count === 1 ? "" : "s"}`}
      danger
      onConfirm={() => onConfirm(includeShared)}
      onCancel={onCancel}
    >
      <p>
        These files go back to how they were before the task. Files it created
        are removed. You can undo the rewind right after.
      </p>
      {paths.length ? (
        <ul className="rewind-files" aria-label="Files that will change">
          {shown.map((path) => (
            <li key={path}>
              <code>{path}</code>
              {notReported.has(path) && (
                <span className="dim">
                  {" "}
                  · not reported by the agent (a command it ran or another
                  program changed it)
                </span>
              )}
            </li>
          ))}
          {paths.length > shown.length && (
            <li className="dim">and {paths.length - shown.length} more</li>
          )}
        </ul>
      ) : null}
      {kept.length ? (
        <>
          <p>
            You saved these in the editor during the turn. They stay as they
            are:
          </p>
          <ul
            className="rewind-files"
            aria-label="Files that stay as you saved them"
          >
            {kept.map((file) => (
              <li key={file.path}>
                <code>{file.path}</code>
                <span className="dim">
                  {file.reason === "edited_by_you_and_agent"
                    ? " · the agent edited it too"
                    : " · only you changed it"}
                </span>
              </li>
            ))}
          </ul>
          {shared.length ? (
            <label className="check">
              <input
                type="checkbox"
                checked={includeShared}
                onChange={(event) => setIncludeShared(event.target.checked)}
              />{" "}
              Also rewind the{" "}
              {shared.length === 1 ? "file" : `${shared.length} files`} the
              agent edited too (this undoes your edits in{" "}
              {shared.length === 1 ? "it" : "them"})
            </label>
          ) : null}
        </>
      ) : null}
      <p className="hint">
        The conversation stays as it is; a divider marks the rewind.
      </p>
    </ConfirmDialog>
  );
}
