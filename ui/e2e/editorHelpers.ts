import { expect, type Locator } from "@playwright/test";

/** Visible DOM only, for the short fixture documents that fit in the viewport.
 * This is not a full-document accessor for CodeMirror's virtualized viewport.
 * CRLF/CR byte assertions must read the saved fake-backend file separately. */
export async function expectEditorText(editor: Locator, expected: string) {
  if (expected.length > 8192 || expected.split("\n").length > 20)
    throw new Error("expectEditorText is only for short visible fixtures");
  await expect(editor).toBeVisible();
  await expect
    .poll(() =>
      editor.evaluate((element) =>
        element instanceof HTMLTextAreaElement
          ? element.value
          : Array.from(
              element.querySelectorAll(".cm-line"),
              (line) => line.textContent ?? "",
            ).join("\n"),
      ),
    )
    .toBe(expected);
}
