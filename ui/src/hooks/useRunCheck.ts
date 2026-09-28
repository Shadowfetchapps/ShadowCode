import type { RefObject } from "react";
import { api, type CheckJobRequest, type Job } from "../api";
import { sameWorkspacePath } from "../lib/trust";
import { useStableCallback } from "./useStableCallback";

/** A check is a new native command task, independent of the composer/model. */
export function useRunCheck(context: {
  workspace: string;
  sessionId: string;
  selectedRef: RefObject<string>;
  selection: RefObject<number>;
  submittingRef: RefObject<boolean>;
  locked: boolean;
  setSubmitting: (value: boolean) => void;
  start: (job: Job) => void;
  pin: () => void;
  refresh: () => Promise<unknown>;
}) {
  return useStableCallback(async (request: CheckJobRequest) => {
    if (context.locked || context.submittingRef.current)
      throw new Error(
        "Wait for the current task or navigation to finish before running a check.",
      );
    if (
      !context.sessionId ||
      request.session_id !== context.sessionId ||
      request.session_id !== context.selectedRef.current ||
      !sameWorkspacePath(request.workspace, context.workspace)
    )
      throw new Error(
        "The selected conversation changed. Reopen Run a check in its original project.",
      );

    const generation = context.selection.current;
    context.submittingRef.current = true;
    context.setSubmitting(true);
    try {
      const job = await api.startTestJob({ ...request, queue: false });
      if (
        generation === context.selection.current &&
        request.session_id === context.selectedRef.current &&
        job.session_id === request.session_id &&
        sameWorkspacePath(job.workspace, request.workspace)
      ) {
        context.start(job);
        context.pin();
      }
      // Once accepted, a failed list refresh must not invite duplicate execution.
      await context.refresh().catch(() => undefined);
    } finally {
      context.submittingRef.current = false;
      context.setSubmitting(false);
    }
  });
}
