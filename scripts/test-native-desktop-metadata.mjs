import assert from "node:assert/strict";
import {
  mkdtemp,
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

for (const appimage of [false, true]) {
  test(`normalizes the ${appimage ? "AppImage" : "Debian"} desktop ID`, async () => {
    const root = await mkdtemp(path.join(tmpdir(), "shadowcode-desktop-id-"));
    try {
      const applications = path.join(root, "usr/share/applications");
      await mkdir(applications, { recursive: true });
      await writeFile(path.join(applications, "ShadowCode.desktop"), desktop);
      if (appimage)
        await symlink(
          "usr/share/applications/ShadowCode.desktop",
          path.join(root, "ShadowCode.desktop"),
        );
      await normalizeDesktopEntry(root, appimage);
      assert.equal(
        await readlink(path.join(root, DESKTOP_FILE)).catch(() => null),
        appimage ? `usr/share/applications/${DESKTOP_FILE}` : null,
      );
      assert.equal(await readFile(path.join(applications, DESKTOP_FILE), "utf8"), desktop);
      await assert.rejects(
        stat(path.join(applications, "ShadowCode.desktop")),
        { code: "ENOENT" },
      );
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
}
