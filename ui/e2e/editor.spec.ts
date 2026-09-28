import { test, expect, type Page } from "@playwright/test";
import { installFakeBackend } from "./fakeBackend";
import { expectEditorText } from "./editorHelpers";

// Actual rendered CodeMirror, with the existing test transport only at the
// filesystem boundary. No editor instance, transaction or internal state API
// is exposed to these tests. All fixture documents fit in the visible viewport.
test.beforeEach(async ({ page }) => {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(error.message));
  (page as Page & { errors?: string[] }).errors = errors;
  await page.addInitScript(installFakeBackend, {});
  await page.goto("/");
  await expect(
    page.getByRole("heading", { name: "What should we work on?" }),
  ).toBeVisible();
});

test.afterEach(async ({ page }) => {
  expect((page as Page & { errors?: string[] }).errors).toEqual([]);
});

async function seedFiles(page: Page, files: Record<string, string>) {
  await page.evaluate((files) => {
    const fake = (window as any).__SHADOW_FAKE__;
    Object.assign(fake.state.files, files);
    const bridge = window.__SHADOW_TEST_TRANSPORT__!;
    const request = bridge.request;
    bridge.request = async (path, method, body) => {
      if (path.split("?")[0] === "/api/workspace/files" && method === "GET")
        return {
          workspace: "/work/demo",
          path: ".",
          parent: ".",
          entries: Object.keys(fake.state.files)
            .filter((name) => !name.includes("/"))
            .map((name) => ({ name, path: name, type: "file" })),
        };
      return request(path, method, body);
    };
  }, files);
}

async function openFile(page: Page, path: string) {
  const drawer = page.getByRole("complementary", { name: "Drawer" });
  if (!(await drawer.isVisible()))
    await page.getByRole("button", { name: "Review changes" }).click();
  await drawer
    .locator(".drawer-tabs")
    .getByRole("button", { name: "Files" })
    .click();
  await drawer.getByRole("button", { name: path, exact: true }).click();
  const editor = drawer.getByRole("textbox", {
    name: `Edit ${path}`,
    exact: true,
  });
  await expect(editor).toBeVisible();
  return { drawer, editor };
}

const savedFile = (page: Page, path: string) =>
  page.evaluate(
    (path) => (window as any).__SHADOW_FAKE__.state.files[path] as string,
    path,
  );

const fileWrites = (page: Page) =>
  page.evaluate(() =>
    (
      (window as any).__SHADOW_FAKE__.log as {
        method: string;
        path: string;
        body: { content: string };
      }[]
    ).filter(
      (entry) =>
        entry.method === "PUT" &&
        entry.path.split("?")[0] === "/api/workspace/file",
    ),
  );

test("renders syntax, searches and replaces, then undoes and saves through the live editor", async ({
  page,
}) => {
  const original = "const token = 1;\nconst second = token;\n";
  const replaced = "const result = 1;\nconst second = result;\n";
  await seedFiles(page, { "sample.ts": original });
  const { drawer, editor } = await openFile(page, "sample.ts");
  await expectEditorText(editor, original);
  const keyword = editor
    .locator(".cm-line span")
    .filter({ hasText: /^const$/ })
    .first();
  await expect(keyword).toBeVisible();
  await expect
    .poll(() =>
      keyword.evaluate(
        (element) =>
          getComputedStyle(element).color !==
          getComputedStyle(element.closest(".cm-content")!).color,
      ),
    )
    .toBe(true);
  for (const theme of ["light", "dark"]) {
    await page.evaluate(
      (theme) => (document.documentElement.dataset.theme = theme),
      theme,
    );
    await expect(page.locator("html")).toHaveAttribute("data-theme", theme);
    await drawer.screenshot({
      path: `test-results/editor-syntax-${theme}.png`,
    });
  }
  await page.evaluate(() => (document.documentElement.dataset.theme = "light"));
  expect(await fileWrites(page)).toEqual([]);
  await editor.press("Control+f");
  const search = drawer.getByRole("textbox", { name: "Find", exact: true });
  await expect(search).toBeFocused();
  await search.fill("token");
  await search.press("Tab");
  const replace = drawer.getByRole("textbox", { name: "Replace", exact: true });
  await replace.fill("result");
  await replace.press("Tab");
  await drawer
    .getByRole("button", { name: "replace all", exact: true })
    .click();
  await expectEditorText(editor, replaced);
  await editor.press("Escape");
  await expect(search).toHaveCount(0);
  await editor.press("Control+z");
  await expectEditorText(editor, original);
  await editor.press("Control+Shift+z");
  await expectEditorText(editor, replaced);
  await editor.press("Control+s");
  await expect.poll(() => savedFile(page, "sample.ts")).toBe(replaced);
  await expect(drawer.getByText("Saved", { exact: true })).toBeVisible();
  const writes = await fileWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].body.content).toBe(replaced);
});

test("typing a question mark edits the file without opening Help", async ({
  page,
}) => {
  const { drawer, editor } = await openFile(page, "README.md");
  await editor.press("Control+Home");
  await editor.press("End");
  // Generate a real key event; insertText/fill would not exercise the app's
  // global question-mark shortcut against a contenteditable editor.
  await page.keyboard.type("?");
  await expectEditorText(editor, "# Demo?\n");
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(editor).toBeFocused();
  await expect(drawer).toBeVisible();
  expect(await savedFile(page, "README.md")).toBe("# Demo\n");
  expect(await fileWrites(page)).toEqual([]);
});

test("keeps per-file undo separate and forgets history when a clean buffer closes", async ({
  page,
}) => {
  await seedFiles(page, { "other.py": 'print("old")\n' });
  const { drawer, editor: first } = await openFile(page, "README.md");
  await first.fill("# First file edit\n");
  await expectEditorText(first, "# First file edit\n");
  const { editor: second } = await openFile(page, "other.py");
  await second.fill('print("new")\n');
  await expectEditorText(second, 'print("new")\n');
  await second.press("Control+z");
  await expectEditorText(second, 'print("old")\n');
  await drawer
    .getByRole("button", { name: "Open README.md, unsaved", exact: true })
    .click();
  await expectEditorText(first, "# First file edit\n");
  await first.press("Control+z");
  await expectEditorText(first, "# Demo\n");
  await first.press("Control+Shift+z");
  await expectEditorText(first, "# First file edit\n");
  await first.press("Control+s");
  await expect(drawer.getByText("Saved", { exact: true })).toBeVisible();
  await drawer.getByRole("button", { name: "Close file", exact: true }).click();
  await expect(first).toHaveCount(0);
  await openFile(page, "README.md");
  await first.press("Control+z");
  await expectEditorText(first, "# First file edit\n");
  await drawer
    .getByRole("button", { name: "Open other.py", exact: true })
    .click();
  await expectEditorText(second, 'print("old")\n');
});

test("a clean external replacement clears undo instead of reviving older file bytes", async ({
  page,
}) => {
  const { drawer, editor } = await openFile(page, "README.md");
  await editor.fill("# Saved edit\n");
  await editor.press("Control+s");
  await expect(drawer.getByText("Saved", { exact: true })).toBeVisible();
  await page.evaluate(() => {
    (window as any).__SHADOW_FAKE__.state.files["README.md"] =
      "# New disk revision\n";
    window.dispatchEvent(new Event("focus"));
  });
  await expectEditorText(editor, "# New disk revision\n");
  await expect(
    drawer.getByRole("group", { name: "File conflict" }),
  ).toHaveCount(0);
  await editor.press("Control+z");
  await expectEditorText(editor, "# New disk revision\n");
  await expect(
    drawer.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  expect(await savedFile(page, "README.md")).toBe("# New disk revision\n");
  expect(await fileWrites(page)).toHaveLength(1);
});

test("keeps ordinary file undo while a mixed-newline file uses the plain editor", async ({
  page,
}) => {
  const mixed = "first\r\nsecond\rthird\n";
  await seedFiles(page, { "mixed.txt": mixed });
  const { drawer, editor: ordinary } = await openFile(page, "README.md");
  await ordinary.fill("# Ordinary draft\n");
  await expectEditorText(ordinary, "# Ordinary draft\n");

  const { editor: plain } = await openFile(page, "mixed.txt");
  expect(await plain.evaluate((element) => element.tagName)).toBe("TEXTAREA");
  await expect(drawer.locator(".cm-content")).toHaveCount(0);
  await plain.press("Control+Home");
  await plain.press("End");
  await page.keyboard.insertText("!");
  await expectEditorText(plain, "first!\nsecond\nthird\n");

  await drawer
    .getByRole("button", { name: "Open README.md, unsaved", exact: true })
    .click();
  await expectEditorText(ordinary, "# Ordinary draft\n");
  await ordinary.press("Control+z");
  await expectEditorText(ordinary, "# Demo\n");
  await ordinary.press("Control+Shift+z");
  await expectEditorText(ordinary, "# Ordinary draft\n");

  await drawer
    .getByRole("button", { name: "Open mixed.txt, unsaved", exact: true })
    .click();
  await expectEditorText(plain, "first!\nsecond\nthird\n");
  await plain.press("Control+s");
  await expect
    .poll(() => savedFile(page, "mixed.txt"))
    .toBe("first!\r\nsecond\rthird\n");
  expect(await savedFile(page, "README.md")).toBe("# Demo\n");
  const writes = await fileWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].body.content).toBe("first!\r\nsecond\rthird\n");
});

for (const [name, separator] of [
  ["CRLF", "\r\n"],
  ["CR", "\r"],
] as const) {
  test(`preserves ${name} bytes through no-op opening and a real Enter/save`, async ({
    page,
  }) => {
    const original = `first${separator}second${separator}`;
    const expected = `${original}third${separator}`;
    await seedFiles(page, { "newlines.txt": original });
    const { drawer, editor } = await openFile(page, "newlines.txt");
    await expectEditorText(editor, "first\nsecond\n");
    await editor.press("Control+s");
    await expect(
      drawer.getByRole("button", { name: "Save", exact: true }),
    ).toBeDisabled();
    expect(await fileWrites(page)).toEqual([]);
    expect(await savedFile(page, "newlines.txt")).toBe(original);
    await editor.press("Control+End");
    await page.keyboard.insertText("third");
    await editor.press("Enter");
    await expectEditorText(editor, "first\nsecond\nthird\n");
    await editor.press("Control+s");
    await expect.poll(() => savedFile(page, "newlines.txt")).toBe(expected);
    const writes = await fileWrites(page);
    expect(writes).toHaveLength(1);
    expect(writes[0].body.content).toBe(expected);
  });
}

test("keeps mixed separators in the plain fallback without incidental writes", async ({
  page,
}) => {
  const original = "first\r\nsecond\rthird\n";
  await seedFiles(page, { "mixed.txt": original });
  const { drawer, editor } = await openFile(page, "mixed.txt");
  await expect(drawer.locator(".file-editor-hint")).toContainText(
    "Mixed line endings · plain editing",
  );
  expect(await editor.evaluate((element) => element.tagName)).toBe("TEXTAREA");
  await expectEditorText(editor, "first\nsecond\nthird\n");
  await editor.press("Control+s");
  expect(await fileWrites(page)).toEqual([]);
  expect(await savedFile(page, "mixed.txt")).toBe(original);
  await expect(
    drawer.getByRole("button", { name: "Save", exact: true }),
  ).toBeDisabled();
  await editor.press("Control+Home");
  await editor.press("End");
  await page.keyboard.insertText("!");
  await expectEditorText(editor, "first!\nsecond\nthird\n");
  await editor.press("Control+s");
  await expect
    .poll(() => savedFile(page, "mixed.txt"))
    .toBe("first!\r\nsecond\rthird\n");
  const writes = await fileWrites(page);
  expect(writes).toHaveLength(1);
  expect(writes[0].body.content).toBe("first!\r\nsecond\rthird\n");
});
