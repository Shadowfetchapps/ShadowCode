import type { EventRow, Job } from "../api";
import { isActive } from "./jobs";
import { listen, request } from "./transport";

export interface JobStream {
  onopen: (() => void) | null;
  onmessage: ((event: { data: string }) => void) | null;
  onerror: (() => void) | null;
  close(): void;
}
type Page = { events: EventRow[]; job: Job };
type Dependencies = {
  read: (after: number) => Promise<Page>;
  subscribe: (wake: () => void) => Promise<() => void>;
};

/** Coalesce only adjacent text fragments within one fetched page. Keeping the
 * last durable ID preserves reconnect progress; the stored rows stay intact. */
export function compactStreamRows(
  events: EventRow[],
  after: number,
): EventRow[] {
  const result: EventRow[] = [];
  let cursor = after;
  for (const event of events) {
    if (!event.id || event.id <= cursor) continue;
    cursor = event.id;
    const previous = result.at(-1);
    const text = event.payload?.text;
    const message = event.payload?.message_id;
    if (
      event.type === "model.stream" &&
      previous?.type === "model.stream" &&
      event.task_id === previous.task_id &&
      event.session_id === previous.session_id &&
      typeof message === "string" &&
      message.length > 0 &&
      message === previous.payload?.message_id &&
      typeof text === "string" &&
      typeof previous.payload?.text === "string" &&
      previous.payload.text.length + text.length <= 65536 &&
      Object.keys(event.payload).every(
        (key) => key === "text" || key === "message_id",
      ) &&
      Object.keys(previous.payload).every(
        (key) => key === "text" || key === "message_id",
      )
    ) {
      result[result.length - 1] = {
        ...event,
        payload: { ...event.payload, text: previous.payload.text + text },
      };
    } else result.push(event);
  }
  return result;
}

/** Notifications only wake the reader. State comes from the saved job snapshot
 * and transcript content comes from ordered, durable rows. */
export function nativeJobStream(after: number, deps: Dependencies): JobStream {
  let closed = false;
  let reading = false;
  let again = false;
  let connected = false;
  let cursor = after;
  let status = "";
  let unsubscribe: (() => void) | undefined;
  let scheduled: ReturnType<typeof setTimeout> | undefined;
  const source: JobStream = {
    onopen: null,
    onmessage: null,
    onerror: null,
    close() {
      if (closed) return;
      closed = true;
      clearInterval(poll);
      clearTimeout(scheduled);
      unsubscribe?.();
    },
  };
  async function drain() {
    if (closed) return;
    if (reading) {
      again = true;
      return;
    }
    reading = true;
    try {
      do {
        again = false;
        const page = await deps.read(cursor);
        if (closed) return;
        if (!connected) {
          connected = true;
          source.onopen?.();
        }
        const active = isActive(page.job);
        if (active && status !== page.job.status) {
          status = page.job.status;
          // Deliver before rows, so a start event in this page can promote a
          // queued snapshot. Snapshot cursors must never skip transcript rows.
          source.onmessage?.({
            data: JSON.stringify({ type: "job.state", payload: page.job }),
          });
        }
        for (const event of compactStreamRows(page.events, cursor)) {
          if (closed) return;
          if (event.id && event.id > cursor) {
            source.onmessage?.({ data: JSON.stringify(event) });
            cursor = event.id;
          }
        }
        if (!active && cursor >= (page.job.event_cursor || 0)) {
          source.onmessage?.({
            data: JSON.stringify({ type: "job.done", payload: page.job }),
          });
          source.close();
          return;
        }
        // The IPC service caps each page at 512 events. Catch up before waiting
        // for another notification, including when completion was already saved.
        again ||= page.events.length >= 512;
      } while (again && !closed);
    } catch {
      if (!closed) {
        connected = false;
        source.onerror?.();
      }
    } finally {
      reading = false;
    }
  }
  function wake() {
    if (closed || scheduled) return;
    scheduled = setTimeout(() => {
      scheduled = undefined;
      void drain();
    }, 20);
  }
  const poll = setInterval(wake, 2500);
  void deps
    .subscribe(wake)
    .then((stop) => {
      if (closed) stop();
      else {
        unsubscribe = stop;
        wake();
      }
    })
    .catch(() => {
      if (!closed) source.onerror?.();
    });
  // A failed listener still has durable replay and polling recovery.
  wake();
  return source;
}

export function jobEvents(id: string, after: number): JobStream {
  return nativeJobStream(after, {
    read: (cursor) =>
      request<Page>(`/api/jobs/${id}/events?after=${cursor}&limit=512`),
    subscribe: (wake) => listen("shadowcode:events", wake),
  });
}
