// Prove packaging PATH ignores host Hermes/node hijacks and is enough for linuxdeploy.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, mkdir, writeFile, symlink, rm } from "node:fs/promises";
import { existsSync, readlinkSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import test from "node:test";
import {
  applyPackagingPath,
  bundleEnvironment,
  isUnsafePackagingDir,
  packagingDirs,
  packagingPath,
  SYSTEM_PACKAGING_DIRS,
} from "./native-packaging-env.mjs";

const exec = promisify(execFile);
const root = fileURLToPath(new URL("../", import.meta.url));
const linuxdeploy = path.join(
  root,
  "target/.tauri/linuxdeploy-x86_64.AppImage",
);
const hermesNode = "/usr/local/bin/node";
const dirtyPath = `${path.dirname(hermesNode)}:/snap/bin:/usr/bin:/bin`;

test("packaging PATH is constructed, not inherited", () => {
  const dirs = packagingDirs(root, { execDir: "/definitely-missing" });
  for (const dir of SYSTEM_PACKAGING_DIRS) {
    if (!existsSync(dir)) continue;
    const resolved = path.resolve(dir);
    assert.ok(
      dirs.includes(resolved),
      `${dir} should stay on PATH (merged-/usr systems still keep the name)`,
    );
  }
  const joined = dirs.join(":");
  assert.doesNotMatch(joined, /(^|:)\/usr\/local\/bin(:|$)/);
  assert.doesNotMatch(joined, /(^|:)\/snap\/bin(:|$)/);
  assert.doesNotMatch(joined, /\.hermes/);
  const rustDev = path.resolve(root, "../tools/rust-dev/extracted/usr/bin");
  if (existsSync(rustDev)) assert.ok(dirs.includes(rustDev));
  const tauriTools = path.join(root, "target/.tauri");
  if (existsSync(tauriTools)) assert.ok(dirs.includes(tauriTools));
});

test("Hermes and /usr/local/bin node hijacks are rejected", async () => {
  assert.equal(isUnsafePackagingDir("/usr/local/bin"), true);
  const scratch = await mkdtemp(
    path.join(tmpdir(), "shadowcode-packaging-path-"),
  );
  try {
    await symlink("/root/.hermes/node/bin/node", path.join(scratch, "node"));
    assert.equal(isUnsafePackagingDir(scratch), true);
    const dirs = packagingDirs(root, { execDir: scratch });
    assert.ok(!dirs.includes(path.resolve(scratch)));
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
});

test("Rustup Cargo home survives PATH cleanup and still rejects node hijacks", async () => {
  const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-rustup-path-"));
  const cargoHome = path.join(scratch, "custom-cargo");
  const bin = path.join(cargoHome, "bin");
  await mkdir(bin, { recursive: true });
  try {
    for (const name of ["cargo", "rustc"]) {
      await writeFile(
        path.join(bin, name),
        `#!/bin/sh\nprintf '${name} rustup-fixture\\n'\n`,
        { mode: 0o755 },
      );
    }
    const next = packagingPath(scratch, { cargoHome });
    assert.ok(next.split(":").includes(bin));
    for (const name of ["cargo", "rustc"]) {
      const result = await exec(name, ["--version"], {
        env: { ...process.env, PATH: next },
      });
      assert.equal(result.stdout.trim(), `${name} rustup-fixture`);
    }
    await symlink("/root/.hermes/node/bin/node", path.join(bin, "node"));
    assert.ok(!packagingDirs(scratch, { cargoHome }).includes(bin));
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
});

test("packaging PATH retains the real compiler and Cargo", async () => {
  const env = { ...process.env, PATH: packagingPath(root) };
  const compiler = await exec("rustc", ["--version", "--verbose"], { env });
  const cargo = await exec("cargo", ["--version"], { env });
  assert.match(compiler.stdout, /^rustc /);
  assert.match(cargo.stdout, /^cargo /);
});

test("applyPackagingPath overwrites a dirty process PATH", () => {
  const previous = process.env.PATH;
  process.env.PATH = dirtyPath;
  try {
    const next = applyPackagingPath(root);
    assert.equal(process.env.PATH, next);
    assert.doesNotMatch(next, /(^|:)\/usr\/local\/bin(:|$)/);
    assert.match(next, /(^|:)\/usr\/bin(:|$)/);
    assert.notEqual(next, dirtyPath);
  } finally {
    process.env.PATH = previous;
  }
});

test("linuxdeploy plugin scan survives a dirty caller PATH", async (t) => {
  if (!existsSync(linuxdeploy)) {
    t.skip("linuxdeploy AppImage is not cached under target/.tauri");
    return;
  }
  const hijackPresent =
    existsSync(hermesNode) &&
    (() => {
      try {
        return readlinkSync(hermesNode).includes(".hermes");
      } catch {
        return false;
      }
    })();
  if (hijackPresent) {
    await assert.rejects(
      exec(linuxdeploy, ["--list-plugins"], {
        env: {
          ...process.env,
          PATH: dirtyPath,
          APPIMAGE_EXTRACT_AND_RUN: "1",
        },
        encoding: "utf8",
        timeout: 30000,
      }),
      (error) => {
        const text = `${error.stderr || ""}${error.stdout || ""}${error.message}`;
        assert.match(
          text,
          /Permission denied.*\/usr\/local\/bin\/node|\/usr\/local\/bin\/node/,
        );
        return true;
      },
    );
  }
  const previous = process.env.PATH;
  process.env.PATH = dirtyPath;
  try {
    const sanitized = applyPackagingPath(root);
    assert.doesNotMatch(sanitized, /(^|:)\/usr\/local\/bin(:|$)/);
    const result = await exec(linuxdeploy, ["--list-plugins"], {
      env: {
        ...process.env,
        PATH: sanitized,
        APPIMAGE_EXTRACT_AND_RUN: "1",
      },
      encoding: "utf8",
      timeout: 30000,
    });
    const text = `${result.stdout}${result.stderr}`;
    assert.match(text, /Available plugins/);
    assert.doesNotMatch(text, /Permission denied/);
  } finally {
    process.env.PATH = previous;
  }
});

test("the AppImage bundle uses the pinned runtime, not upstream's newest", () => {
  // Upstream's "continuous" type2-runtime moved from 75849dc, which made
  // build-native.mjs stop at its runtime check.
  assert.deepEqual(
    bundleEnvironment("appimage", "/work/target/native-runtime/runtime-x86_64"),
    { LDAI_RUNTIME_FILE: "/work/target/native-runtime/runtime-x86_64" },
  );
  assert.deepEqual(bundleEnvironment("deb", undefined), {});
  assert.throws(() => bundleEnvironment("appimage", "runtime-x86_64"));
  assert.throws(() => bundleEnvironment("appimage", ""));
});
