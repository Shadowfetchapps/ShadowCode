/** Textareas expose LF-normalized values. Preserve original separators outside
 * the user's changed range instead of rewriting every CR/CRLF in the file. */
export function plainEditorRange(
  previous: string,
  from: number,
  to: number,
  insert: string | ((selected: string) => string),
): string {
  const originalOffset = (offset: number) => {
    let original = 0;
    for (let index = 0; index < offset; index++, original++) {
      if (previous[original] === "\r" && previous[original + 1] === "\n")
        original++;
    }
    return original;
  };
  const start = originalOffset(from);
  const end = originalOffset(to);
  return (
    previous.slice(0, start) +
    (typeof insert === "function"
      ? insert(previous.slice(start, end))
      : insert) +
    previous.slice(end)
  );
}

export function plainEditorEdit(previous: string, next: string): string {
  const normalized = previous.replace(/\r\n|\r/g, "\n");
  if (normalized === next) return previous;
  let start = 0;
  while (
    start < normalized.length &&
    start < next.length &&
    normalized[start] === next[start]
  )
    start++;
  let end = normalized.length;
  let nextEnd = next.length;
  while (
    end > start &&
    nextEnd > start &&
    normalized[end - 1] === next[nextEnd - 1]
  ) {
    end--;
    nextEnd--;
  }
  return plainEditorRange(previous, start, end, next.slice(start, nextEnd));
}
