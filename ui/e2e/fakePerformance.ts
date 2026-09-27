/** Browser-only performance fixture. Install after installFakeBackend.
 * Uses the desktop's 128-event history pages and normal job event transport.
 * The full fixture belongs to the fake backend; the UI receives one page.
 * Self-contained because Playwright serializes this function into the page.
 */
export function installFakePerformance(options: {
  messages?: number;
  initialReplyChars?: number;
  intervalMs?: number;
  initialReplyText?: string;
}) {
  const scope = window as any;
  const inner = scope.__SHADOW_TEST_TRANSPORT__;
  const fake = scope.__SHADOW_FAKE__;
  if (!inner || !fake) throw new Error("Install the base fake first");
  const count = options.messages ?? 10_000;
  const pageSize = 128;
  const session = fake.state.sessions[0];
  session.title = "Performance history";
  session.target = "local:llamacpp";
  const now = () => Math.floor(Date.now() / 1000);
  const saved = Array.from({ length: count }, (_, index) => ({
    id: index + 1,
    ts: now(),
    session_id: session.id,
    task_id: `saved-${Math.floor(index / 2)}`,
    type: index % 2 === 0 ? "user.message" : "model.delta",
    payload: {
      text:
        `Saved message ${String(index + 1).padStart(5, "0")}.` +
        (index % 2
          ? "\n\nRecorded answer with **emphasis**, `code`, and a short explanation."
          : ""),
      message_id: `saved-message-${index + 1}`,
    },
  }));
  const live: any[] = [];
  const listeners = new Set<(payload: unknown) => void>();
  const reads: { path: string; events: number; first: number; last: number }[] =
    [];
  let cursor = count;
  let timer: ReturnType<typeof setInterval> | undefined;
  let job: any = null;
  let fragments = 0;
  const page = (through: number) => {
    const events = saved.slice(
      Math.max(0, Math.min(count, through) - pageSize),
      Math.min(count, through),
    );
    return {
      events,
      first_cursor: events[0]?.id || 0,
      event_cursor: through,
      has_older: (events[0]?.id || 0) > 1,
    };
  };
  const emit = (type: string, payload: Record<string, unknown>) => {
    live.push({
      id: ++cursor,
      ts: now(),
      session_id: session.id,
      task_id: "performance-task",
      type,
      payload,
    });
    job.event_cursor = cursor;
    listeners.forEach((wake) => wake({ type, session_id: session.id }));
  };
  const record = (path: string, value: any) => {
    if (value.events)
      reads.push({
        path,
        events: value.events.length,
        first: value.events[0]?.id || 0,
        last: value.events.at(-1)?.id || 0,
      });
    return JSON.parse(JSON.stringify(value));
  };
  scope.__SHADOW_TEST_TRANSPORT__ = {
    ...inner,
    async listen(event: string, handler: (payload: unknown) => void) {
      const stop = await inner.listen(event, handler);
      if (event === "shadowcode:events") listeners.add(handler);
      return () => {
        listeners.delete(handler);
        stop();
      };
    },
    async request(fullPath: string, method: string, body: unknown) {
      const [path, query = ""] = fullPath.split("?");
      const q = new URLSearchParams(query);
      if (path === `/api/sessions/${session.id}/events`) {
        fake.log.push({ path: fullPath, method, body });
        return record(fullPath, page(Number(q.get("before") || count + 1) - 1));
      }
      if (
        path === `/api/sessions/${session.id}` ||
        path === `/api/sessions/${session.id}/activate`
      ) {
        const result = await inner.request(fullPath, method, body);
        const latest = page(count);
        return record(fullPath, {
          ...result,
          events: latest.events,
          event_cursor: count,
          history_page: {
            first_cursor: latest.first_cursor,
            has_older: latest.has_older,
          },
        });
      }
      if (path === "/api/jobs/current" && job) return { job: { ...job } };
      if (path === "/api/jobs/performance-job/events") {
        const events = live
          .filter((event) => event.id > Number(q.get("after") || 0))
          .slice(0, 512);
        return record(fullPath, { events, job });
      }
      if (path === "/api/jobs/performance-job") return { ...job };
      return inner.request(fullPath, method, body);
    },
  };
  scope.__SHADOW_PERFORMANCE__ = {
    reads,
    stats: () => ({
      fragments,
      cursor,
      listeners: listeners.size,
      storedMessages: saved.length,
      liveEvents: live.length,
    }),
    prepareStream() {
      job = {
        id: "performance-job",
        task_id: "performance-task",
        session_id: session.id,
        workspace: session.workspace,
        status: "running",
        task: "Continue a long streamed explanation",
        model: "local:llamacpp",
        model_name: "Fixture only",
        provider: "llamacpp",
        started_at: now(),
        event_cursor: count,
      };
      fake.state.jobs.push(job);
    },
    startStream() {
      if (!job || timer)
        throw new Error("Prepare the fixture once before streaming");
      emit("agent.started", { task: job.task, job_id: job.id });
      const paragraph =
        "## Implementation detail\n\nA streamed explanation with **formatting**, `code`, and a list.\n\n- Preserve history and review changes.\n- Verify the relevant behavior.\n\n";
      const size = options.initialReplyChars ?? 32_768;
      emit("model.stream", {
        message_id: "long-reply",
        text:
          options.initialReplyText ??
          paragraph.repeat(Math.ceil(size / paragraph.length)).slice(0, size),
      });
      timer = setInterval(() => {
        fragments++;
        emit("model.stream", {
          message_id: "long-reply",
          text: `\n\nUpdate ${fragments}: inspect the current files, retain **evidence**, and explain the result with bounded output.\n`,
        });
      }, options.intervalMs ?? 50);
    },
    stopStream() {
      clearInterval(timer);
      timer = undefined;
    },
    finishStream(text: string) {
      clearInterval(timer);
      timer = undefined;
      emit("model.delta", { message_id: "long-reply", text });
      job.status = "completed";
      job.summary = text;
      job.finished_at = now();
      emit("agent.completed", { success: true, summary: text });
    },
  };
}
