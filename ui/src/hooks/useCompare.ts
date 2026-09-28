import { useCallback, useRef, useState, type RefObject } from "react";
import {
  api,
  type CompareLane,
  type CompareRecord,
  type Session,
} from "../api";
import { compareBlocked } from "../lib/compare";
import { sameWorkspacePath } from "../lib/trust";
import { isReady, type PickerTarget } from "../lib/picker";
import type { Attachment } from "../lib/attachments";
import type { ToastKind } from "./useToasts";

/** The open conversation is one lane of a comparison. */
export type LaneOf = {
  compareId: string;
  model: string;
  name: string;
  title: string;
  /** The project the comparison belongs to ("" until known). */
  workspace: string;
};

/** What Compare needs from the rest of the window, read on every render. */
export type CompareContext = {
  workspace: string;
  sessionId: string;
  sessions: Session[];
  selectedRef: RefObject<string>;
  task: string;
  attachments: Attachment[];
  web: boolean;
  /** The project is a Git repository. */
  repo: boolean;
  /** Open editor drafts are not on disk and therefore cannot be snapshotted. */
  unsavedFiles: string[];
  targets: PickerTarget[];
  selectedTarget: PickerTarget | undefined;
  locked: boolean;
  openSession: (id: string) => Promise<void>;
  refresh: () => Promise<void>;
  reviewChanges: (path?: string) => void;
  toast: (text: string, kind?: ToastKind) => void;
  showDialog: () => void;
  /** The dialog started a comparison: clear the composer and close it. */
  started: () => void;
};

/** Compare: one task on 2–3 models, each in its own copy of the project,
 * the Compare view, and lane conversations that remember where they belong. */
export function useCompare(ctx: CompareContext) {
  const [view, setView] = useState<"chat" | "compare">("chat");
  const [compareId, setCompareId] = useState("");
  const [models, setModels] = useState<string[]>([]);
  const [laneOf, setLaneOf] = useState<LaneOf | null>(null);
  /** Lane copies seen in comparison records; activating a lane records its
   * copy as a project, which the project lists leave out. */
  const [laneTrees, setLaneTrees] = useState<string[]>([]);
  /** The project conversation to return to from a lane conversation. */
  const returnTo = useRef("");
  const selectedRef = ctx.selectedRef;
  /** Why Compare cannot start now, or null. */
  const blocked = compareBlocked({
    task: ctx.task,
    repo: ctx.repo,
    inLane: Boolean(laneOf),
    images: ctx.attachments.filter((a) => a.kind === "image").length,
  });

  const noteLanes = useCallback((records: CompareRecord[]) => {
    const paths = records.flatMap((r) =>
      r.lanes.map((lane) => lane.worktree).filter(Boolean),
    );
    setLaneTrees((prev) => {
      const next = [...new Set([...prev, ...paths])];
      return next.length === prev.length ? prev : next;
    });
  }, []);

  /** Lane conversations show where they belong; the comparison's project is
   * read once when the lane was not opened from the Compare view. */
  function trackLane(
    id: string,
    detail: {
      compare_id?: string | null;
      compare_lane?: string | null;
      title?: string;
    },
  ) {
    const cid = detail.compare_id || "";
    if (!cid) {
      setLaneOf(null);
      return;
    }
    const model = detail.compare_lane || "";
    const title = detail.title || "";
    setLaneOf((prev) =>
      prev && prev.compareId === cid && prev.model === model
        ? { ...prev, title }
        : {
            compareId: cid,
            model,
            name: title.replace(/^Compare · /, "") || model,
            title,
            workspace: "",
          },
    );
    void api
      .compare(cid)
      .then((record) => {
        noteLanes([record]);
        if (selectedRef.current !== id) return;
        const lane = record.lanes.find((l) => l.model === model);
        setLaneOf((prev) =>
          prev && prev.compareId === cid
            ? {
                ...prev,
                workspace: record.workspace,
                name: lane?.name || prev.name,
              }
            : prev,
        );
      })
      .catch(() => undefined);
  }

  function open() {
    if (blocked || ctx.locked) return;
    setModels((prev) => {
      const kept = prev.filter((id) => ctx.targets.some((t) => t.id === id));
      if (kept.length >= 2) return kept;
      const selected = ctx.selectedTarget;
      const first = selected && isReady(selected) ? selected.id : "";
      const next = kept.length ? kept : first ? [first] : [];
      while (next.length < 2) next.push("");
      return next;
    });
    ctx.showDialog();
  }

  async function start(chosen: string[]) {
    if (ctx.unsavedFiles.length)
      throw new Error(
        `Save or discard open drafts before comparing: ${ctx.unsavedFiles.join(", ")}`,
      );
    const texts = ctx.attachments
      .filter((a) => a.kind === "text")
      .map((a) => a.path);
    const text = (
      ctx.task.trim() +
      (texts.length ? `\n\nAttached paths: ${texts.join(", ")}` : "")
    ).trim();
    // Throws into the dialog, which shows the engine's reason inline.
    const record = await api.startCompare({
      workspace: ctx.workspace || undefined,
      task: text,
      models: chosen,
      web: ctx.web,
    });
    ctx.started();
    returnTo.current = ctx.sessionId;
    setCompareId(record.id);
    setView("compare");
    void ctx.refresh().catch(() => undefined);
  }

  /** Back from a lane's conversation to its project and the Compare view. */
  async function back(id = laneOf?.compareId || compareId) {
    const lane = laneOf;
    if (lane) {
      let project = lane.workspace;
      if (!project)
        project = await api
          .compare(lane.compareId)
          .then((r) => r.workspace)
          .catch(() => "");
      const previous =
        returnTo.current && returnTo.current !== ctx.sessionId
          ? returnTo.current
          : ctx.sessions.find(
              (s) => project && sameWorkspacePath(s.workspace, project),
            )?.id;
      try {
        if (previous) await ctx.openSession(previous);
        else if (project) {
          const opened = await api.openProject(project);
          await ctx.openSession(opened.session_id);
        }
      } catch (e) {
        ctx.toast(String(e), "err");
        return;
      }
    }
    setCompareId(id);
    setView("compare");
  }

  function openList(id = "") {
    if (laneOf) void back(id || laneOf.compareId);
    else {
      returnTo.current = ctx.sessionId;
      setCompareId(id);
      setView("compare");
    }
  }

  async function openLane(record: CompareRecord, lane: CompareLane) {
    if (!laneOf) returnTo.current = ctx.sessionId;
    const previous = laneOf;
    noteLanes([record]);
    setLaneOf({
      compareId: record.id,
      model: lane.model,
      name: lane.name,
      title: `Compare · ${lane.name}`,
      workspace: record.workspace,
    });
    setView("chat");
    await ctx.openSession(lane.session_id);
    if (selectedRef.current !== lane.session_id) setLaneOf(previous);
  }

  async function openLaneDiff(
    record: CompareRecord,
    lane: CompareLane,
    path: string,
  ) {
    await openLane(record, lane);
    ctx.reviewChanges(path);
  }

  return {
    blocked,
    view,
    setView,
    compareId,
    setCompareId,
    models,
    setModels,
    laneOf,
    laneTrees,
    /** The project comparisons belong to (a lane's copy belongs to its
     * comparison's project). */
    projectPath: laneOf?.workspace || ctx.workspace,
    noteLanes,
    trackLane,
    open,
    start,
    back,
    openList,
    openLane,
    openLaneDiff,
  };
}
