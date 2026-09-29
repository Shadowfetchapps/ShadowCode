import { lazy, Suspense, useEffect, useMemo, useRef, useState } from "react";
import { api, type CommandResult, type DownloadModel, type Job } from "./api";
import { Drawer, type DrawerTab } from "./components/Drawer";
import { Sidebar } from "./components/Sidebar";
import { Onboarding } from "./components/Onboarding";
import type { PermissionMode } from "./components/ComposerControls";
import type { ApprovalDecision } from "./components/ApprovalCard";
import type { AdvancedTab, SettingsSection } from "./components/Settings";
import { Stage } from "./components/shell/Stage";
import { TopBar } from "./components/shell/TopBar";
import { BootScreen, Toasts } from "./components/shell/Chrome";
import { composerAccess } from "./components/shell/ComposerDock";
import { AppDialogs, type Overlay } from "./components/shell/AppDialogs";
import { paletteItems } from "./components/shell/palette";
import { useConversation } from "./hooks/useConversation";
import { useToasts } from "./hooks/useToasts";
import { useTheme } from "./hooks/useTheme";
import { useFeed } from "./hooks/useFeed";
import { useShortcuts, type ShortcutAction } from "./hooks/useShortcuts";
import { useWorkspace } from "./hooks/useWorkspace";
import {
  useAllowance,
  usePickerTargets,
  useRefreshOnFocus,
} from "./hooks/useCatalog";
import { useCompare } from "./hooks/useCompare";
import { useStickyScroll } from "./hooks/useStickyScroll";
import { useDrawerMemory } from "./hooks/useDrawerMemory";
import { useStableCallback } from "./hooks/useStableCallback";
import { useTaskActions, type Consent } from "./hooks/useTaskActions";
import { useAttachments } from "./hooks/useAttachments";
import { useComposerExtras } from "./hooks/useComposerExtras";
import { useConversationEdits } from "./hooks/useConversationEdits";
import { RewindDialog } from "./components/RewindDialog";
import { useNavigation } from "./hooks/useNavigation";
import { useJobControls } from "./hooks/useJobControls";
import { useDesktopEvents, useSidebar } from "./hooks/useWindow";
import { useRowActions } from "./hooks/useRowActions";
import { useRunCheck } from "./hooks/useRunCheck";
import { useConversationBadges } from "./hooks/useConversationBadges";
import { useConversationMenu } from "./hooks/useConversationMenu";
import { useNotificationLinks } from "./hooks/useNotificationLinks";
import { useWorktreeTask } from "./hooks/useWorktreeTask";
import { WorktreeBar } from "./components/WorktreeBar";
import { OpinionContext } from "./components/TranscriptOpinions";
import { ConsentDialog } from "./components/ConsentDialog";
import { useConversationOpinions } from "./hooks/useConversationOpinions";
import { isLocal, vendorKey, type PickerTarget } from "./lib/picker";
import { limitsFrom, resolveFallback } from "./lib/allowance";
import {
  draftKey,
  isSessionCommand,
  readStore,
  writeStore,
} from "./lib/storage";
import { sameWorkspacePath } from "./lib/trust";
import { closingPr, issueFollowUp } from "./lib/issues";
import { exportSession as saveExport } from "./lib/transport";
import { isActive } from "./lib/jobs";

const ReviewView = lazy(() =>
  import("./components/ReviewView").then((module) => ({
    default: module.ReviewView,
  })),
);

/** The window. Behaviour lives in `hooks/` (navigation, the approvals and
 * jobs feed, the conversation stream, sending, Compare, shortcuts, theme);
 * layout lives in `components/shell/`. This component wires them together. */
export default function App() {
  const [pickerOpen, setPickerOpen] = useState(false);
  const [commands, setCommands] = useState<
    { name: string; description: string; arg_spec: string }[]
  >([]);
  const [task, setTask] = useState("");
  const [settings, setSettings] = useState<{
    section: SettingsSection;
    advanced?: AdvancedTab;
    vendor?: string;
  }>({ section: "accounts" });
  const [commandCards, setCommandCards] = useState<CommandResult[]>([]);
  const [webEnabled, setWebEnabled] = useState(
    () => readStore("shadow:web") === "on",
  );
  const [consent, setConsent] = useState<Consent | null>(null);
  const [panel, setPanel] = useState<DrawerTab | null>(null);
  const [diffPath, setDiffPath] = useState("");
  const [overlay, setOverlay] = useState<Overlay>("");
  const [submitting, setSubmitting] = useState(false);
  const allowanceReturn = useRef(false);
  const promptRef = useRef<HTMLTextAreaElement>(null);
  const submittingRef = useRef(false);
  const taskRef = useRef("");
  taskRef.current = task;

  const { toasts, toast, dismiss } = useToasts();
  const shutdown = useDesktopEvents(toast);
  const [sidebar, setSidebar] = useSidebar();
  const setJobs = useStableCallback((jobs: Job[]) => feed.setJobs(jobs));
  const ws = useWorkspace(setJobs);
  const { workspace, cfg, status, health, sessions, git, refresh } = ws;
  const picker = usePickerTargets(toast);
  const pickerTargets = picker.targets;
  const allowance = useAllowance();
  const reloadCatalog = () => {
    void picker.reload();
    void allowance.reload();
  };
  const memory = useDrawerMemory(workspace);
  useTheme(
    ws.configLoaded
      ? String((cfg.ui as { theme?: string } | undefined)?.theme || "system")
      : undefined,
  );

  function openSettings(
    section: SettingsSection = "accounts",
    extra: { advanced?: AdvancedTab; vendor?: string } = {},
  ) {
    setSettings({ section, ...extra });
    setOverlay("settings");
  }

  const conversation = useConversation((done) => {
    void refresh().catch(() => undefined);
    reloadCatalog();
    if (done?.status === "limit_reached") void actions.followLimit(done);
  });
  const { transcript, job, busy } = conversation;
  const jobRef = useRef(job);
  jobRef.current = job;

  // Callbacks below may reach hooks created later in this render; they
  // only run after it.
  const nav = useNavigation({
    ws,
    conversation,
    taskRef,
    setTask,
    clearComposer: () => {
      files.setAttachments([]);
      setCommandCards([]);
    },
    submittingRef,
    submitting,
    lanes: {
      track: (id, detail) => {
        compare.trackLane(id, detail);
        worktree.track(id, detail);
      },
      showChat: () => compare.setView("chat"),
      note: (records) => compare.noteLanes(records),
    },
    reloadCatalog,
    pin: () => scroll.pin(),
    toast,
    showProjects: () => setOverlay("project"),
    closeOverlay: () => setOverlay(""),
    focusPrompt: () => promptRef.current?.focus(),
  });
  const { sessionId, switching, trust, modelChoice } = nav;
  const feed = useFeed(sessionId);
  const { jobs, approvals } = feed;
  const conversationBadges = useConversationBadges({
    jobs,
    waiting: feed.waiting,
    sessionId,
    sessions,
  });
  const worktree = useWorktreeTask({
    sessionId,
    selectedRef: nav.selectedRef,
    jobs,
    openSession: nav.openSession,
    refresh,
    toast,
  });
  useNotificationLinks(sessionId, (id) => {
    compare.setView("chat");
    void nav.openSession(id);
  });

  const queueing =
    busy || jobs.some((item) => item.workspace === workspace && isActive(item));
  const composerLocked = submitting || switching || Boolean(shutdown);
  // A task the conversation already shows as started is not queued any more,
  // even before the next feed read says so.
  const liveTaskId = transcript.activeTaskId;
  const queuedJobs = useMemo(
    () =>
      jobs
        .filter(
          (item) =>
            item.workspace === workspace &&
            item.status === "queued" &&
            item.task_id !== liveTaskId &&
            // A second opinion waiting its turn shows on its own card.
            !item.second_opinion,
        )
        .slice()
        .reverse(),
    [jobs, workspace, liveTaskId],
  );
  const queuedTaskIds = useMemo(
    () => new Set(queuedJobs.map((queued) => queued.task_id || "")),
    [queuedJobs],
  );
  const commandWaiting = queueing && task.trim().startsWith("/");

  const selectedTarget = pickerTargets.find((t) => t.id === modelChoice);
  const selectedRef = useRef(selectedTarget);
  selectedRef.current = selectedTarget;
  // A free model finished downloading (in the empty conversation or in
  // Settings): it joins the picker, and is chosen when nothing is chosen yet.
  const adopted = useRef(new Map<string, number>());
  const adoptDownload = useStableCallback((model: DownloadModel) => {
    const id = model.model_id;
    if (!id || Date.now() - (adopted.current.get(id) || 0) < 10000) return;
    adopted.current.set(id, Date.now());
    void picker.reload().then(() => {
      if (!selectedRef.current) {
        void nav.selectTarget(id);
        toast(
          `${model.name} is ready. Ask it anything about this project.`,
          "ok",
        );
      } else
        toast(
          `${model.name} is downloaded and ready in the model picker.`,
          "ok",
        );
    });
  });
  const canAttachImages = selectedTarget?.vision === true;
  const { webAllowed } = composerAccess(
    cfg,
    status?.permissions.level,
    selectedTarget,
  );
  const files = useAttachments({
    locked: composerLocked,
    vision: canAttachImages,
    modelName: selectedTarget?.name,
    toast,
  });
  const { attachments, setAttachments } = files;
  const extras = useComposerExtras({
    workspace,
    sessionId,
    target: selectedTarget,
  });
  const scroll = useStickyScroll({
    active: nav.ready && !switching,
    content: [
      transcript.items,
      transcript.activity,
      commandCards,
      busy,
      submitting,
      approvals,
      queuedJobs.length,
    ],
    historyViewing: conversation.history.viewing,
    historyCursor: conversation.history.firstCursor,
  });
  const controls = useJobControls({
    conversation,
    feed,
    sessionId,
    selectedRef: nav.selectedRef,
    submitting,
    submittingRef,
    switching,
    setRunningChoice: nav.setRunningChoice,
    openSession: nav.openSession,
    refresh,
    toast,
  });

  useRefreshOnFocus(picker.fetched, reloadCatalog);
  const { reload: reloadPicker } = picker;
  const { reload: reloadAllowance } = allowance;
  useEffect(() => {
    if (transcript.usageVersion) {
      void reloadPicker();
      void reloadAllowance();
    }
  }, [transcript.usageVersion, reloadPicker, reloadAllowance]);
  useEffect(() => {
    document.title = `${busy ? "● " : ""}ShadowCode`;
  }, [busy]);
  useEffect(() => {
    const key = draftKey(sessionId, workspace);
    const timer = setTimeout(() => {
      if (isSessionCommand(task)) return;
      writeStore(key, task || null);
    }, 200);
    return () => clearTimeout(timer);
  }, [task, sessionId, workspace]);
  useEffect(() => {
    let current = true;
    api
      .commands()
      .then((result) => {
        if (current) setCommands(result.commands);
      })
      .catch(() => {
        if (current) setCommands([]);
      });
    return () => {
      current = false;
    };
  }, [workspace]);
  const reloadCommands = useStableCallback(async () => {
    const result = await api.commands();
    setCommands(result.commands);
  });

  /** A task's changes open the full-width Review; without a task, the
   * working tree opens in the Changes drawer. */
  function reviewChanges(path?: string, taskId?: string) {
    if (taskId) {
      setPanel((p) => (p === "changes" ? null : p));
      review.open(taskId, path);
      return;
    }
    setDiffPath(path || "");
    setPanel("changes");
  }
  function exportSession(format: "md" | "json" = "md") {
    if (sessionId)
      void saveExport(sessionId, format).catch((e) => toast(String(e), "err"));
  }
  async function setPermissionMode(mode: PermissionMode) {
    try {
      await api.saveConfig({ permissions: { mode } });
      await ws.reloadConfig();
    } catch (e) {
      toast(String(e), "err");
    }
  }
  const openSetup = (target: PickerTarget) =>
    isLocal(target)
      ? openSettings("local")
      : openSettings("accounts", { vendor: vendorKey(target) });

  const compare = useCompare({
    workspace,
    unsavedFiles: Object.values(memory.memory.filesBuffers)
      .filter((buffer) => buffer.draft !== buffer.base)
      .map((buffer) => buffer.path),
    sessionId,
    sessions,
    selectedRef: nav.selectedRef,
    task,
    attachments,
    web: webAllowed && webEnabled,
    repo: git.repo,
    targets: pickerTargets,
    selectedTarget,
    locked: composerLocked,
    openSession: nav.openSession,
    refresh,
    reviewChanges,
    toast,
    showDialog: () => setOverlay("compare"),
    started: () => {
      setOverlay("");
      setTask("");
      setAttachments([]);
      writeStore(draftKey(sessionId, workspace), null);
    },
  });
  // A worktree conversation belongs to its project.
  const projectPath = worktree.task?.workspace || compare.projectPath;
  const listedProjects = ws.projects.filter(
    (p) => !compare.laneTrees.some((tree) => sameWorkspacePath(tree, p.path)),
  );
  const conversations = useConversationMenu({
    sessions,
    projectPath,
    projects: listedProjects,
    sessionId,
    selectedRef: nav.selectedRef,
    openSession: (id) => {
      compare.setView("chat");
      return nav.openSession(id);
    },
    newSession: () => nav.newSession({ force: true }),
    mostRecent: conversationBadges.mostRecent,
    refresh,
    toast,
  });

  const actions = useTaskActions({
    task,
    setTask,
    attachments,
    setAttachments,
    workspace,
    sessionId,
    setSessionId: nav.setSessionId,
    selectedRef: nav.selectedRef,
    selection: nav.selection,
    submittingRef,
    taskRef,
    jobRef,
    busy,
    queueing,
    composerLocked,
    commandWaiting,
    setSubmitting,
    setError: nav.setError,
    setTrust: nav.setTrust,
    setConsent,
    health,
    status,
    pickerLoaded: picker.loaded,
    selectedTarget,
    modelChoice,
    canAttachImages,
    webAllowed,
    webEnabled,
    conversation,
    refresh,
    reloadConfig: ws.reloadConfig,
    reloadAllowance: allowance.reload,
    toast,
    newSession: nav.newSession,
    openSession: nav.openSession,
    openSettings,
    selectTarget: nav.selectTarget,
    setPanel,
    setPickerOpen,
    setCommandCards,
    setRunningChoice: nav.setRunningChoice,
    pin: scroll.pin,
    extras,
    gitRepo: git.repo,
    inWorktree: Boolean(worktree.task),
  });

  const shortcuts: Record<ShortcutAction, () => void> = {
    "close-overlay": () => setOverlay(""),
    "close-trust": () => nav.setTrust(null),
    "close-picker": () => setPickerOpen(false),
    "close-panel": () => setPanel(null),
    palette: () => setOverlay("palette"),
    sidebar: () => setSidebar((v) => !v),
    changes: () => setPanel((p) => (p ? null : "changes")),
    terminal: () => setPanel((p) => (p === "terminal" ? null : "terminal")),
    settings: () => openSettings(),
    project: () => setOverlay("project"),
    new: () => void nav.newSession(),
    model: () => setPickerOpen(true),
    focus: () => promptRef.current?.focus(),
    stop: () => void controls.stop(),
    export: () => exportSession(),
    help: () => setOverlay("help"),
    "previous-conversation": conversations.previous,
    "next-conversation": conversations.next,
    "recent-conversation": conversations.recent,
  };
  useShortcuts(
    {
      consent: Boolean(consent),
      overlay: Boolean(overlay),
      trust: Boolean(trust),
      picker: pickerOpen,
      panel: Boolean(panel),
      onboarding: nav.needsOnboard,
    },
    (action) => shortcuts[action](),
  );

  const { review, rewinding, messages, refreshAfterFiles } =
    useConversationEdits({
      conversation,
      jobRef,
      selectedRef: nav.selectedRef,
      submittingRef,
      sessionId,
      workspace,
      target: selectedTarget,
      busy: busy || submitting,
      queueing,
      openSession: nav.openSession,
      startTask: actions.startTask,
      refresh,
      toast,
    });
  const runCheck = useRunCheck({
    workspace,
    sessionId,
    selectedRef: nav.selectedRef,
    selection: nav.selection,
    submittingRef,
    locked: composerLocked || queueing,
    setSubmitting,
    start: conversation.start,
    pin: scroll.pin,
    refresh,
  });
  const rowActions = useRowActions({
    setTranscript: conversation.setTranscript,
    reviewChanges,
    rewind: rewinding.ask,
    editResend: messages.editResend,
    retry: messages.retry,
    copy: messages.copy,
    continueOnFallback: actions.continueOnFallback,
    chooseModel: () => setPickerOpen(true),
    openLocal: () => openSettings("local"),
    fork: controls.fork,
    openSession: nav.openSession,
    runCheck: sessionId
      ? {
          workspace,
          sessionId,
          disabled: composerLocked || queueing,
          onRun: runCheck,
        }
      : undefined,
  });
  const secondOpinions = useConversationOpinions({
    workspace,
    sessionId,
    targets: pickerTargets,
    toast,
    selectTarget: nav.selectTarget,
    draft: (text) => {
      setTask((current) => (current.trim() ? `${current}\n\n${text}` : text));
      promptRef.current?.focus();
    },
    reviewTask: (taskId) => reviewChanges(undefined, taskId),
    openSession: nav.openSession,
  });
  const onDecide = useStableCallback(
    (id: string, answer: ApprovalDecision) => void controls.decide(id, answer),
  );
  const issueLink = issueFollowUp(memory.memory.issueTask, job, busy);
  const issueOffer = issueLink
    ? {
        number: issueLink.number,
        onOpen: () => {
          memory.update("prDraft", (current) =>
            closingPr(issueLink, current.base),
          );
          memory.update("issueTask", null);
          setPanel("git");
        },
        onDismiss: () => memory.update("issueTask", null),
      }
    : null;
  const fallback = useMemo(
    () => resolveFallback(limitsFrom(cfg), allowance.data, pickerTargets),
    [cfg, allowance.data, pickerTargets],
  );

  if (!nav.ready)
    return (
      <BootScreen
        timedOut={nav.bootTimeout}
        error={nav.error}
        onRetry={() => {
          nav.setBootTimeout(false);
          void nav.boot();
        }}
      />
    );
  if (nav.needsOnboard)
    return (
      <Onboarding
        onDone={(next) => {
          nav.setNeedsOnboard(false);
          void nav.boot();
          if (next === "openrouter")
            openSettings("accounts", { vendor: "openrouter" });
          else if (next === "subscription") openSettings("accounts");
          else if (next === "local") openSettings("local");
        }}
      />
    );

  const view = compare.view;
  const currentSession = sessions.find((s) => s.id === sessionId);
  const palette = paletteItems({
    newTask: () => void nav.newSession(),
    chooseModel: () => setPickerOpen(true),
    openProject: () => setOverlay("project"),
    panel: setPanel,
    compare: () =>
      compare.blocked ? toast(compare.blocked, "info") : compare.open(),
    comparisons: () => compare.openList(),
    settings: (section, advanced) =>
      openSettings(section, advanced ? { advanced } : {}),
    exportTask: exportSession,
    stop: () => void controls.stop(),
    toggleTheme: () =>
      void api
        .saveConfig({
          ui: {
            theme:
              document.documentElement.dataset.theme === "dark"
                ? "light"
                : "dark",
          },
        })
        .then(ws.reloadConfig)
        .catch((e) => toast(String(e), "err")),
    help: () => setOverlay("help"),
  });

  return (
    <div
      className={`app ${sidebar ? "with-sidebar" : ""} ${panel ? "drawer-open" : ""} ${panel === "preview" ? "drawer-preview" : ""}`}
    >
      {sidebar && (
        <Sidebar
          sessions={sessions}
          projects={listedProjects}
          selected={sessionId}
          workspace={projectPath}
          jobs={jobs}
          badges={conversationBadges.badges}
          onAction={(action, session, title) =>
            void conversations.act(action, session, title)
          }
          onSelect={(id) => {
            compare.setView("chat");
            void nav.openSession(id);
          }}
          onNew={() => void nav.newSession()}
          onProject={(path) => void nav.pickProject(path)}
          onSettings={() => openSettings()}
          onHide={() => setSidebar(false)}
        />
      )}
      <TopBar
        sidebar={sidebar}
        onShowSidebar={() => setSidebar(true)}
        projectPath={projectPath}
        onProject={() => setOverlay("project")}
        title={currentSession?.title || compare.laneOf?.title || "New task"}
        comparing={view === "compare"}
        onComparisons={() =>
          view === "compare" ? compare.setView("chat") : compare.openList()
        }
        changesOpen={panel === "changes"}
        onChanges={() => setPanel(panel === "changes" ? null : "changes")}
        changeCount={git.count}
        onPalette={() => setOverlay("palette")}
      />
      <OpinionContext.Provider value={secondOpinions.context}>
        <Stage
          conversation={conversation}
          compare={compare}
          scroll={scroll}
          controls={controls}
          nav={nav}
          files={files}
          actions={actions}
          allowance={allowance}
          allowanceOpen={overlay === "allowance"}
          onAllowance={() => {
            allowanceReturn.current = false;
            setOverlay("allowance");
            void allowance.reload();
          }}
          health={health}
          status={status}
          cfg={cfg}
          workspace={workspace}
          gitBranch={git.branch}
          shutdown={shutdown}
          sessions={sessions}
          targets={pickerTargets}
          pickerLoaded={picker.loaded}
          onRefreshModels={() => picker.reload(true)}
          onModelDownloaded={adoptDownload}
          pickerOpen={pickerOpen}
          setPickerOpen={setPickerOpen}
          selectedTarget={selectedTarget}
          queuedJobs={queuedJobs}
          queuedTaskIds={queuedTaskIds}
          fallback={fallback}
          rowActions={rowActions}
          onDecide={onDecide}
          commandCards={commandCards}
          approvals={approvals}
          task={task}
          setTask={setTask}
          promptRef={promptRef}
          commands={commands}
          submitting={submitting}
          queueing={queueing}
          commandWaiting={commandWaiting}
          webEnabled={webEnabled}
          setWebEnabled={setWebEnabled}
          openSettings={openSettings}
          openSetup={openSetup}
          setPanel={setPanel}
          reviewChanges={reviewChanges}
          refresh={refresh}
          setPermissionMode={(mode) => void setPermissionMode(mode)}
          toast={toast}
          issueOffer={issueOffer}
          extras={extras}
          reviewPanel={
            review.target ? (
              <Suspense fallback={<p role="status">Opening review…</p>}>
                <ReviewView
                  key={review.target.taskId}
                  taskId={review.target.taskId}
                  initialPath={review.target.path}
                  busy={busy}
                  onClose={review.close}
                  toast={toast}
                  refresh={refreshAfterFiles}
                  onAskAgent={(prompt) => {
                    setTask(prompt);
                    review.close();
                    promptRef.current?.focus();
                  }}
                  memory={memory.memory}
                  onMemory={memory.update}
                  targets={pickerTargets}
                />
              </Suspense>
            ) : undefined
          }
          worktreeBar={
            worktree.task && (
              <WorktreeBar
                task={worktree.task}
                acting={worktree.acting}
                onAct={(action) => void worktree.act(action)}
              />
            )
          }
        />
      </OpinionContext.Provider>
      {secondOpinions.consent && (
        <ConsentDialog
          request={secondOpinions.consent.request}
          attachments={[]}
          onSend={secondOpinions.consent.send}
          onCancel={secondOpinions.consent.cancel}
        />
      )}
      {panel && (
        <Drawer
          key={workspace}
          tab={panel}
          onTab={setPanel}
          onClose={() => setPanel(null)}
          workspace={workspace}
          sessions={sessions}
          sessionId={sessionId}
          onOpenSession={(id) => void nav.openSession(id)}
          onNewSession={() => void nav.newSession()}
          onRefreshSessions={async () => {
            await refresh();
            const rows = (await api.sessions()).sessions;
            const selected = nav.selectedRef.current;
            if (selected && !rows.some((row) => row.id === selected)) {
              const next = rows.find((row) => row.workspace === workspace);
              if (next) await nav.openSession(next.id);
              else await nav.newSession();
            }
          }}
          diffPath={diffPath}
          onDiffPath={setDiffPath}
          busy={busy}
          toast={toast}
          onAskAgent={(prompt) => {
            setTask(prompt);
            setPanel(null);
            promptRef.current?.focus();
          }}
          onOpenProject={(path) => void nav.pickProject(path)}
          memory={memory.memory}
          onMemory={memory.update}
          onDiscardFileDraft={memory.discardFileDraft}
          onResolveFileDraftConflict={memory.resolveFileDraftConflict}
          targets={pickerTargets}
        />
      )}
      <Toasts toasts={toasts} onDismiss={dismiss} />
      {rewinding.asking && (
        <RewindDialog
          paths={rewinding.asking.paths}
          onConfirm={rewinding.confirm}
          onCancel={rewinding.cancel}
        />
      )}
      <AppDialogs
        overlay={overlay}
        setOverlay={setOverlay}
        trust={trust}
        onCancelTrust={() => nav.setTrust(null)}
        onConfirmTrust={() => void nav.confirmTrust()}
        consent={consent}
        targets={pickerTargets}
        onCancelConsent={() => {
          if (!consent) return;
          setTask(consent.original.task);
          setAttachments(consent.original.attachments);
          for (const m of consent.body.mentions || []) extras.addMention(m);
          if (consent.body.context?.length)
            extras.restoreContext(consent.body.context);
          setConsent(null);
          promptRef.current?.focus();
        }}
        onSendConsent={() => {
          if (!consent) return;
          const { body, original } = consent;
          setConsent(null);
          void actions.startTask({ ...body, handoff_consent: true }, original);
        }}
        settings={{
          cfg,
          initialSection: settings.section,
          initialAdvanced: settings.advanced,
          focusVendor: settings.vendor,
          health,
          sessionId,
          busy,
          onClose: () => setOverlay(""),
          onToast: toast,
          onOpenProject: (path) => void nav.pickProject(path),
          onOpenSession: (id) => void nav.openSession(id),
          onSkillsChanged: reloadCommands,
          onUseSkill: (name) => {
            setTask(`/skill ${name} `);
            promptRef.current?.focus();
          },
          onCatalogChanged: reloadCatalog,
          onModelDownloaded: adoptDownload,
          onSave: async (values) => {
            try {
              await api.saveConfig(values);
              await ws.reloadConfig();
              void picker.reload();
              toast("Settings saved", "ok");
            } catch (e) {
              toast(String(e), "err");
            }
          },
        }}
        limits={limitsFrom(cfg)}
        allowance={allowance}
        onSaveLimits={actions.saveLimits}
        onOpenSettings={openSettings}
        onSetup={openSetup}
        compare={{
          task: task.trim(),
          loading: !picker.loaded,
          models: compare.models,
          onModels: compare.setModels,
          uncommitted: git.count,
          unsavedFiles: Object.values(memory.memory.filesBuffers)
            .filter((buffer) => buffer.draft !== buffer.base)
            .map((buffer) => buffer.path),
          web: webAllowed && webEnabled,
          onStart: compare.start,
          onClose: () => {
            setOverlay("");
            promptRef.current?.focus();
          },
        }}
        allowanceReturn={allowanceReturn}
        version={health?.version || ""}
        palette={palette}
        projects={listedProjects}
        workspace={workspace}
        onPickProject={(path) => void nav.pickProject(path)}
      />
    </div>
  );
}
