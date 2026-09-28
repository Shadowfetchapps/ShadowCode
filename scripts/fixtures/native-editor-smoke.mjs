// Real WebDriver Files smoke shared by source and packaged desktop runs.
// The caller owns its disposable project, WebDriver session and cleanup.
import assert from "node:assert/strict";
import { readFile, writeFile } from "node:fs/promises";
import path from "node:path";

export const EDITOR_FILE = "native-editor.ts";
export const OTHER_FILE = "native-other.ts";
export const EDITOR_BASE = "export const answer: number = 42;\r\nexport const label = 'native fixture';\r\n";
export const OTHER_BASE = "export const other = true;\n";
const ADDED = "// edited through native keyboard";
const EDITOR_SAVED = EDITOR_BASE + ADDED;
const editor = `.cm-content[role="textbox"][aria-label="Edit ${EDITOR_FILE}"]`;
const other = `.cm-content[role="textbox"][aria-label="Edit ${OTHER_FILE}"]`;

// Seed alongside README, before the disposable project's base commit.
export async function seedNativeEditorFixtures(project) {
  await writeFile(path.join(project, EDITOR_FILE), EDITOR_BASE);
  await writeFile(path.join(project, OTHER_FILE), OTHER_BASE);
}

async function keyboard(ctx, key, modifier) {
  const actions = [
    ...(modifier ? [{ type: "keyDown", value: modifier }] : []),
    { type: "keyDown", value: key },
    { type: "keyUp", value: key },
    ...(modifier ? [{ type: "keyUp", value: modifier }] : []),
  ];
  try {
    await ctx.wd("POST", `/session/${ctx.session}/actions`, {
      actions: [{ type: "key", id: "native-editor-keyboard", actions }],
    });
  } finally {
    // Release modifiers even when the driver rejects an action. This is input
    // cleanup only; the existing harness owns all app/driver/process cleanup.
    await ctx.wd("DELETE", `/session/${ctx.session}/actions`).catch(() => {});
  }
}

async function visibleText(ctx, selector = editor) {
  return ctx.execute(`
    const e = document.querySelector(arguments[0]);
    if (!e || !e.getClientRects().length) return null;
    return [...e.querySelectorAll('.cm-line')].map(l => l.textContent || '').join('\\n');
  `, [selector]);
}

async function expectText(ctx, expected, selector = editor) {
  assert.ok(expected.length < 8192 && expected.split("\n").length < 20);
  await ctx.until("Short CodeMirror fixture has expected visible text", async () =>
    await visibleText(ctx, selector) === expected.replace(/\r\n|\r/g, "\n"));
}

async function openFixture(ctx, name, selector) {
  // Restrict to the filename text node in the Files listing. Icon/name nodes
  // need not have whitespace between them; do not guess their combined label.
  assert.ok([EDITOR_FILE, OTHER_FILE].includes(name));
  const xpath = `//div[contains(@class,'drawer-body')]//button[contains(concat(' ',normalize-space(@class),' '),' file ') and text()='${name}']`;
  const id = await ctx.until(`File listed: ${name}`, async () =>
    (await ctx.wd("POST", `/session/${ctx.session}/element`, { using: "xpath", value: xpath }))["element-6066-11e4-a52e-4f735466cecf"]);
  await ctx.wd("POST", `/session/${ctx.session}/element/${id}/click`, {});
  await ctx.until(`Native editor open for ${name}`, () => ctx.visible(selector));
}

async function savedState(ctx) {
  return ctx.execute(`
    const toolbar = document.querySelector('.file-editor-toolbar');
    return !!toolbar && toolbar.querySelector('[role=status]')?.textContent === 'Saved'
      && [...toolbar.querySelectorAll('button')].some(b => b.textContent.trim() === 'Save' && b.disabled);
  `);
}

async function installDiagnostics(ctx) {
  await ctx.execute(`
    const evidence = { csp: [], runtimeErrors: 0, promiseErrors: 0, overflow: false };
    const csp = event => {
      if (evidence.csp.length >= 16) { evidence.overflow = true; return; }
      // Retain directive names only: no blocked URL, source, message, stack or
      // private document content. Counts fail the check; they are not ignored.
      evidence.csp.push(String(event.effectiveDirective || '').slice(0, 80));
    };
    const error = () => { evidence.runtimeErrors++; };
    const rejection = () => { evidence.promiseErrors++; };
    addEventListener('securitypolicyviolation', csp);
    addEventListener('error', error);
    addEventListener('unhandledrejection', rejection);
    window.__nativeEditorEvidence = { evidence, stop() {
      removeEventListener('securitypolicyviolation', csp);
      removeEventListener('error', error);
      removeEventListener('unhandledrejection', rejection);
    }};
  `);
}

async function finishDiagnostics(ctx) {
  const evidence = await ctx.execute(`
    const state = window.__nativeEditorEvidence;
    if (!state) return null;
    state.stop(); delete window.__nativeEditorEvidence; return state.evidence;
  `);
  await writeFile(path.join(ctx.artifacts, "native-editor-diagnostics.json"), JSON.stringify(evidence, null, 2));
  assert.deepEqual(evidence, { csp: [], runtimeErrors: 0, promiseErrors: 0, overflow: false },
    "No CSP violation or page exception during the editor interval; unrelated exceptions remain visible failures");
}

// Called after the Changes screenshot, before closing that drawer.
// ctx: {wd,session,execute,until,visible,click,clickButton,type,note,
//       screenshot,project,artifacts}. Reuse the harness's bounded helpers.
export async function nativeEditorBeforeReload(ctx) {
  await installDiagnostics(ctx);
  let failure;
  try {
    await ctx.clickButton("Files", "//div[@class='drawer-tabs']");
    await openFixture(ctx, EDITOR_FILE, editor);
    await expectText(ctx, EDITOR_BASE);
    assert.equal(await savedState(ctx), true, "Opening a file does not dirty it");
    assert.deepEqual(await readFile(path.join(ctx.project, EDITOR_FILE)), Buffer.from(EDITOR_BASE));
    await ctx.until("TypeScript chunk loaded and rendered under native CSP", () => ctx.execute(`
      const e = document.querySelector(arguments[0]);
      return !!e && [...e.querySelectorAll('.cm-line span')].some(s =>
        /^(export|const)$/.test(s.textContent || '') && getComputedStyle(s).color !== getComputedStyle(e).color)
        && !document.querySelector('.file-editor')?.textContent.includes('Syntax highlighting is unavailable');
    `, [editor]));

    await ctx.click(editor);
    await keyboard(ctx, "\uE010", "\uE009"); // Ctrl+End; no clipboard use.
    await ctx.type(editor, ADDED); // Actual WebDriver key input, not DOM assignment.
    await expectText(ctx, EDITOR_SAVED);
    assert.deepEqual(await readFile(path.join(ctx.project, EDITOR_FILE)), Buffer.from(EDITOR_BASE),
      "Typing only changes the draft before Save");

    // Opening an uncached second file must not destroy the first file's undo
    // history. Return through its tab; no fake API or forced editor state.
    await openFixture(ctx, OTHER_FILE, other);
    await expectText(ctx, OTHER_BASE, other);
    // The accessible tab label includes the absolute workspace path while
    // the Files list intentionally displays only the basename. Re-selecting
    // the file from that list exercises the same tab switch without assuming
    // the backend's path presentation.
    await openFixture(ctx, EDITOR_FILE, editor);
    await expectText(ctx, EDITOR_SAVED);
    await ctx.click(editor);
    // Driver typing may form multiple history groups; accept only bounded
    // keyboard Undo transitions that recover the exact original fixture.
    for (let n = 0; n <= ADDED.length && await visibleText(ctx) !== EDITOR_BASE.replace(/\r\n/g, "\n"); n++)
      await keyboard(ctx, "z", "\uE009");
    await expectText(ctx, EDITOR_BASE);
    await ctx.type(editor, ADDED);
    await expectText(ctx, EDITOR_SAVED);
    await keyboard(ctx, "s", "\uE009");
    await ctx.until("Native guarded Save persisted exact CRLF bytes", async () =>
      (await readFile(path.join(ctx.project, EDITOR_FILE))).equals(Buffer.from(EDITOR_SAVED)) && await savedState(ctx));
    assert.deepEqual(await readFile(path.join(ctx.project, OTHER_FILE)), Buffer.from(OTHER_BASE), "Other file unchanged");
    await ctx.screenshot("files-editor");
    ctx.note("Files: native TypeScript highlighting, real keyboard edit, first-open file switch, Undo, guarded Save, exact CRLF bytes");
  } catch (error) {
    failure = error;
  } finally {
    // Preserve diagnostics even on an earlier assertion; do not mask that
    // original error. The caller's existing finally owns process cleanup.
    try { await finishDiagnostics(ctx); } catch (error) { failure ??= error; }
  }
  if (failure) throw failure;
}

// Called after the harness's real page refresh/conversation restore.
// Reopen through existing Review changes button, then use fresh file read.
export async function nativeEditorAfterReload(ctx) {
  await ctx.clickButton("Review changes");
  await ctx.clickButton("Files", "//div[@class='drawer-tabs']");
  await openFixture(ctx, EDITOR_FILE, editor);
  await expectText(ctx, EDITOR_SAVED);
  await ctx.until("Saved state after actual native page reload", () => savedState(ctx));
  assert.deepEqual(await readFile(path.join(ctx.project, EDITOR_FILE)), Buffer.from(EDITOR_SAVED));
  assert.deepEqual(await readFile(path.join(ctx.project, OTHER_FILE)), Buffer.from(OTHER_BASE));
  await ctx.click("button.drawer-close");
  ctx.note("Files reload retains exact saved content without an unsaved draft");
}
