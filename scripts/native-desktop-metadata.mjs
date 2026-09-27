// Tauri names the launcher after productName. AppStream and AppImageHub need
// the desktop entry and metadata to share the application's reverse-DNS ID.
import assert from "node:assert/strict";
import {
  lstat,
  readFile,
  readlink,
  rename,
  symlink,
  unlink,
} from "node:fs/promises";
import path from "node:path";

export const DESKTOP_ID = "com.shadowfetch.shadowcode";
export const DESKTOP_FILE = `${DESKTOP_ID}.desktop`;
export const METAINFO_FILE = `${DESKTOP_ID}.appdata.xml`;

export async function normalizeDesktopEntry(packageRoot, appimage = false) {
  const applications = path.join(packageRoot, "usr/share/applications");
  const oldDesktop = path.join(applications, "ShadowCode.desktop");
  const desktop = path.join(applications, DESKTOP_FILE);
  const contents = await readFile(oldDesktop, "utf8");
  assert.match(contents, /^Name=ShadowCode$/m);
  assert.match(contents, /^Type=Application$/m);
  assert.match(contents, /^Exec=shadowcode$/m);
  if (appimage) {
    const oldLink = path.join(packageRoot, "ShadowCode.desktop");
    // Fresh bundler output can contain a copy of the launcher; reused AppDirs
    // can contain a symlink. Validate either before replacing the root entry.
    const entry = await lstat(oldLink);
    if (entry.isSymbolicLink()) {
      assert.equal(
        await readlink(oldLink),
        "usr/share/applications/ShadowCode.desktop",
        "Unexpected Tauri AppDir launcher link",
      );
    } else {
      assert.ok(entry.isFile(), "Unexpected Tauri AppDir launcher type");
      assert.equal(
        await readFile(oldLink, "utf8"),
        contents,
        "Unexpected Tauri AppDir launcher contents",
      );
    }
    await unlink(oldLink);
  }
  await rename(oldDesktop, desktop);
  if (appimage)
    await symlink(
      `usr/share/applications/${DESKTOP_FILE}`,
      path.join(packageRoot, DESKTOP_FILE),
    );
}
