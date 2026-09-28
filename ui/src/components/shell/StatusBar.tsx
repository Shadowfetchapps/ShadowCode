import type { ReactNode } from "react";
import { GitBranch } from "lucide-react";
import type { AllowanceResponse } from "../../api";
import { AllowanceButton } from "../Allowance";

/** The bottom line: state, branch, allowance, context use and version. */
export function StatusBar({
  busy,
  paused,
  reconnecting,
  connected = true,
  branch,
  onBranch,
  allowance,
  allowanceOpen,
  onAllowance,
  context,
  version,
}: {
  busy: boolean;
  paused: boolean;
  reconnecting: boolean;
  /** The engine answered at startup. Without it nothing is "Ready". */
  connected?: boolean;
  branch: string;
  onBranch: () => void;
  allowance: AllowanceResponse | null;
  allowanceOpen: boolean;
  onAllowance: () => void;
  /** The context & cost chip. */
  context?: ReactNode;
  version: string;
}) {
  return (
    <footer className="statusline" aria-live="polite">
      <span
        className={`status-dot ${busy ? "active" : !connected ? "offline" : ""}`}
      />
      <span className="status-label">
        {busy
          ? paused
            ? "Paused"
            : "Working"
          : reconnecting
            ? "Reconnecting"
            : connected
              ? "Ready"
              : "Not connected"}
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
      {version && <span className="version">v{version}</span>}
    </footer>
  );
}
