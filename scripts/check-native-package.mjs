// Build-time package verification; Node is not included in either distribution.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import {
  mkdtemp,
  readFile,
  readdir,
  rm,
  stat,
  mkdir,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { promisify } from "node:util";
import { fileURLToPath } from "node:url";
import {
  RUNTIME_LOCATION,
  parseFields,
  verifyRuntimeDirectory,
} from "./llama-runtime.mjs";
import { DESKTOP_FILE, METAINFO_FILE } from "./native-desktop-metadata.mjs";
const run = promisify(execFile);
const [appimagePath, debPath] = process.argv
  .slice(2)
  .map((p) => path.resolve(p));
assert.ok(
  appimagePath && debPath,
  "Usage: node scripts/check-native-package.mjs APPIMAGE DEB",
);
const artifacts = path.resolve(
  process.env.SHADOW_PACKAGE_ARTIFACTS || "artifacts/native-package",
);
await mkdir(artifacts, { recursive: true });
await rm(path.join(artifacts, "package.json"), { force: true });
const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-package-"));
const options = { timeout: 120000, maxBuffer: 16000000 };
const runWith = (binary, args, extra = {}) =>
  run(binary, args, { ...options, ...extra });
const pin = parseFields(
  await readFile(
    fileURLToPath(new URL("../tools/llama.cpp.pin", import.meta.url)),
    "utf8",
  ),
);
const metainfoName = METAINFO_FILE;
const metainfoSource = fileURLToPath(
  new URL(`../packaging/${metainfoName}`, import.meta.url),
);
const digest = async (file) =>
  createHash("sha256")
    .update(await readFile(file))
    .digest("hex");
async function inspectTree(directory) {
  const files = [];
  for (const entry of await readdir(directory, { withFileTypes: true })) {
    const file = path.join(directory, entry.name);
    if (entry.isDirectory()) files.push(...(await inspectTree(file)));
    else if (entry.isFile() || entry.isSymbolicLink()) files.push(file);
    // Do not follow bundle symlinks outside the extracted tree.
  }
  return files;
}
async function verifyNotices(directory, includeSystem) {
  const base = path.join(directory, "usr/share/doc/shadowcode/notices");
  const application = JSON.parse(
    await readFile(path.join(base, "application.json"), "utf8"),
  );
  assert.equal(application.schema, 1);
  for (const ecosystem of ["cargo", "npm", "rust-toolchain"]) {
    assert.ok(
      application.packages.some((pkg) => pkg.ecosystem === ecosystem),
      `Missing ${ecosystem} notices`,
    );
  }
  const llama = application.packages.find(
    (pkg) => pkg.ecosystem === "managed-runtime" && pkg.name === "llama.cpp",
  );
  assert.equal(
    llama?.version,
    pin.commit,
    "The notices must attribute the bundled runtime to the pinned llama.cpp",
  );
  assert.ok(
    llama.notices.some((notice) => notice.file.endsWith("/llama.cpp-LICENSE")),
    "The llama.cpp MIT notice must be shipped",
  );
  assert.ok(
    application.projectNotice?.file === "ShadowCode-NOTICE",
    "ShadowCode's NOTICE must be shipped with its license",
  );
  const files = [application.projectLicense, application.projectNotice];
  for (const pkg of application.packages) {
    assert.ok(
      pkg.name && pkg.version && pkg.notices.length,
      "Incomplete application attribution",
    );
    files.push(...pkg.notices);
  }
  let systemPackages = 0;
  if (includeSystem) {
    const system = JSON.parse(
      await readFile(path.join(base, "system.json"), "utf8"),
    );
    assert.equal(system.schema, 1);
    systemPackages = system.packages.length;
    assert.ok(
      systemPackages > 0 &&
        system.helpers.length > 0 &&
        system.commonLicenses.length > 0,
    );
    for (const pkg of system.packages) {
      assert.ok(
        pkg.name &&
          pkg.version &&
          pkg.source &&
          pkg.sourceVersion &&
          pkg.files.length &&
          pkg.notices.length,
        "Incomplete system attribution",
      );
      files.push(...pkg.notices);
    }
    files.push(...system.helpers, ...system.commonLicenses);
  }
  for (const file of files) {
    const absolute = path.resolve(base, file.file);
    assert.ok(
      absolute.startsWith(`${base}${path.sep}`),
      "Notice path must stay in the package",
    );
    assert.equal(
      await digest(absolute),
      file.sha256,
      `Missing or changed notice: ${file.file}`,
    );
  }
  return {
    applicationPackages: application.packages.length,
    systemPackages,
    noticeFiles: new Set(files.map((file) => file.file)).size,
  };
}
async function verifyMetainfo(directory) {
  const metainfo = path.join(directory, "usr/share/metainfo", metainfoName);
  assert.equal(
    await digest(metainfo),
    await digest(metainfoSource),
    "The package must ship the reviewed AppStream metadata unchanged",
  );
  const desktop = await readFile(
    path.join(directory, "usr/share/applications", DESKTOP_FILE),
    "utf8",
  );
  assert.match(desktop, /^Name=ShadowCode$/m);
  assert.match(desktop, /^Type=Application$/m);
  await run("appstreamcli", ["validate", "--no-net", metainfo], options);
}
try {
  const version = (
    await run(
      appimagePath,
      ["--appimage-extract-and-run", "--version"],
      options,
    )
  ).stdout.trim();
  assert.match(version, /^ShadowCode \d+\.\d+\.\d+$/);
  await run(appimagePath, ["--appimage-extract"], { ...options, cwd: scratch });
  const appdir = path.join(scratch, "squashfs-root");
  await verifyMetainfo(appdir);
  assert.equal(
    await digest(path.join(appdir, "AppRun")),
    await digest(
      fileURLToPath(new URL("../packaging/native-app-run.sh", import.meta.url)),
    ),
    "The package must use the launcher that preserves caller paths and external language runtimes",
  );
  const appimageNotices = await verifyNotices(appdir, true);
  // The runtime as a user machine loads it: relative links only, the pinned
  // commit, bundled libraries resolved inside the directory, LD_LIBRARY_PATH
  // unset.
  const appimageRuntime = await verifyRuntimeDirectory(
    path.join(appdir, RUNTIME_LOCATION),
    pin,
    { run: runWith },
  );
  const runtimeBase = path.join(
    appdir,
    "usr/share/doc/shadowcode/notices/runtime",
  );
  const runtime = JSON.parse(
    await readFile(path.join(runtimeBase, "runtime.json"), "utf8"),
  );
  assert.equal(runtime.schema, 1);
  assert.equal(runtime.patchset, "isolated-extraction-v2");
  const runtimeVersion = await run(
    appimagePath,
    ["--appimage-version"],
    options,
  );
  assert.ok(
    (runtimeVersion.stdout + runtimeVersion.stderr).includes(
      `ShadowCode runtime patchset: ${runtime.patchset}`,
    ),
  );
  assert.ok(
    runtime.files.length > 15,
    "Runtime notices and provenance must be present",
  );
  for (const file of runtime.files) {
    const absolute = path.resolve(runtimeBase, file.file);
    assert.ok(absolute.startsWith(`${runtimeBase}${path.sep}`));
    assert.equal(
      await digest(absolute),
      file.sha256,
      `Runtime notice changed: ${file.file}`,
    );
  }
  await run(
    "objcopy",
    [
      "--dump-section",
      `.text=${path.join(scratch, "runtime.text")}`,
      appimagePath,
      path.join(scratch, "runtime.elf"),
    ],
    options,
  );
  assert.equal(
    await digest(path.join(scratch, "runtime.text")),
    runtime.textSha256,
    "Final AppImage must contain the source-built patched runtime",
  );
  const sourcesPath = path.join(
    path.dirname(appimagePath),
    `ShadowCode_${version.split(" ")[1]}_appimage-runtime-sources.tar.gz`,
  );
  const sources = path.join(scratch, "runtime-sources");
  await mkdir(sources);
  await run("tar", ["-xzf", sourcesPath, "-C", sources], options);
  const sourceReceipt = JSON.parse(
    await readFile(path.join(sources, "receipt.json"), "utf8"),
  );
  assert.equal(sourceReceipt.inputHash, runtime.inputHash);
  assert.equal(sourceReceipt.sha256, runtime.sha256);
  for (const file of sourceReceipt.files.filter(
    (file) =>
      file.file.startsWith("sources/") || !file.file.startsWith("notices/"),
  )) {
    const absolute = path.resolve(sources, file.file);
    assert.ok(absolute.startsWith(`${sources}${path.sep}`));
    assert.equal(
      await digest(absolute),
      file.sha256,
      `Runtime source artifact changed: ${file.file}`,
    );
  }
  const files = await inspectTree(appdir);
  const python = files.filter((file) =>
    /^(?:python[\d.]*|libpython.*|.*\.py[co]?)$/i.test(path.basename(file)),
  );
  assert.deepEqual(
    python,
    [],
    "The native package must not contain Python interpreters, libraries, or sidecars",
  );
  const executable = path.join(appdir, "usr/bin/shadowcode");
  assert.equal(
    (await readFile(executable)).subarray(0, 4).toString(),
    "\x7fELF",
  );
  assert.equal(
    (await run(executable, ["ui", "--version"], options)).stdout.trim(),
    version,
  );
  const dependencies = (await run("ldd", [executable], options)).stdout;
  assert.equal(
    dependencies.includes("not found"),
    false,
    "Native dependencies must resolve on the build host",
  );
  assert.equal(/libpython/i.test(dependencies), false);
  const deb = path.join(scratch, "deb");
  await run("dpkg-deb", ["--extract", debPath, deb], options);
  await verifyMetainfo(deb);
  const debNotices = await verifyNotices(deb, false);
  const debRuntime = await verifyRuntimeDirectory(
    path.join(deb, RUNTIME_LOCATION),
    pin,
    { run: runWith },
  );
  const debDependencies = (
    await run("dpkg-deb", ["--field", debPath, "Depends"], options)
  ).stdout.trim();
  for (const dependency of ["libgomp1", "libssl3"])
    assert.ok(
      debDependencies
        .split(/,\s*/)
        .some((entry) => entry.split(" ")[0] === dependency),
      `The deb must depend on ${dependency} for the llama.cpp runtime`,
    );
  // Voice input records through ALSA (libasound.so.2, linked by cpal).
  assert.ok(
    debDependencies
      .split(/,\s*/)
      .some((entry) => entry.split(" ")[0] === "libasound2"),
    "The deb must depend on libasound2 for voice input",
  );
  assert.deepEqual(
    (await inspectTree(deb)).filter((file) =>
      /^(?:python[\d.]*|libpython.*|.*\.py[co]?)$/i.test(path.basename(file)),
    ),
    [],
    "The Debian package must not contain Python runtimes or sidecars",
  );
  const debExecutable = path.join(deb, "usr/bin/shadowcode");
  assert.equal(
    (await readFile(debExecutable)).subarray(0, 4).toString(),
    "\x7fELF",
  );
  assert.equal(
    (await run(debExecutable, ["--version"], options)).stdout.trim(),
    version,
  );
  // Tauri patches the bundle type and linuxdeploy may change ELF rpaths, so
  // comparing entire executable hashes across formats would reject valid builds.
  await run(
    "objcopy",
    [
      "--dump-section",
      `.text=${path.join(scratch, "appimage.text")}`,
      executable,
    ],
    options,
  );
  await run(
    "objcopy",
    [
      "--dump-section",
      `.text=${path.join(scratch, "deb.text")}`,
      debExecutable,
    ],
    options,
  );
  assert.equal(
    await digest(path.join(scratch, "appimage.text")),
    await digest(path.join(scratch, "deb.text")),
    "Both packages must contain the same compiled application code",
  );
  const debVersion = (
    await run("dpkg-deb", ["--field", debPath, "Version"], options)
  ).stdout.trim();
  assert.equal(debVersion, version.split(" ")[1]);
  const report = {
    passed: true,
    version,
    checkedAt: new Date().toISOString(),
    executableBytes: (await stat(executable)).size,
    bundleFiles: files.length,
    checks: [
      "FUSE-free AppImage version",
      "caller-preserving native AppRun launcher",
      "ELF executable",
      "legacy ui launcher compatibility",
      "no Python runtime or sidecars",
      "host dependency resolution",
      "matching compiled code and versions in AppImage and Debian packages",
      "versioned dependency inventories and SHA-256 verification of every notice",
      "validated matching AppStream metadata and desktop launcher in both packages",
      "patched AppImage runtime machine code, notices, and matching source archive",
      "managed llama.cpp in usr/lib/shadowcode of both packages: pinned commit, relative symlinks, bundled libraries resolved inside the directory, llama-server --version with LD_LIBRARY_PATH unset, llama.cpp MIT notice",
    ],
    packages: await Promise.all(
      [appimagePath, debPath, sourcesPath].map(async (file) => ({
        file: path.basename(file),
        bytes: (await stat(file)).size,
        sha256: await digest(file),
      })),
    ),
    debDependencies,
    managedRuntime: { appimage: appimageRuntime, deb: debRuntime },
    maintainer: (
      await run("dpkg-deb", ["--field", debPath, "Maintainer"], options)
    ).stdout.trim(),
    notices: {
      appimage: appimageNotices,
      deb: debNotices,
      runtime: { patchset: runtime.patchset, files: runtime.files.length },
    },
  };
  await writeFile(
    path.join(artifacts, "package.json"),
    JSON.stringify(report, null, 2),
  );
  await writeFile(
    path.join(artifacts, "SHA256SUMS"),
    report.packages.map((file) => `${file.sha256}  ${file.file}\n`).join(""),
  );
  console.log(JSON.stringify(report, null, 2));
} finally {
  await rm(scratch, { recursive: true, force: true });
}
