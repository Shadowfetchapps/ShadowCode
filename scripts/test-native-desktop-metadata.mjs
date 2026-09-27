import assert from "node:assert/strict";
import {
  mkdtemp,
  lstat,
  mkdir,
  readFile,
  readlink,
  rm,
  stat,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import {
  DESKTOP_FILE,
  normalizeDesktopEntry,
} from "./native-desktop-metadata.mjs";

const desktop =
  "[Desktop Entry]\nType=Application\nName=ShadowCode\nExec=shadowcode\nIcon=shadowcode\n";

for (const layout of ["Debian", "AppImage symlink", "AppImage regular file"]) {
  const appimage = layout !== "Debian";
  test(`normalizes the ${layout} desktop ID`, async () => {
    const root = await mkdtemp(path.join(tmpdir(), "shadowcode-desktop-id-"));
    try {
      const applications = path.join(root, "usr/share/applications");
      await mkdir(applications, { recursive: true });
      await writeFile(path.join(applications, "ShadowCode.desktop"), desktop);
      if (layout === "AppImage regular file")
        await writeFile(path.join(root, "ShadowCode.desktop"), desktop);
      else if (appimage)
        await symlink(
          "usr/share/applications/ShadowCode.desktop",
          path.join(root, "ShadowCode.desktop"),
        );
      await normalizeDesktopEntry(root, appimage);
      assert.equal(
        await readlink(path.join(root, DESKTOP_FILE)).catch(() => null),
        appimage ? `usr/share/applications/${DESKTOP_FILE}` : null,
      );
      assert.equal(
        await readFile(path.join(applications, DESKTOP_FILE), "utf8"),
        desktop,
      );
      await assert.rejects(
        stat(path.join(applications, "ShadowCode.desktop")),
        { code: "ENOENT" },
      );
      await assert.rejects(stat(path.join(root, "ShadowCode.desktop")), {
        code: "ENOENT",
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
}

for (const layout of ["unexpected symlink", "different file", "directory"]) {
  test(`rejects an AppDir ${layout} before modifying launchers`, async () => {
    const root = await mkdtemp(path.join(tmpdir(), "shadowcode-desktop-id-"));
    try {
      const applications = path.join(root, "usr/share/applications");
      const rootEntry = path.join(root, "ShadowCode.desktop");
      await mkdir(applications, { recursive: true });
      await writeFile(path.join(applications, "ShadowCode.desktop"), desktop);
      if (layout === "unexpected symlink")
        await symlink("other.desktop", rootEntry);
      else if (layout === "different file")
        await writeFile(
          rootEntry,
          desktop.replace("Exec=shadowcode", "Exec=other"),
        );
      else await mkdir(rootEntry);
      await assert.rejects(
        normalizeDesktopEntry(root, true),
        /Unexpected Tauri/,
      );
      await lstat(rootEntry);
      assert.equal(
        await readFile(path.join(applications, "ShadowCode.desktop"), "utf8"),
        desktop,
      );
      await assert.rejects(lstat(path.join(root, DESKTOP_FILE)), {
        code: "ENOENT",
      });
      await assert.rejects(lstat(path.join(applications, DESKTOP_FILE)), {
        code: "ENOENT",
      });
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
}
