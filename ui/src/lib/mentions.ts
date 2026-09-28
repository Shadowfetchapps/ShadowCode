/** @-mentions in the composer: the `@token` under the caret, and the files
 * and folders a message mentions. A picked file stays in the text as
 * `@path` (vendor CLIs read that) and as a chip; native models also get the
 * file's contents (`mentions` on POST /api/jobs). */

export type Mention = { path: string; kind: "file" | "dir" };

/** The `@word` the caret is in or just after, with where it starts and
 * ends. An `@` inside a word (an e-mail address) is not a mention. */
export function mentionAt(
  text: string,
  caret: number,
): { start: number; end: number; query: string } | null {
  const upto = text.slice(0, Math.max(0, Math.min(caret, text.length)));
  const match = /(^|\s)@([^\s@]*)$/.exec(upto);
  if (!match) return null;
  const start = upto.length - match[2].length - 1;
  const rest = /^[^\s]*/.exec(text.slice(upto.length))?.[0] || "";
  return { start, end: upto.length + rest.length, query: match[2] + rest };
}

/** How a mention is written in the message. Folders end with `/`. */
export const mentionToken = (m: Mention) =>
  `@${m.path}${m.kind === "dir" && !m.path.endsWith("/") ? "/" : ""}`;

/** Replace the `@query` token with the picked mention (and a space). */
export function insertMention(
  text: string,
  token: { start: number; end: number },
  value: string,
): { text: string; caret: number } {
  const after = text.slice(token.end).replace(/^ /, "");
  const next = `${text.slice(0, token.start)}${value} ${after}`;
  return { text: next, caret: token.start + value.length + 1 };
}

/** The chips still written in the message: deleting `@path` from the text
 * drops its attachment too. */
export function mentionsInText(text: string, chips: Mention[]): Mention[] {
  return chips.filter((m) => {
    const token = mentionToken(m);
    let at = text.indexOf(token);
    while (at >= 0) {
      const before = at === 0 ? " " : text[at - 1];
      const next = text[at + token.length];
      if (
        /\s/.test(before) &&
        (next === undefined || /\s|[.,;:!?)]/.test(next))
      )
        return true;
      at = text.indexOf(token, at + 1);
    }
    return false;
  });
}

/** Remove a mention's token from the message (its chip was removed). */
export function removeMentionText(text: string, m: Mention): string {
  const token = mentionToken(m);
  return text
    .split(/(\s+)/)
    .filter((part) => part !== token)
    .join("")
    .replace(/[ \t]{2,}/g, " ")
    .trim();
}

/** Whether `path` can still match `query` in the project search (its
 * letters appear in order, ignoring case). Used to drop earlier results that
 * the text typed since rules out while the new answer is on its way. */
export function fitsMentionQuery(path: string, query: string): boolean {
  const haystack = path.toLowerCase();
  let at = 0;
  for (const letter of query.toLowerCase()) {
    at = haystack.indexOf(letter, at);
    if (at < 0) return false;
    at += 1;
  }
  return true;
}
