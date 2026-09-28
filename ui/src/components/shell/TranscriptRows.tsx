import { api } from "../../api";
import {
  memo,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
  type RefObject,
} from "react";
import { ActivityTimeline } from "../ActivityTimeline";
import { CommandCardView, OpCard, type ChatItem } from "../cards";
import { LimitFallbackItem } from "../LimitFallback";
import { Markdown } from "../Markdown";
import { CopyButton, UserMessage } from "../MessageActions";
import { SubagentCard } from "../SubagentCard";
import { TaskSummary, type DiffStat } from "../TaskSummary";
import type { RunCheckAction } from "../RunCheck";
import type { TaskActivity } from "../../lib/activity";
import type { Fallback } from "../../lib/allowance";

/** Rows rendered at first; "Show earlier" adds this many more. */
export const ROW_WINDOW = 150;

type LimitItem = Extract<ChatItem, { kind: "limit" }>;

/** Row callbacks. The app passes stable functions, so a memoized row only
 * re-renders when its own item or task activity changes. */
export type RowActions = {
  runCheck?: RunCheckAction;
  onToggleTool: (key: string) => void;
  diffStats: (paths: string[]) => Promise<Record<string, DiffStat>>;
  /** Review a task's changes (the Review view), or with no task the
   * working tree (the Changes drawer). */
  onReview: (path?: string, taskId?: string) => void;
  onRewind: (taskId: string) => void;
  onContinue: (item: LimitItem, choice: Fallback) => void;
  onChooseModel: () => void;
  onOpenLocal: () => void;
  onFork: (eventId: number) => void;
  onEditResend: (
    item: Extract<ChatItem, { kind: "user" }>,
    text: string,
    undoFiles: boolean,
  ) => void;
  onRetry: (text: string) => void;
  onCopy: (text: string) => void;
  /** Open a subagent's own conversation (hidden from the sidebar). */
  onOpenSession?: (sessionId: string) => void;
};

type Row = {
  item: ChatItem;
  key: string;
  /** The task's timeline goes above this answer (a finished task reads
   * timeline, answer, summary). */
  timelineHere: boolean;
  /** The summary's timeline was already shown above the answer. */
  timelineAbove: boolean;
  /** A task that stopped without a completion record still shows what it
   * did, after its last row. */
  stranded: boolean;
  /** A queued follow-up's prompt shows in the queue, not the transcript. */
  queued: boolean;
};

/** Which rows render and how, from the whole transcript. */
export function transcriptRows(
  items: ChatItem[],
  activity: Record<string, TaskActivity>,
  activeTaskId: string,
  liveTaskId: string | undefined,
  queuedTaskIds: ReadonlySet<string>,
): Row[] {
  const last: Record<string, number> = {};
  const summarized = new Set<string>();
  items.forEach((item, index) => {
    if (item.taskId) last[item.taskId] = index;
    if (item.kind === "summary") summarized.add(item.taskId);
  });
  const timelineAt: Record<string, number> = {};
  items.forEach((item, index) => {
    if (
      item.kind === "agent" &&
      item.taskId &&
      summarized.has(item.taskId) &&
      !(item.taskId in timelineAt)
    )
      timelineAt[item.taskId] = index;
  });
  const rows: Row[] = [];
  items.forEach((item, index) => {
    const task = item.taskId ? activity[item.taskId] : undefined;
    const queued =
      item.kind === "user" &&
      liveTaskId !== item.taskId &&
      Boolean(item.taskId && queuedTaskIds.has(item.taskId));
    const stranded = Boolean(
      item.taskId &&
      last[item.taskId] === index &&
      item.taskId !== activeTaskId &&
      task &&
      !task.finished &&
      task.calls.length > 0,
    );
    const timelineHere = Boolean(
      item.taskId && timelineAt[item.taskId] === index,
    );
    const renders =
      !queued &&
      (item.kind === "tool"
        ? item.tool === "hook"
        : item.kind === "summary"
          ? Boolean(task)
          : true);
    if (!renders && !stranded && !timelineHere) return;
    rows.push({
      item,
      key: item.key || `i:${index}`,
      timelineHere,
      timelineAbove: Boolean(item.taskId && item.taskId in timelineAt),
      stranded,
      queued,
    });
  });
  return rows;
}

/** One row. Props are the item (kept by reference while unchanged), its
 * task's activity and plain flags, so memo skips rows an event did not touch. */
const TranscriptRow = memo(function TranscriptRow({
  item,
  rowKey,
  timelineHere,
  timelineAbove,
  stranded,
  queued,
  activity,
  fallback,
  locked,
  forkDisabled,
  actions,
}: Omit<Row, "key"> & {
  rowKey: string;
  activity: TaskActivity | undefined;
  fallback: Fallback | null;
  locked: boolean;
  forkDisabled: boolean;
  actions: RowActions;
}) {
  let node: ReactNode = null;
  if (queued) node = null;
  else if (item.kind === "tool")
    // Tool calls live in the activity timeline; lifecycle hooks have no tool
    // event and stay as cards.
    node =
      item.tool === "hook" ? (
        <OpCard item={item} onToggle={() => actions.onToggleTool(rowKey)} />
      ) : null;
  else if (item.kind === "command") node = <CommandCardView card={item.card} />;
  else if (item.kind === "note")
    node = (
      <div className={`msg-note ${item.warning ? "warning" : ""}`}>
        {item.text}
      </div>
    );
  else if (item.kind === "subagent")
    node = <SubagentCard run={item.run} onOpen={actions.onOpenSession} />;
  else if (item.kind === "divider")
    node = (
      <div
        className={`msg-divider${item.rewound ? " is-rewound" : ""}`}
        role="separator"
        aria-label={item.text}
      >
        <span>{item.text}</span>
      </div>
    );
  else if (item.kind === "summary")
    node = activity ? (
      <div className="msg-summary">
        {!timelineAbove && <ActivityTimeline activity={activity} withSummary />}
        <TaskSummary
          activity={activity}
          workspace={actions.runCheck?.workspace}
          sessionId={actions.runCheck?.sessionId}
          runCheck={actions.runCheck}
          readVerification={api.jobVerification}
          diffStats={actions.diffStats}
          onReview={(path) => actions.onReview(path, item.taskId)}
          onRewind={
            // A subscription turn is rewindable once its project
            // checkpoint recorded the files it changed.
            activity.verification?.status === "vendor_owned" &&
            !activity.checkpointed
              ? undefined
              : () => actions.onRewind(item.taskId)
          }
        />
      </div>
    ) : null;
  else if (item.kind === "limit")
    node = (
      <LimitFallbackItem
        item={item}
        fallback={fallback}
        disabled={locked}
        onContinue={(choice) => actions.onContinue(item, choice)}
        onChoose={actions.onChooseModel}
        onOpenLocal={actions.onOpenLocal}
      />
    );
  else if (item.kind === "user")
    node = (
      <UserMessage
        item={item}
        disabled={forkDisabled}
        onEditResend={actions.onEditResend}
        onRetry={actions.onRetry}
        onCopy={actions.onCopy}
      />
    );
  else
    node = (
      <div className="msg-agent">
        {item.who && <div className="who">{item.who}</div>}
        <Markdown>{item.text}</Markdown>
        {!item.live && (
          <div
            className="agent-actions"
            role="group"
            aria-label="Answer actions"
          >
            <CopyButton
              text={item.text}
              onCopy={actions.onCopy}
              label="Copy answer"
            />
            {item.eventId && (
              <button
                type="button"
                className="ghost fork-action"
                disabled={forkDisabled}
                onClick={() => actions.onFork(item.eventId!)}
              >
                Fork from here
              </button>
            )}
          </div>
        )}
      </div>
    );
  if (stranded)
    return (
      <div>
        {node}
        <ActivityTimeline activity={activity} />
      </div>
    );
  if (timelineHere)
    return (
      <div className="msg-with-activity">
        <ActivityTimeline activity={activity} withSummary />
        {node}
      </div>
    );
  return <>{node}</>;
});

/** The conversation's rows with stable keys, memoized per row. Long
 * transcripts render their latest `ROW_WINDOW` rows first; earlier rows
 * appear on request without moving what is on screen. */
export function TranscriptRows({
  items,
  activity,
  activeTaskId,
  liveTaskId,
  queuedTaskIds,
  fallback,
  locked,
  forkDisabled,
  actions,
  scrollRef,
  resetKey,
}: {
  items: ChatItem[];
  activity: Record<string, TaskActivity>;
  activeTaskId: string;
  liveTaskId?: string;
  queuedTaskIds: ReadonlySet<string>;
  fallback: Fallback | null;
  locked: boolean;
  forkDisabled: boolean;
  actions: RowActions;
  scrollRef: RefObject<HTMLDivElement | null>;
  /** Changes when another conversation or history page opens. */
  resetKey: string;
}) {
  const rows = useMemo(
    () =>
      transcriptRows(items, activity, activeTaskId, liveTaskId, queuedTaskIds),
    [items, activity, activeTaskId, liveTaskId, queuedTaskIds],
  );
  const [limit, setLimit] = useState({ key: resetKey, rows: ROW_WINDOW });
  if (limit.key !== resetKey) setLimit({ key: resetKey, rows: ROW_WINDOW });
  const shown = limit.key === resetKey ? limit.rows : ROW_WINDOW;
  const anchor = useRef<number | null>(null);
  useLayoutEffect(() => {
    const el = scrollRef.current;
    if (anchor.current === null || !el) return;
    el.scrollTop += el.scrollHeight - anchor.current;
    anchor.current = null;
  }, [shown, scrollRef]);
  const start = Math.max(0, rows.length - shown);
  return (
    <>
      {start > 0 && (
        <div className="history-navigation">
          <div className="row">
            <button
              type="button"
              className="ghost"
              onClick={() => {
                anchor.current = scrollRef.current?.scrollHeight ?? null;
                setLimit({ key: resetKey, rows: shown + ROW_WINDOW });
              }}
            >
              Show {Math.min(start, ROW_WINDOW)} earlier items
            </button>
          </div>
        </div>
      )}
      {rows.slice(start).map((row) => (
        <TranscriptRow
          key={row.key}
          rowKey={row.key}
          item={row.item}
          timelineHere={row.timelineHere}
          timelineAbove={row.timelineAbove}
          stranded={row.stranded}
          queued={row.queued}
          activity={
            // Only rows that draw a timeline or summary follow the activity.
            row.item.taskId &&
            (row.item.kind === "summary" || row.stranded || row.timelineHere)
              ? activity[row.item.taskId]
              : undefined
          }
          fallback={row.item.kind === "limit" ? fallback : null}
          locked={row.item.kind === "limit" ? locked : false}
          forkDisabled={
            row.item.kind === "agent" || row.item.kind === "user"
              ? forkDisabled
              : false
          }
          actions={actions}
        />
      ))}
    </>
  );
}
