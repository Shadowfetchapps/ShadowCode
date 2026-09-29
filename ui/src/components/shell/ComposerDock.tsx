import type { ComponentProps } from "react";
import { GitCompareArrows } from "lucide-react";
import type { Job, PlanStep, Session } from "../../api";
import { Composer } from "../Composer";
import { MicButton } from "../MicButton";
import { isRemote } from "../../lib/transport";
import {
  NetworkPill,
  PermissionControl,
  WebToggle,
  type PermissionMode,
} from "../ComposerControls";
import { ComposerMoreOptions } from "../ComposerMoreOptions";
import { RolesControl } from "../RolesControl";
import { EffortControl, ModeToggle } from "../ComposerModes";
import { QueuedTasks } from "../QueuedTasks";
import type { Effort, TaskMode } from "../../lib/effort";
import { UnifiedPicker } from "../UnifiedPicker";
import { RunInWorktreeButton } from "../RunInWorktreeButton";
import { TaskPlan } from "./Chrome";
import {
  isApiKey,
  isLocal,
  vendorKey,
  type PickerTarget,
} from "../../lib/picker";

type ComposerProps = ComponentProps<typeof Composer>;

/** How the chosen row runs: the permission mode, whether ShadowCode's own
 * agent loop (and so its web access) applies, and what a vendor CLI does
 * with these choices. */
export function composerAccess(
  cfg: Record<string, unknown>,
  level: string | undefined,
  target: PickerTarget | undefined,
) {
  const permissions = (cfg.permissions || {}) as Record<string, unknown>;
  const mode: PermissionMode =
    permissions.mode === "allow_edits" ? "allow_edits" : "ask";
  const readOnly = (level || permissions.level) === "read_only";
  const network = String(
    ((cfg.network || {}) as { mode?: string }).mode || "online",
  );
  const notes = (permissions.vendor_notes || {}) as Record<string, string>;
  // Local and OpenRouter rows run on ShadowCode's own agent loop, with its
  // tools, approvals and web access; only subscription CLIs bring their own.
  const ownLoop = Boolean(target && (isLocal(target) || isApiKey(target)));
  const vendorNote =
    target && !ownLoop
      ? notes[vendorKey(target)] ||
        notes[`cli-${vendorKey(target)}`] ||
        `${target.name.split(" · ")[0]} runs its own tools, sandbox and web access; ShadowCode passes this choice to it where the tool supports it.`
      : undefined;
  return {
    mode,
    readOnly,
    network,
    ownLoop,
    vendorNote,
    webAllowed: ownLoop && network === "online",
  };
}

/** Everything below the conversation: queued follow-ups, the task plan and
 * the composer with its model picker, permission and web controls and the
 * Compare button. */
export function ComposerDock({
  hidden,
  queue,
  plan,
  composer,
  picker,
  permission,
  network,
  compare,
  voice,
  modes,
  worktree,
  worktreeNote,
  roles,
}: {
  hidden: boolean;
  queue: {
    jobs: Job[];
    sessions: Session[];
    selected: string;
    cancelling: string[];
    disabled: boolean;
    onCancel: (job: Job) => void;
    onOpen: (id: string) => void;
  };
  plan: PlanStep[];
  composer: Omit<ComposerProps, "picker" | "controls" | "more">;
  picker: ComponentProps<typeof UnifiedPicker>;
  permission: {
    mode: PermissionMode;
    readOnly: boolean;
    vendorNote?: string;
    onChange: (mode: PermissionMode) => void;
    onOpenSettings: () => void;
  };
  network: {
    mode: string;
    /** ShadowCode's own agent loop runs the chosen row (web applies). */
    ownLoop: boolean;
    webEnabled: boolean;
    onWeb: (enabled: boolean) => void;
  };
  compare: { reason: string | null; locked: boolean; onOpen: () => void };
  /** Dictation: errors to show, and Settings › Voice when not set up. */
  voice: { onError: (message: string) => void; onOpenSettings: () => void };
  /** Code / Plan / Ask, and reasoning effort where the row supports it. */
  modes?: {
    mode: TaskMode;
    onMode: (mode: TaskMode) => void;
    effort: Effort;
    effortShown: boolean;
    onEffort: (effort: Effort) => void;
  };
  /** "Run in new worktree" (stays discoverable when it cannot apply). */
  worktree?: ComponentProps<typeof RunInWorktreeButton>;
  /** Why worktrees cannot be used in this project or conversation at all
   * (shown in the More menu; general reasons such as a missing model are
   * already shown under the composer). */
  worktreeNote?: string | null;
  /** The project's roles (Plan → Implement → Review), in More. */
  roles?: Omit<ComponentProps<typeof RolesControl>, "mode">;
}) {
  const effortIndicator =
    modes?.effortShown && modes.effort !== "default"
      ? modes.effort[0].toUpperCase() + modes.effort.slice(1)
      : undefined;
  // Unavailable options stay in the More menu so they can be found, with the
  // reason shown beside them rather than only in a hover tooltip. An empty
  // composer gets one line instead of the same reason under every option.
  const needsTask = !composer.task.trim() && !composer.attachments?.length;
  return (
    <div className="composer-wrap" hidden={hidden}>
      <QueuedTasks {...queue} />
      <TaskPlan plan={plan} />
      <Composer
        {...composer}
        picker={<UnifiedPicker {...picker} />}
        voice={
          // The host computer's microphone is never used for a remote device.
          isRemote() ? undefined : (
            <MicButton
              task={composer.task}
              onTask={composer.onTask}
              promptRef={composer.promptRef}
              disabled={hidden}
              onError={voice.onError}
              onOpenSettings={voice.onOpenSettings}
            />
          )
        }
        controls={
          <>
            {modes && <ModeToggle mode={modes.mode} onChange={modes.onMode} />}
            <PermissionControl {...permission} />
            {network.mode === "offline" ? (
              <NetworkPill mode="offline" />
            ) : network.ownLoop ? (
              network.mode === "web_off" ? (
                <NetworkPill mode="web_off" />
              ) : (
                <WebToggle
                  enabled={network.webEnabled}
                  onChange={network.onWeb}
                />
              )
            ) : null}
          </>
        }
        more={
          <ComposerMoreOptions
            indicator={effortIndicator}
            roles={Boolean(roles?.view?.setup.pipeline)}
          >
            {modes?.effortShown && (
              <EffortControl effort={modes.effort} onChange={modes.onEffort} />
            )}
            {roles && <RolesControl {...roles} mode={modes?.mode || "code"} />}
            {needsTask && (
              <p className="composer-more-reason">
                Type a task first to run it in a new worktree or compare models
                on it.
              </p>
            )}
            {worktree && <RunInWorktreeButton {...worktree} />}
            {worktree && worktreeNote && !needsTask && (
              // The button's accessible name already carries the reason.
              <p className="composer-more-reason" aria-hidden="true">
                {worktreeNote}
              </p>
            )}
            <button
              type="button"
              className="compare-btn"
              aria-disabled={Boolean(compare.reason) || compare.locked}
              aria-describedby={compare.reason ? "compare-blocked" : undefined}
              title={
                compare.reason ||
                "Run this task on 2–3 models at once and keep the best result"
              }
              onClick={() => {
                if (!compare.reason && !compare.locked) compare.onOpen();
              }}
            >
              <GitCompareArrows size={15} aria-hidden="true" />
              <span className="compare-btn-text">Compare</span>
            </button>
            {compare.reason && (
              <p
                id="compare-blocked"
                className={needsTask ? "sr-only" : "composer-more-reason"}
              >
                {compare.reason}
              </p>
            )}
          </ComposerMoreOptions>
        }
      />
    </div>
  );
}
