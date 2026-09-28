import type { ReactNode } from "react";
import { CircleArrowUp, GitBranch } from "lucide-react";
import type { AllowanceResponse } from "../../api";
import { AllowanceButton } from "../Allowance";
import "../../about.css";

/** The bottom line: state, branch, allowance, context use, an update notice
 * when a newer release exists, and the version. */
export function StatusBar({
  busy,
  paused,
  reconnecting,
  branch,
  onBranch,
  allowance,
  allowanceOpen,
  onAllowance,
  context,
  update = "",
  onUpdate,
  version,
}: {
  busy: boolean;
  paused: boolean;
  reconnecting: boolean;
  branch: string;
  onBranch: () => void;
  allowance: AllowanceResponse | null;
  allowanceOpen: boolean;
  onAllowance: () => void;
  /** The context & cost chip. */
  context?: ReactNode;
  /** A newer version to announce (opens Settings › About), or "". */
  update?: string;
  onUpdate?: () => void;
  version: string;
}) {
  return (
    <footer className="statusline" aria-live="polite">
      <span className={`status-dot ${busy ? "active" : ""}`} />
      <span className="status-label">
        {busy
          ? paused
            ? "Paused"
            : "Working"
          : reconnecting
            ? "Reconnecting"
            : "Ready"}
      </span>
      {branch && (
        <>
          <span className="sep" aria-hidden="true">
            ·
          </span>
          <button type="button" title="Review git changes" onClick={onBranch}>
            <GitBranch size={12} aria-hidden="true" />
            {branch}
          </button>
        </>
      )}
      <span className="sep" aria-hidden="true">
        ·
      </span>
      <AllowanceButton
        data={allowance}
        open={allowanceOpen}
        onOpen={onAllowance}
      />
      <span className="grow" />
      {context}
      {update && onUpdate && (
        <button
          type="button"
          className="update-notice"
          title="See what's new and how to update"
          onClick={onUpdate}
        >
          <CircleArrowUp size={12} aria-hidden="true" />
          Update available: {update}
        </button>
      )}
      <span className="version">{version ? `v${version}` : "Connecting"}</span>
    </footer>
  );
}
