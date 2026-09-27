// The managed llama.cpp runtime (packaging/llama.cpp/bin) as shipped in both
// packages under usr/lib/shadowcode. Build-time only; no Node code is shipped.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import {
  chmod,
  cp,
  lstat,
  readFile,
  readdir,
  readlink,
  realpath,
  rename,
  rm,
  stat,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { normalizeDesktopEntry } from "./native-desktop-metadata.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
export const RUNTIME_LOCATION = "usr/lib/shadowcode";
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");

/** Parse a key=value file such as COMMIT or tools/llama.cpp.pin. */
export function parseFields(text) {
  const fields = {};
  for (const line of text.split("\n")) {
    const at = line.indexOf("=");
    if (at > 0) fields[line.slice(0, at).trim()] = line.slice(at + 1).trim();
  }
  return fields;
}

/**
 * Every entry below `directory` (relative paths), with the problems that make
 * a copied runtime unusable elsewhere: absolute symlinks, symlinks leaving the
 * directory, and dangling symlinks.
 */
export async function inspectRuntimeTree(directory) {
  const entries = [];
  const problems = [];
  const base = path.resolve(directory);
  async function walk(relative) {
    for (const entry of await readdir(path.join(base, relative), {
      withFileTypes: true,
    })) {
      const file = path.join(relative, entry.name);
      const absolute = path.join(base, file);
      if (entry.isDirectory()) {
        entries.push({ file, type: "directory" });
        await walk(file);
      } else if (entry.isSymbolicLink()) {
        const target = await readlink(absolute);
        entries.push({ file, type: "symlink", target });
        if (path.isAbsolute(target)) {
          problems.push(`${file} is an absolute symlink to ${target}`);
          continue;
        }
        const resolved = path.resolve(path.dirname(absolute), target);
        if (!resolved.startsWith(`${base}${path.sep}`)) {
          problems.push(`${file} points outside the runtime (${target})`);
          continue;
        }
        if (!(await stat(absolute).catch(() => null))) {
          problems.push(`${file} is a dangling symlink to ${target}`);
        }
      } else if (entry.isFile()) {
        const info = await lstat(absolute);
        entries.push({ file, type: "file", mode: info.mode & 0o777 });
      } else {
        problems.push(`${file} is not a regular file, directory or symlink`);
      }
    }
  }
  await walk("");
  return { entries, problems };
}

/**
 * The runtime to package, validated against tools/llama.cpp.pin and the
 * pinned notices in licenses/native/sources.json.
 */
export async function readManagedRuntime(checkout = root) {
  const directory = path.join(checkout, "packaging/llama.cpp/bin");
  const missing = (what) =>
    new Error(
      `Managed llama.cpp is ${what}. Run scripts/build-llama.cpp.sh --no-user-install before packaging.`,
    );
  const server = await stat(path.join(directory, "llama-server")).catch(
    () => null,
  );
  if (!server?.isFile()) throw missing("missing");
  const commitText = await readFile(
    path.join(directory, "COMMIT"),
    "utf8",
  ).catch(() => null);
  if (!commitText) throw missing("missing its COMMIT file");
  const built = parseFields(commitText);
  const pin = parseFields(
    await readFile(path.join(checkout, "tools/llama.cpp.pin"), "utf8"),
  );
  assert.match(built.commit || "", /^[0-9a-f]{40}$/, "COMMIT has no commit");
  assert.equal(
    built.commit,
    pin.commit,
    `The runtime was built from llama.cpp ${built.commit}, but tools/llama.cpp.pin pins ${pin.commit}. Rebuild it.`,
  );
  const vulkan = /vulkan/.test(built.backend || "");
  // Release packages must carry the GPU module; a CPU-only runtime happens
  // silently when glslc or the Vulkan headers were missing at build time.
  if (!vulkan && process.env.SHADOWCODE_ALLOW_CPU_ONLY_RUNTIME !== "1") {
    throw new Error(
      `The llama.cpp runtime in ${directory} was built without the Vulkan module (backend=${built.backend || "unknown"}). Install glslc and libvulkan-dev and rebuild it, or set SHADOWCODE_ALLOW_CPU_ONLY_RUNTIME=1 to package a CPU-only runtime on purpose.`,
    );
  }
  if (vulkan) {
    assert.match(
      pin.spirv_headers_commit || "",
      /^[0-9a-f]{40}$/,
      "tools/llama.cpp.pin must pin spirv_headers_commit",
    );
    assert.equal(
      built.spirv_headers_commit,
      pin.spirv_headers_commit,
      "The Vulkan module was not built from the pinned SPIRV-Headers commit. Rebuild the runtime.",
    );
  }
  const architectures = await readFile(
    path.join(directory, "architectures.txt"),
    "utf8",
  ).catch(() => "");
  assert.ok(
    architectures.trim().split("\n").length > 10,
    "architectures.txt is missing from the managed runtime",
  );
  const { problems } = await inspectRuntimeTree(directory);
  assert.deepEqual(problems, [], "The managed runtime has broken symlinks");

  const components = [
    {
      name: "llama.cpp",
      version: built.commit,
      license: "MIT",
      source: `${built.url || "https://github.com/ggml-org/llama.cpp.git"}#${built.commit}`,
    },
  ];
  if (vulkan)
    components.push({
      name: "SPIRV-Headers",
      version: built.spirv_headers_commit,
      license: "MIT",
      source: `https://github.com/KhronosGroup/SPIRV-Headers.git#${built.spirv_headers_commit}`,
    });
  const vendor = path.join(checkout, "licenses/native");
  const manifest = JSON.parse(
    await readFile(path.join(vendor, "sources.json"), "utf8"),
  );
  for (const component of components) {
    const id = `${component.name}@${component.version}`;
    component.notices = manifest.files.filter((file) =>
      file.packages.includes(id),
    );
    assert.ok(
      component.notices.length,
      `No pinned notices for ${id} in licenses/native/sources.json`,
    );
    for (const notice of component.notices) {
      assert.ok(notice.runtime, `${notice.file} has no runtime path`);
      const shipped = await readFile(
        path.join(directory, notice.runtime),
      ).catch(() => null);
      assert.ok(
        shipped,
        `The runtime lacks ${notice.runtime}; run scripts/build-llama.cpp.sh --notices-only`,
      );
      assert.equal(
        sha256(shipped),
        notice.sha256,
        `${notice.runtime} differs from licenses/native/${notice.file}`,
      );
    }
  }
  return {
    directory,
    commit: built.commit,
    backend: built.backend || "unknown",
    spirvHeadersCommit: built.spirv_headers_commit || null,
    components,
  };
}

/**
 * Copy the runtime into `destination` (a package's usr/lib/shadowcode),
 * keeping relative symlinks verbatim. Node's fs.cp rewrites relative symlink
 * targets to absolute build-tree paths unless verbatimSymlinks is set.
 */
export async function copyManagedRuntime(runtime, destination) {
  await rm(destination, { recursive: true, force: true });
  await cp(runtime.directory, destination, {
    recursive: true,
    verbatimSymlinks: true,
  });
  const { entries, problems } = await inspectRuntimeTree(destination);
  assert.deepEqual(problems, [], "Copied runtime has broken symlinks");
  await chmod(destination, 0o755);
  for (const entry of entries) {
    const absolute = path.join(destination, entry.file);
    if (entry.type === "directory") await chmod(absolute, 0o755);
    else if (entry.type === "file")
      await chmod(absolute, entry.mode & 0o111 ? 0o755 : 0o644);
  }
  return entries;
}

// Libraries the runtime may take from the host. Everything else must resolve
// inside usr/lib/shadowcode through its $ORIGIN rpath.
export const ALLOWED_SYSTEM_LIBRARIES = new Set([
  "linux-vdso.so.1",
  "ld-linux-x86-64.so.2",
  "libc.so.6",
  "libm.so.6",
  "libdl.so.2",
  "libpthread.so.0",
  "librt.so.1",
  "libstdc++.so.6",
  "libgcc_s.so.1",
  "libgomp.so.1",
  "libvulkan.so.1",
  "libssl.so.3",
  "libcrypto.so.3",
]);

async function isElf(file) {
  const bytes = await readFile(file).catch(() => null);
  return bytes?.subarray(0, 4).toString("latin1") === "\x7fELF";
}

/**
 * Verify an installed/extracted runtime directory the way a user machine would
 * load it: relative symlinks only, COMMIT matching the pin, notices present,
 * every NEEDED library inside the directory or an allowed system library, and
 * `llama-server --version` printing the pinned commit with LD_LIBRARY_PATH
 * unset. Returns a report; throws on the first failure.
 */
export async function verifyRuntimeDirectory(directory, pin, { run }) {
  const { entries, problems } = await inspectRuntimeTree(directory);
  assert.deepEqual(problems, [], `${directory}: broken symlinks`);
  const names = new Set(entries.map((entry) => entry.file));
  for (const required of [
    "llama-server",
    "COMMIT",
    "architectures.txt",
    "NOTICES/llama.cpp-LICENSE",
  ])
    assert.ok(names.has(required), `${directory} lacks ${required}`);
  const commit = parseFields(
    await readFile(path.join(directory, "COMMIT"), "utf8"),
  );
  assert.equal(
    commit.commit,
    pin.commit,
    "Runtime COMMIT differs from the pin",
  );
  if (/vulkan/.test(commit.backend || "")) {
    assert.equal(
      commit.spirv_headers_commit,
      pin.spirv_headers_commit,
      "Runtime SPIRV-Headers commit differs from the pin",
    );
    assert.ok(names.has("libggml-vulkan.so"), "Vulkan module missing");
    assert.ok(
      names.has("NOTICES/SPIRV-Headers-LICENSE"),
      "SPIRV-Headers notice missing",
    );
  }
  assert.match(
    await readFile(path.join(directory, "NOTICES/llama.cpp-LICENSE"), "utf8"),
    /MIT License[\s\S]*ggml authors/,
    "llama.cpp MIT notice missing",
  );
  const architectures = (
    await readFile(path.join(directory, "architectures.txt"), "utf8")
  )
    .trim()
    .split("\n");
  assert.ok(architectures.length > 10, "architectures.txt is empty");

  // A clean environment: no LD_LIBRARY_PATH, no AppImage variables.
  const env = { PATH: "/usr/bin:/bin", HOME: process.env.HOME || "/tmp" };
  const base = path.resolve(directory);
  const realBase = await realpath(base);
  const needed = {};
  for (const entry of entries.filter((entry) => entry.type === "file")) {
    const file = path.join(directory, entry.file);
    if (!(await isElf(file))) continue;
    const dynamic = (await run("readelf", ["-d", file], { env })).stdout;
    const libraries = [
      ...dynamic.matchAll(/\(NEEDED\)\s+Shared library: \[([^\]]+)\]/g),
    ].map((match) => match[1]);
    const runpath =
      /\((?:RUNPATH|RPATH)\)\s+Library r(?:un)?path: \[([^\]]+)\]/.exec(
        dynamic,
      )?.[1];
    const inside = libraries.filter((library) => names.has(library));
    if (inside.length)
      assert.equal(
        runpath,
        "$ORIGIN",
        `${entry.file} needs bundled libraries but its runpath is ${runpath}`,
      );
    const foreign = libraries.filter(
      (library) =>
        !names.has(library) && !ALLOWED_SYSTEM_LIBRARIES.has(library),
    );
    assert.deepEqual(
      foreign,
      [],
      `${entry.file} needs libraries that are neither bundled nor allowed system libraries`,
    );
    const resolved = (await run("ldd", [file], { env })).stdout;
    assert.equal(
      /not found/.test(resolved),
      false,
      `${entry.file} has unresolved libraries:\n${resolved}`,
    );
    // Bundled libraries must load from this directory, not from the build
    // tree or another installed runtime.
    for (const [, library, location] of resolved.matchAll(
      /^\s*(\S+) => (\/\S+)/gm,
    )) {
      if (!names.has(library)) continue;
      assert.ok(
        [base, realBase].some((prefix) =>
          location.startsWith(`${prefix}${path.sep}`),
        ),
        `${entry.file} loads ${library} from ${location}, outside ${directory}`,
      );
    }
    needed[entry.file] = libraries;
  }
  const server = path.join(directory, "llama-server");
  const version = await run(server, ["--version"], { env, cwd: "/" });
  const versionText = `${version.stdout}${version.stderr}`;
  const reported = /commit ([0-9a-f]{7,40})/.exec(versionText)?.[1];
  assert.ok(
    reported && pin.commit.startsWith(reported),
    `llama-server --version did not report the pinned commit:\n${versionText}`,
  );
  const devices = await run(server, ["--list-devices"], { env, cwd: "/" });
  return {
    commit: commit.commit,
    backend: commit.backend,
    spirvHeadersCommit: commit.spirv_headers_commit || null,
    files: entries.filter((entry) => entry.type === "file").length,
    symlinks: entries.filter((entry) => entry.type === "symlink").length,
    architectures: architectures.length,
    version: versionText
      .trim()
      .split("\n")
      .filter((line) => /^version:|^built with/.test(line)),
    devices: `${devices.stdout}${devices.stderr}`.trim().split("\n").slice(-6),
    needed,
  };
}

/**
 * Add the runtime to a built .deb as /usr/lib/shadowcode with its relative
 * symlinks (the Tauri bundler's custom-files copy would dereference them),
 * regenerating md5sums and Installed-Size. Replaces `deb` atomically.
 */
export async function addRuntimeToDeb(
  deb,
  runtime,
  scratch,
  { run, normalizeDesktop = false },
) {
  const work = path.join(scratch, "deb-root");
  await rm(work, { recursive: true, force: true });
  await run("dpkg-deb", ["--raw-extract", deb, work]);
  if (normalizeDesktop) await normalizeDesktopEntry(work);
  await copyManagedRuntime(runtime, path.join(work, RUNTIME_LOCATION));
  const sums = [];
  let kibibytes = 0;
  const { entries } = await inspectRuntimeTree(work);
  for (const entry of entries) {
    if (entry.file === "DEBIAN" || entry.file.startsWith(`DEBIAN${path.sep}`))
      continue;
    if (entry.type === "file") {
      const bytes = await readFile(path.join(work, entry.file));
      sums.push(
        `${createHash("md5").update(bytes).digest("hex")}  ${entry.file}`,
      );
      kibibytes += Math.ceil(bytes.length / 1024);
    } else kibibytes += 1;
  }
  await writeFile(
    path.join(work, "DEBIAN/md5sums"),
    `${sums.sort((a, b) => a.slice(34).localeCompare(b.slice(34))).join("\n")}\n`,
  );
  const controlPath = path.join(work, "DEBIAN/control");
  const control = await readFile(controlPath, "utf8");
  assert.match(
    control,
    /^Installed-Size: \d+$/m,
    "deb control lacks Installed-Size",
  );
  await writeFile(
    controlPath,
    control.replace(/^Installed-Size: \d+$/m, `Installed-Size: ${kibibytes}`),
  );
  const pending = path.join(scratch, path.basename(deb));
  await run("dpkg-deb", [
    "--root-owner-group",
    "-Zxz",
    "--build",
    work,
    pending,
  ]);
  await rename(pending, deb);
  await rm(work, { recursive: true, force: true });
}
