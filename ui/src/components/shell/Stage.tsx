import { lazy, Suspense, type ReactNode, type RefObject } from "react";
import { ArrowDown } from "lucide-react";
import type {
  Approval,
  CommandResult,
  DownloadModel,
  Health,
  Job,
  Session,
} from "../../api";
import type { ApprovalDecision } from "../ApprovalCard";
import type { DrawerTab } from "../Drawer";
import type { AdvancedTab, SettingsSection } from "../Settings";
import { ChatView } from "./ChatView";
import { ComposerDock, composerAccess } from "./ComposerDock";
import { StageBanners } from "./StageBanners";
import { StatusBar } from "./StatusBar";
import { TranscriptRows, type RowActions } from "./TranscriptRows";
import type { useAttachments } from "../../hooks/useAttachments";
import type { ComposerExtras } from "../../hooks/useComposerExtras";
import type { useAllowance } from "../../hooks/useCatalog";
import type { useCompare } from "../../hooks/useCompare";
import type { useConversation } from "../../hooks/useConversation";
import type { useJobControls } from "../../hooks/useJobControls";
import type { useNavigation } from "../../hooks/useNavigation";
import type { useStickyScroll } from "../../hooks/useStickyScroll";
import type { useTaskActions } from "../../hooks/useTaskActions";
import type { ToastKind } from "../../hooks/useToasts";
import type { WorkspaceStatus } from "../../hooks/useWorkspace";
import type { Fallback } from "../../lib/allowance";
import { isReady, type PickerTarget } from "../../lib/picker";
import { useModelDownloads } from "../../hooks/useModelDownloads";
import { ModelSetupPanel } from "../ModelSetup";

const CompareView = lazy(() =>
  import("../CompareView").then((module) => ({ default: module.CompareView })),
);
import { writeStore } from "../../lib/storage";
import { invoke, isNative, isRemote } from "../../lib/transport";
import { noticeVersion, useUpdateNotice } from "../../hooks/useUpdates";
import { trustRequestFor } from "../../lib/trust";
import { chipInput } from "../../lib/usageChip";
import { ContextChip } from "../ContextChip";
import { useSpendEstimate } from "../../hooks/useSpendEstimate";
import type { TryOn } from "../../hooks/useTaskActions";

/** The main column: banners, the Compare view or the conversation, the
 * composer and the status line. It only lays out what the app's hooks
 * provide. */
export function Stage({
  conversation,
  compare,
  scroll,
  controls,
  nav,
  files,
  actions,
  allowance,
  allowanceOpen,
  onAllowance,
  health,
  status,
  cfg,
  workspace,
  gitBranch,
  shutdown,
  sessions,
  targets,
  pickerLoaded,
  onRefreshModels,
  onModelDownloaded,
  pickerOpen,
  setPickerOpen,
  selectedTarget,
  queuedJobs,
  queuedTaskIds,
  fallback,
  rowActions,
  onDecide,
  commandCards,
  approvals,
  task,
  setTask,
  promptRef,
  commands,
  submitting,
  queueing,
  commandWaiting,
  webEnabled,
  setWebEnabled,
  openSettings,
  openSetup,
  setPanel,
  reviewChanges,
  refresh,
  setPermissionMode,
  toast,
  issueOffer,
  extras,
  reviewPanel,
  worktreeBar,
  tryOn,
  onTryOnModel,
}: {
  conversation: ReturnType<typeof useConversation>;
  compare: ReturnType<typeof useCompare>;
  scroll: ReturnType<typeof useStickyScroll>;
  controls: ReturnType<typeof useJobControls>;
  nav: ReturnType<typeof useNavigation>;
  files: ReturnType<typeof useAttachments>;
  actions: ReturnType<typeof useTaskActions>;
  allowance: ReturnType<typeof useAllowance>;
  allowanceOpen: boolean;
  onAllowance: () => void;
  health: Health | null;
  status: WorkspaceStatus | null;
  cfg: Record<string, unknown>;
  workspace: string;
  gitBranch: string;
  shutdown: { status: string; message?: string } | null;
  sessions: Session[];
  targets: PickerTarget[];
  pickerLoaded: boolean;
  onRefreshModels: () => Promise<void>;
  /** A free model finished downloading (choose it if nothing is chosen). */
  onModelDownloaded: (model: DownloadModel) => void;
  pickerOpen: boolean;
  setPickerOpen: (open: boolean) => void;
  selectedTarget: PickerTarget | undefined;
  queuedJobs: Job[];
  queuedTaskIds: ReadonlySet<string>;
  fallback: Fallback | null;
  rowActions: RowActions;
  onDecide: (id: string, answer: ApprovalDecision) => void;
  commandCards: CommandResult[];
  approvals: Approval[];
  task: string;
  setTask: (task: string) => void;
  promptRef: RefObject<HTMLTextAreaElement | null>;
  commands: { name: string; description: string; arg_spec: string }[];
  submitting: boolean;
  queueing: boolean;
  commandWaiting: boolean;
  webEnabled: boolean;
  setWebEnabled: (enabled: boolean) => void;
  openSettings: (
    section?: SettingsSection,
    extra?: { advanced?: AdvancedTab; vendor?: string },
  ) => void;
  openSetup: (target: PickerTarget) => void;
  setPanel: (tab: DrawerTab) => void;
  reviewChanges: (path?: string) => void;
  refresh: () => Promise<void>;
  setPermissionMode: (mode: "ask" | "allow_edits") => void;
  toast: (text: string, kind?: ToastKind) => void;
  issueOffer?: {
    number: number;
    onOpen: () => void;
    onDismiss: () => void;
  } | null;
  extras: ComposerExtras;
  /** The full-width Review view, shown instead of the conversation. */
  reviewPanel?: ReactNode;
  /** The open conversation's worktree (Apply / Keep as branch / Discard). */
  worktreeBar?: ReactNode;
  /** "Try on…" is choosing a model to continue a stopped task. */
  tryOn?: TryOn | null;
  onTryOnModel?: (targetId: string) => void;
}) {
  const { transcript, job, busy, connection, history } = conversation;
  const { switching, sessionId, modelChoice, runningChoice } = nav;
  // Updates are this computer's business: a paired phone never asks.
  const updates = useUpdateNotice(isNative() && !isRemote());
  const view = compare.view;
  const locked = busy || submitting || switching || Boolean(shutdown);
  const composerLocked = submitting || switching || Boolean(shutdown);
  const access = composerAccess(cfg, status?.permissions.level, selectedTarget);
  const activeModel = job?.routing || transcript.routing;
  const chip = chipInput(activeModel, selectedTarget, transcript);
  const empty =
    !transcript.items.length &&
    !commandCards.length &&
    !busy &&
    !submitting &&
    !switching;
  const activeTaskId = transcript.activeTaskId || job?.task_id || "";
  const pendingNote = tryOn
    ? "Pick a model to continue the stopped task on"
    : busy && modelChoice && runningChoice && modelChoice !== runningChoice
      ? "Applies to your next message"
      : undefined;
  const estimate = useSpendEstimate(
    sessionId,
    selectedTarget,
    task.length,
    `${job?.id || ""}:${job?.status || ""}:${transcript.sessionUsage?.turns || 0}`,
  );
  const hasContent = Boolean(task.trim() || files.attachments.length);
  const worktreeReason =
    actions.worktreeBlocked ||
    (!hasContent
      ? "Add a task or attachment first"
      : !selectedTarget
        ? (actions.sendBlocked ?? "Choose a model first")
        : (actions.sendBlocked ??
          (composerLocked ? "Wait until the composer is ready" : null)));
  const focusWith = (text: string) => {
    setTask(text);
    promptRef.current?.focus();
  };
  // No model selected: show how to get one. A free model downloaded here is
  // selected as soon as it is ready (unless something was chosen meanwhile).
  const needsModel = pickerLoaded && !selectedTarget;
  const downloads = useModelDownloads({
    enabled: needsModel,
    onError: (text) => toast(text, "err"),
    onInstalled: onModelDownloaded,
  });
  const modelSetup = needsModel ? (
    <ModelSetupPanel
      downloads={downloads}
      hasReadyModel={targets.some(isReady)}
      onChooseModel={() => setPickerOpen(true)}
      onOpenRouter={() => openSettings("accounts", { vendor: "openrouter" })}
      onSubscription={() => openSettings("accounts")}
      onBrowse={() => openSettings("local")}
    />
  ) : undefined;
  return (
    <main className="stage">
      <StageBanners
        attached={Boolean(health?.desktop_attached)}
        limitVendor={transcript.limit?.vendor}
        onChooseModel={() => setPickerOpen(true)}
        interrupted={job?.status === "interrupted"}
        onContinueInterrupted={() =>
          focusWith(
            `Continue the interrupted task. Inspect the current files and verify the remaining work. Original request: ${job?.task}`,
          )
        }
        reconnecting={connection === "reconnecting"}
        laneName={
          compare.laneOf && view === "chat" ? compare.laneOf.name : undefined
        }
        switching={switching}
        onBackToCompare={() => void compare.back()}
        issueOffer={issueOffer}
      />
      {view === "compare" && (
        <Suspense fallback={<p role="status">Opening Compare…</p>}>
          <CompareView
            workspace={compare.projectPath}
            compareId={compare.compareId}
            targets={targets}
            onSelect={compare.setCompareId}
            onClose={() => compare.setView("chat")}
            onOpenLane={(record, lane) => void compare.openLane(record, lane)}
            onOpenDiff={(record, lane, path) =>
              void compare.openLaneDiff(record, lane, path)
            }
            onOpenChanges={(path) => {
              void refresh().catch(() => undefined);
              reviewChanges(path);
            }}
            onApplied={() => void refresh().catch(() => undefined)}
            onRecords={compare.noteLanes}
          />
        </Suspense>
      )}
      {view !== "compare" && reviewPanel}
      <ChatView
        hidden={view === "compare" || Boolean(reviewPanel)}
        streamRef={scroll.streamRef}
        onScroll={scroll.onScroll}
        empty={empty}
        switching={switching}
        shutdown={shutdown}
        onRetryQuit={() => void invoke("desktop_quit")}
        error={nav.error}
        canTrust={Boolean(workspace)}
        onTrust={() =>
          nav.setTrust(trustRequestFor(workspace, status?.permissions))
        }
        onReconnect={
          !health || connection === "reconnecting"
            ? () => void nav.boot()
            : undefined
        }
        onDismissError={() => nav.setError("")}
        history={history}
        onOlder={() => {
          scroll.release();
          void history.older();
        }}
        onNewer={() => {
          scroll.release();
          void history.newer();
        }}
        onLatest={() => {
          history.latest();
          scroll.pin();
        }}
        onSuggestion={focusWith}
        needsModel={needsModel}
        onChooseModel={() => setPickerOpen(true)}
        modelSetup={modelSetup}
        rows={
          <TranscriptRows
            items={transcript.items}
            activity={transcript.activity}
            activeTaskId={activeTaskId}
            liveTaskId={transcript.activeTaskId}
            queuedTaskIds={queuedTaskIds}
            fallback={fallback}
            locked={composerLocked}
            forkDisabled={busy || submitting || switching || controls.forking}
            actions={rowActions}
            scrollRef={scroll.streamRef}
            resetKey={`${sessionId}:${history.firstCursor}`}
          />
        }
        commandCards={commandCards}
        approvals={approvals}
        onDecide={onDecide}
        working={busy || submitting}
        activity={activeTaskId ? transcript.activity[activeTaskId] : undefined}
        job={job}
        busy={busy}
        onToast={toast}
      />
      {view === "chat" &&
        !reviewPanel &&
        (!scroll.atBottom || history.viewing) && (
          <button
            type="button"
            className="jump-latest"
            onClick={() => {
              if (history.viewing) history.latest();
              scroll.jumpToLatest();
            }}
          >
            <ArrowDown size={14} aria-hidden="true" />
            Latest activity
          </button>
        )}
      {view === "chat" && !reviewPanel && worktreeBar}
      <ComposerDock
        hidden={view === "compare" || Boolean(reviewPanel)}
        queue={{
          jobs: queuedJobs,
          sessions,
          selected: sessionId,
          cancelling: controls.cancellingQueued,
          disabled: submitting || switching || Boolean(shutdown),
          onCancel: (queued) => void controls.cancelQueued(queued),
          onOpen: (id) => void nav.openSession(id),
        }}
        plan={transcript.plan}
        composer={{
          task,
          onTask: setTask,
          promptRef,
          attachments: files.attachments,
          onRemoveAttachment: files.remove,
          onAttach: (list) => void files.attach(list),
          canAttachImages: selectedTarget?.vision === true,
          attachDisabled: composerLocked,
          commands,
          placeholder: queueing
            ? "Add a follow-up to the queue…"
            : empty
              ? "Describe what you want to build…"
              : "Ask for a follow-up change…",
          hint: commandWaiting
            ? "Commands wait until idle"
            : task
              ? queueing
                ? actions.worktreeBlocked
                  ? "↵ Queue"
                  : "↵ Queue · Ctrl+Shift+↵ Run now in a worktree"
                : "↵ Send"
              : "/ commands · @ files · ↑ earlier",
          estimate,
          busy,
          queueing,
          submitting,
          locked,
          canSend: actions.canSend,
          sendBlocked:
            hasContent || !selectedTarget ? actions.sendBlocked : null,
          stopDisabled: job?.status === "cancelling",
          onSubmit: () => void actions.submit(),
          onSubmitWorktree: actions.canRunInWorktree
            ? () => void actions.submit({ worktree: true })
            : undefined,
          onStop: () => void controls.stop(),
          mentions: extras.mentions,
          onMention: extras.addMention,
          onRemoveMention: extras.removeMention,
          context: extras.context,
          onRemoveContext: extras.removeContext,
          history: extras.history,
        }}
        modes={{
          mode: extras.mode,
          onMode: extras.setMode,
          effort: extras.effort,
          effortShown: extras.effortShown,
          onEffort: extras.setEffort,
        }}
        picker={{
          targets,
          value: modelChoice,
          open: pickerOpen,
          onOpenChange: setPickerOpen,
          loading: !pickerLoaded,
          onRefresh: onRefreshModels,
          note: pendingNote,
          onSelect: (id) => {
            if (tryOn && onTryOnModel) {
              onTryOnModel(id);
              return;
            }
            void nav.selectTarget(id);
            promptRef.current?.focus();
          },
          onConnect: (vendor) => openSettings("accounts", { vendor }),
          onSetup: openSetup,
          onAddLocal: () => openSettings("local"),
        }}
        permission={{
          mode: access.mode,
          readOnly: access.readOnly,
          vendorNote: access.vendorNote,
          onChange: setPermissionMode,
          onOpenSettings: () => openSettings("permissions"),
        }}
        network={{
          mode: access.network,
          ownLoop: access.ownLoop,
          webEnabled,
          onWeb: (next) => {
            setWebEnabled(next);
            writeStore("shadow:web", next ? "on" : "off");
          },
        }}
        compare={{
          reason: compare.blocked,
          locked: composerLocked,
          onOpen: compare.open,
        }}
        voice={{
          onError: (message) => toast(message, "err"),
          onOpenSettings: () => openSettings("voice"),
        }}
        worktree={{
          reason: worktreeReason,
          enabled: actions.canRunInWorktree,
          queueing,
          onRun: () => void actions.submit({ worktree: true }),
        }}
        worktreeNote={actions.worktreeBlocked}
      />
      <StatusBar
        busy={busy}
        paused={job?.status === "paused"}
        reconnecting={connection === "reconnecting"}
        connected={Boolean(health)}
        branch={gitBranch}
        onBranch={() => setPanel("git")}
        allowance={allowance.data}
        allowanceOpen={allowanceOpen}
        onAllowance={onAllowance}
        context={
          <ContextChip input={chip} compaction={transcript.compaction} />
        }
        update={noticeVersion(updates)}
        onUpdate={() => openSettings("about")}
        version={health?.version || ""}
      />
    </main>
  );
}
