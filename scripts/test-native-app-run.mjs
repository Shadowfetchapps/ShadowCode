import assert from "node:assert/strict";
import test from "node:test";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { chmod, copyFile, mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";

const run = promisify(execFile);
for (const backend of [undefined, "", "x11", "wayland"]) {
  test(`AppRun preserves ${backend === undefined ? "unset" : JSON.stringify(backend)} backend semantics and caller arguments`, async () => {
    const root = await mkdtemp(path.join(tmpdir(), "shadowcode app-run "));
    try {
      const appdir = path.join(root, "app");
      const cwd = path.join(root, "project");
      await mkdir(path.join(appdir, "apprun-hooks"), { recursive: true });
      await mkdir(path.join(appdir, "usr/bin"), { recursive: true });
      await mkdir(cwd);
      await copyFile(new URL("../packaging/native-app-run.sh", import.meta.url), path.join(appdir, "AppRun"));
      await writeFile(path.join(appdir, "apprun-hooks/linuxdeploy-plugin-gtk.sh"), "export GDK_BACKEND=x11\n");
      const executable = path.join(appdir, "usr/bin/shadowcode");
      await writeFile(executable, '#!/usr/bin/env bash\nprintf "%s\\0" "$GDK_BACKEND" "$PWD" "$@"\n');
      await chmod(executable, 0o755);
      const env = { ...process.env };
      delete env.GDK_BACKEND;
      if (backend !== undefined) env.GDK_BACKEND = backend;
      const args = ["ui", "a path with spaces", "$(must remain literal)"];
      const { stdout } = await run("bash", [path.join(appdir, "AppRun"), ...args], { cwd, env, timeout: 5000 });
      assert.deepEqual(stdout.split("\0"), [backend || "x11", cwd, ...args, ""]);
    } finally {
      await rm(root, { recursive: true, force: true });
    }
  });
}
