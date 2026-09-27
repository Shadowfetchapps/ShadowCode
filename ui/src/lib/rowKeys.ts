import type { ChatItem } from "../components/cards";

/** The identity a row keeps while it updates: a streamed answer by its
 * message id, a tool by its creating event, a legacy tool/hook by its call id. */
function stableKey(item: ChatItem): string | undefined {
  if (item.kind === "agent" && item.messageId)
    return `m:${item.taskId || ""}:${item.messageId}`;
  if (item.kind === "tool" && item.originEventId)
    return `t:${item.taskId || ""}:event:${item.originEventId}`;
  if (item.kind === "tool" && item.callId)
    return `t:${item.taskId || ""}:${item.callId}`;
  if (item.kind === "subagent") return `s:${item.run.runId}`;
  return undefined;
}

/** Gives every new transcript row a React key that survives later events:
 * message and call ids where the row has one, otherwise the id of the event
 * that created it (its position for rows without an event id). Rows the
 * reducer updates keep their key (it copies it onto replacements), so only
 * new rows need one. Replaying the same events always yields the same keys.
 *
 * Most events append: then only the unkeyed tail is visited, which keeps a
 * 10 000-event replay linear. An insert or removal elsewhere falls back to
 * one full pass. `next` must be the array the reducer built for this event
 * (never shared with `previous`); new rows are keyed in place. */
export function keyRows(
  previous: ChatItem[],
  next: ChatItem[],
  eventId: number | undefined,
): ChatItem[] {
  if (next === previous) return next;
  let tail = 0;
  while (tail < next.length && !next[next.length - 1 - tail].key) tail++;
  const appended = next.length - previous.length === tail;
  let serial = 0;
  for (
    let index = appended ? next.length - tail : 0;
    index < next.length;
    index++
  ) {
    const item = next[index];
    if (item.key) continue;
    let key =
      stableKey(item) ?? (eventId ? `e:${eventId}:${serial++}` : `n:${index}`);
    // Positions repeat once rows are removed; event and call ids do not.
    if (key.startsWith("n:"))
      while (next.some((other) => other.key === key)) key = `${key}+`;
    next[index] = { ...item, key };
  }
  return next;
}
