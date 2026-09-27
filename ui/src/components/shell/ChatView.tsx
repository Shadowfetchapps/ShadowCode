import type { ReactNode, RefObject, UIEvent } from "react";
import { LoaderCircle, X } from "lucide-react";
import type { Approval, CommandResult, Job } from "../../api";
import { ActivityTimeline } from "../ActivityTimeline";
import { CommandCardView } from "../cards";
import { ApprovalCard, type ApprovalDecision } from "../ApprovalCard";
import { Elapsed } from "../Elapsed";
import { TaskSteerBar } from "../TaskSteerBar";
import { WelcomeBanner } from "../WelcomeBanner";
import type { TaskActivity } from "../../lib/activity";
import { isProjectTrustError, trustErrorHint } from "../../lib/trust";
import type { ToastKind } from "../../hooks/useToasts";

export type HistoryState = {
  enabled: boolean;
  viewing: boolean;
  hasOlder: boolean;
  loading: boolean;
  error: string;
};

/** Saved-history paging above the conversation. */
function HistoryNav({
  history,
  onOlder,
  onNewer,
  onLatest,
}: {
  history: HistoryState;
  onOlder: () => void;
  onNewer: () => void;
  onLatest: () => void;
}) {
  return (
    <nav className="history-navigation" aria-label="Conversation history">
      <div className="row">
        <button
          type="button"
          className="ghost"
          disabled={history.loading || !history.hasOlder}
          onClick={onOlder}
        >
          Older messages
        </button>
        {history.viewing && (
          <>
            <button
              type="button"
              className="ghost"
              disabled={history.loading}
              onClick={onNewer}
            >
              Newer messages
            </button>
            <button type="button" className="ghost" onClick={onLatest}>
              Latest messages
            </button>
          </>
        )}
      </div>
      {history.loading && <p role="status">Loading saved messages…</p>}
      {history.viewing && (
        <p className="hint">
          Browsing saved history. Current work continues. Pages may begin
          partway through a task.
        </p>
      )}
      {history.error && (
        <p role="alert" className="error">
          {history.error}
        </p>
      )}
    </nav>
  );
}

/** The scrolling conversation: notices, history paging, the transcript
 * (`rows`), pending approvals and the live "working" timeline. */
export function ChatView({
  hidden,
  streamRef,
  onScroll,
  empty,
  switching,
  shutdown,
  onRetryQuit,
  error,
  canTrust,
  onTrust,
  onReconnect,
  onDismissError,
  history,
  onOlder,
  onNewer,
  onLatest,
  onSuggestion,
  needsModel,
  onChooseModel,
  rows,
  commandCards,
  approvals,
  onDecide,
  working,
  activity,
  job,
  busy,
  onToast,
}: {
  hidden: boolean;
  streamRef: RefObject<HTMLDivElement | null>;
  onScroll: (event: UIEvent<HTMLDivElement>) => void;
  empty: boolean;
  switching: boolean;
  shutdown: { status: string; message?: string } | null;
  onRetryQuit: () => void;
  error: string;
  /** The error is a trust refusal and a project is open. */
  canTrust: boolean;
  onTrust: () => void;
  onReconnect: () => void;
  onDismissError: () => void;
  history: HistoryState;
  onOlder: () => void;
  onNewer: () => void;
  onLatest: () => void;
  onSuggestion: (prompt: string) => void;
  needsModel: boolean;
  onChooseModel: () => void;
  rows: ReactNode;
  commandCards: CommandResult[];
  approvals: Approval[];
  onDecide: (id: string, answer: ApprovalDecision) => void;
  /** A task is running or being submitted. */
  working: boolean;
  /** The running task's activity. */
  activity: TaskActivity | undefined;
  job: Job | null;
  busy: boolean;
  onToast: (text: string, kind?: ToastKind) => void;
}) {
  const since = job?.started_at;
  return (
    <div
      className="chat-stream"
      hidden={hidden}
      ref={streamRef}
      onScroll={onScroll}
    >
      <div className={`chat-inner ${empty ? "is-empty" : ""}`}>
        {shutdown && (
          <div className="notice" role="status">
            <span>
              {shutdown.message ||
                "Stopping active work and saving the session before closing…"}
            </span>
            {shutdown.status === "error" && (
              <button type="button" className="mini" onClick={onRetryQuit}>
                Retry closing
              </button>
            )}
          </div>
        )}
        {error && (
          <div className="notice bad" role="alert">
            <span>
              {error}
              {trustErrorHint(error) ? ` ${trustErrorHint(error)}` : ""}
            </span>
            {isProjectTrustError(error) && canTrust && (
              <button type="button" className="mini" onClick={onTrust}>
                Trust this folder
              </button>
            )}
            <button type="button" className="mini" onClick={onReconnect}>
              Reconnect
            </button>
            <button
              type="button"
              className="icon-btn"
              aria-label="Dismiss error"
              onClick={onDismissError}
            >
              <X size={14} aria-hidden="true" />
            </button>
          </div>
        )}
        {!switching &&
          history.enabled &&
          (history.hasOlder || history.viewing || history.error) && (
            <HistoryNav
              history={history}
              onOlder={onOlder}
              onNewer={onNewer}
              onLatest={onLatest}
            />
          )}
        {switching ? (
          <div className="loading-task">
            <LoaderCircle size={20} className="spin" aria-hidden="true" />
            Opening task…
          </div>
        ) : empty && !history.viewing ? (
          <WelcomeBanner
            onSelect={onSuggestion}
            needsModel={needsModel}
            onChooseModel={onChooseModel}
          />
        ) : (
          <>
            {rows}
            {commandCards.map((card, i) => (
              <CommandCardView key={`c${i}`} card={card} />
            ))}
          </>
        )}
        {approvals.map((a) => (
          <ApprovalCard key={a.id} approval={a} onDecide={onDecide} />
        ))}
        {working && !switching && (
          <div className="working" role="status" aria-live="polite">
            <ActivityTimeline
              activity={activity}
              jobStatus={job?.status}
              localQueued={job?.routing?.model_id?.startsWith("local:gguf:")}
              pendingApprovals={approvals.length}
              elapsed={busy && since ? <Elapsed since={since} /> : undefined}
            />
            {job && (job.status === "running" || job.status === "paused") && (
              <TaskSteerBar
                job={job}
                onToast={(text, kind) => onToast(text, kind || "info")}
              />
            )}
          </div>
        )}
      </div>
    </div>
  );
}
