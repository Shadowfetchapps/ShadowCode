// Build-time attribution. No Node code or package manager is shipped in the app.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import {
  copyFile,
  mkdir,
  readFile,
  readdir,
  rm,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { RUNTIME_LOCATION } from "./llama-runtime.mjs";
import {
  GLIB_DIRECTORY,
  GLIB_PROVENANCE,
  GLIB_README,
  requireGlibBackport,
  verifyGlibBackport,
} from "./native-glib-backport.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));
const run = promisify(execFile);
const options = { cwd: root, timeout: 120000, maxBuffer: 32000000 };
const sha256 = (bytes) => createHash("sha256").update(bytes).digest("hex");
const json = async (file) => JSON.parse(await readFile(file, "utf8"));
const noticeName =
  /^(?:licen[sc]e[s]?|copying|copyright|notice[s]?|authors)(?:[._-]|$)/i;

async function walk(directory, relative = "") {
  const files = [];
  for (const entry of await readdir(path.join(directory, relative), {
    withFileTypes: true,
  })) {
    const file = path.join(relative, entry.name);
    if (
      entry.isDirectory() &&
      ![".git", "node_modules", "target"].includes(entry.name)
    ) {
      files.push(...(await walk(directory, file)));
    } else if (entry.isFile()) files.push(file);
  }
  return files.sort();
}

async function copyNotice(source, destination, relative) {
  const bytes = await readFile(source);
  assert.ok(bytes.length, `Empty notice: ${source}`);
  await mkdir(path.dirname(path.join(destination, relative)), {
    recursive: true,
  });
  await copyFile(source, path.join(destination, relative));
  return { file: relative, sha256: sha256(bytes) };
}

async function upstreamNotices() {
  const vendor = path.join(root, "licenses/native");
  const manifest = await json(path.join(vendor, "sources.json"));
  for (const file of manifest.files) {
    assert.equal(
      sha256(await readFile(path.join(vendor, file.file))),
      file.sha256,
      `Modified upstream notice: ${file.file}`,
    );
  }
  return { vendor, files: manifest.files };
}

export async function glibBackportNotice(metadata, destination, checkout = root) {
  const pkg = requireGlibBackport(metadata, checkout);
  const verified = await verifyGlibBackport(checkout);
  const prefix = `rust/${pkg.name}-${pkg.version}`;
  return {
    ecosystem: "cargo",
    name: pkg.name,
    version: pkg.version,
    license: "MIT",
    source: verified.source,
    sourceSha256: verified.sourceSha256,
    sourceKind: "vendored-backport",
    localPath: GLIB_DIRECTORY,
    sourceTreeSha256: verified.sourceTreeSha256,
    modifications: [{
      advisory: verified.advisory,
      upstreamCommit: verified.patchCommit,
      source: `https://github.com/gtk-rs/gtk-rs-core/commit/${verified.patchCommit}`,
      file: verified.patchedFile,
      sha256: verified.patchedSha256,
    }],
    provenance: await copyNotice(path.join(checkout, GLIB_PROVENANCE), destination, `${prefix}/backport-provenance.json`),
    notices: [
      await copyNotice(path.join(checkout, GLIB_DIRECTORY, "LICENSE"), destination, `${prefix}/LICENSE`),
      await copyNotice(path.join(checkout, GLIB_DIRECTORY, "COPYRIGHT"), destination, `${prefix}/COPYRIGHT`),
      await copyNotice(path.join(checkout, GLIB_README), destination, `${prefix}/BACKPORT.md`),
    ],
  };
}

export async function applicationNotices(destination, managedRuntime = null) {
  const upstream = await upstreamNotices();
  const compiler = (await run("rustc", ["--version", "--verbose"], options))
    .stdout;
  const target = /^host: (.+)$/m.exec(compiler)?.[1];
  const version = /^release: (.+)$/m.exec(compiler)?.[1];
  assert.ok(target && version, "Cannot identify the Rust toolchain");
  assert.ok(
    upstream.files.some((f) =>
      f.packages.includes(`rust-standard-library@${version}`),
    ),
    `Add the standard-library notices before changing Rust ${version}`,
  );
  const metadata = JSON.parse(
    (
      await run(
        "cargo",
        [
          "metadata",
          "--locked",
          "--filter-platform",
          target,
          "--format-version",
          "1",
        ],
        options,
      )
    ).stdout,
  );
  const resolved = new Set(metadata.resolve.nodes.map((node) => node.id));
  const lockedChecksums = new Map(
    (await readFile(path.join(root, "Cargo.lock"), "utf8"))
      .split("[[package]]")
      .slice(1)
      .map((entry) => [
        `${/^name = "([^"]+)"/m.exec(entry)?.[1]}@${/^version = "([^"]+)"/m.exec(entry)?.[1]}`,
        /^checksum = "([a-f0-9]{64})"/m.exec(entry)?.[1],
      ]),
  );
  await rm(destination, { recursive: true, force: true });
  await mkdir(destination, { recursive: true });
  const packages = [];

  // Linux's GTK/WebKit graph requires this third-party path crate. Treat a
  // missing/replaced patch as an error, rather than dropping its MIT license.
  if (target.includes("linux") || metadata.packages.some(
    (pkg) => pkg.name === "glib" && resolved.has(pkg.id),
  )) packages.push(await glibBackportNotice(metadata, destination));

  for (const pkg of metadata.packages.filter(
    (pkg) => pkg.source && resolved.has(pkg.id),
  )) {
    const directory = path.dirname(pkg.manifest_path);
    const id = `${pkg.name}@${pkg.version}`;
    const local = (await walk(directory)).filter((f) =>
      f.split(path.sep).some((part) => noticeName.test(part)),
    );
    if (pkg.license_file && !local.includes(pkg.license_file))
      local.push(pkg.license_file);
    const files = [];
    for (const file of local) {
      const absolute = path.resolve(directory, file);
      assert.ok(
        absolute.startsWith(`${directory}${path.sep}`),
        `Notice outside crate: ${id}`,
      );
      files.push(
        await copyNotice(
          absolute,
          destination,
          `rust/${pkg.name}-${pkg.version}/${file}`,
        ),
      );
    }
    for (const file of upstream.files.filter((file) =>
      file.packages.includes(id),
    )) {
      files.push({
        ...(await copyNotice(
          path.join(upstream.vendor, file.file),
          destination,
          `upstream/${file.file}`,
        )),
        origin: file.url,
      });
    }
    assert.ok(
      files.length,
      `Missing license text for ${id}; add a pinned upstream notice`,
    );
    const sourceSha256 = lockedChecksums.get(id);
    assert.ok(
      sourceSha256 &&
        pkg.source === "registry+https://github.com/rust-lang/crates.io-index",
      `No locked registry source checksum for ${id}`,
    );
    packages.push({
      ecosystem: "cargo",
      name: pkg.name,
      version: pkg.version,
      license: pkg.license,
      source: `https://static.crates.io/crates/${pkg.name}/${pkg.name}-${pkg.version}.crate`,
      sourceSha256,
      notices: files,
    });
  }

  const lock = await json(path.join(root, "ui/package-lock.json"));
  for (const [location, entry] of Object.entries(lock.packages).filter(
    ([location, entry]) => location && !entry.dev,
  )) {
    const directory = path.join(root, "ui", location);
    const pkg = await json(path.join(directory, "package.json"));
    assert.equal(
      pkg.version,
      entry.version,
      `Installed npm package differs from lockfile: ${location}`,
    );
    const files = (await walk(directory)).filter((file) =>
      file.split(path.sep).some((part) => noticeName.test(part)),
    );
    assert.ok(
      files.length,
      `Missing license text for ${pkg.name}@${pkg.version}`,
    );
    const notices = [];
    for (const file of files)
      notices.push(
        await copyNotice(
          path.join(directory, file),
          destination,
          `npm/${location}/${file}`,
        ),
      );
    packages.push({
      ecosystem: "npm",
      name: pkg.name,
      version: pkg.version,
      license: entry.license || pkg.license,
      source: entry.resolved,
      integrity: entry.integrity,
      notices,
    });
  }

  const standardLibrary = [];
  for (const file of upstream.files.filter((file) =>
    file.packages.includes(`rust-standard-library@${version}`),
  )) {
    standardLibrary.push({
      ...(await copyNotice(
        path.join(upstream.vendor, file.file),
        destination,
        `upstream/${file.file}`,
      )),
      origin: file.url,
    });
  }
  packages.push({
    ecosystem: "rust-toolchain",
    name: "rust-standard-library",
    version,
    notices: standardLibrary,
  });
  // The managed llama.cpp runtime shipped as usr/lib/shadowcode in both
  // packages; readManagedRuntime already matched its NOTICES to these pins.
  for (const component of managedRuntime?.components || []) {
    const notices = [];
    for (const file of component.notices)
      notices.push({
        ...(await copyNotice(
          path.join(upstream.vendor, file.file),
          destination,
          `upstream/${file.file}`,
        )),
        origin: file.url,
      });
    packages.push({
      ecosystem: "managed-runtime",
      name: component.name,
      version: component.version,
      license: component.license,
      source: component.source,
      location: RUNTIME_LOCATION,
      backend: managedRuntime.backend,
      notices,
    });
  }
  const projectLicense = await copyNotice(
    path.join(root, "LICENSE"),
    destination,
    "ShadowCode-LICENSE",
  );
  // Apache-2.0 section 4(d): the NOTICE travels with every copy.
  const projectNotice = await copyNotice(
    path.join(root, "NOTICE"),
    destination,
    "ShadowCode-NOTICE",
  );
  const manifest = {
    schema: 1,
    scope:
      "Resolved Cargo graph for the build target (including build/test dependencies), production npm dependency graph, Rust standard library, and the managed llama.cpp runtime in usr/lib/shadowcode. This deliberately includes dependencies eliminated by the linker or JavaScript bundler.",
    target,
    projectLicense,
    projectNotice,
    packages,
  };
  await writeFile(
    path.join(destination, "application.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  await writeFile(
    path.join(destination, "README.txt"),
    "ShadowCode dependency notices\n\napplication.json lists application dependency versions, source locations, and notice hashes.\nThe original license and copyright texts are retained in the referenced files.\nAppImage system libraries and helpers have a separate system.json inventory.\nThe application uses dynamic system libraries in Debian packages.\nThe managed llama.cpp runtime in /usr/lib/shadowcode is listed in application.json (ecosystem managed-runtime); its license texts are also in /usr/lib/shadowcode/NOTICES.\nCorresponding-source release artifacts are tracked separately from this notice inventory.\n",
  );
  console.log(
    `Collected notices for ${packages.length} application dependencies (${target})`,
  );
  return manifest;
}

export async function appdirNotices(appdir) {
  const destination = path.join(appdir, "usr/share/doc/shadowcode/notices");
  await mkdir(destination, { recursive: true });
  const upstream = await upstreamNotices();
  const architecture = (
    await run("dpkg", ["--print-architecture"], options)
  ).stdout.trim();
  const installed = new Map();
  const metadata = (
    await run(
      "dpkg-query",
      [
        "-W",
        "-f=${binary:Package}\t${Version}\t${source:Package}\t${source:Version}\t${Architecture}\n",
      ],
      options,
    )
  ).stdout;
  for (const line of metadata.trim().split("\n")) {
    const [name, version, source, sourceVersion, arch] = line.split("\t");
    if (arch === architecture || arch === "all")
      installed.set(name, {
        name,
        version,
        source,
        sourceVersion,
        architecture: arch,
      });
  }
  // Read dpkg's installed file lists once. Hundreds of separate wildcard -S
  // queries repeatedly scan the entire database and make packaging very slow.
  const owners = new Map();
  for (const file of await readdir("/var/lib/dpkg/info")) {
    if (!file.endsWith(".list")) continue;
    const name = file.slice(0, -5);
    if (!installed.has(name)) continue;
    for (const filename of (
      await readFile(`/var/lib/dpkg/info/${file}`, "utf8")
    ).split("\n")) {
      if (!filename.startsWith("/usr/") && !filename.startsWith("/lib/"))
        continue;
      const base = path.basename(filename);
      const entries = owners.get(base) || [];
      entries.push({ name, filename });
      owners.set(base, entries);
    }
  }
  const packages = new Map();
  const generated = [];
  const managedRuntime = [];
  for (const file of await walk(appdir)) {
    if (file.startsWith(`${RUNTIME_LOCATION}/`)) {
      managedRuntime.push(file);
      continue;
    }
    if (
      !file.startsWith("usr/") ||
      file.startsWith("usr/share/doc/") ||
      file.startsWith("usr/share/icons/") ||
      file.startsWith("usr/share/applications/") ||
      file === "usr/bin/shadowcode"
    )
      continue;
    if (
      /\/(?:gschemas\.compiled|loaders\.cache|immodules\.cache)$/.test(file)
    ) {
      generated.push(file);
      continue;
    }
    let candidates = owners.get(path.basename(file)) || [];
    const exact = candidates.filter((entry) => entry.filename === `/${file}`);
    if (exact.length) candidates = exact;
    assert.ok(candidates.length, `Unattributed bundled file: ${file}`);
    for (const owner of candidates) {
      const pkg = packages.get(owner.name) || {
        ...installed.get(owner.name),
        files: [],
        notices: [],
      };
      if (!pkg.files.includes(file)) pkg.files.push(file);
      packages.set(owner.name, pkg);
    }
  }
  for (const pkg of packages.values()) {
    const name = pkg.name.split(":")[0];
    pkg.notices.push(
      await copyNotice(
        `/usr/share/doc/${name}/copyright`,
        destination,
        `system/${name}/copyright`,
      ),
    );
    pkg.sourceCommand = `apt-get source ${pkg.source}=${pkg.sourceVersion}`;
  }
  // Debian copyright files refer to these complete license texts by path.
  const common = [];
  for (const file of await readdir("/usr/share/common-licenses"))
    common.push(
      await copyNotice(
        `/usr/share/common-licenses/${file}`,
        destination,
        `common-licenses/${file}`,
      ),
    );
  const helpers = [];
  for (const file of upstream.files.filter((file) =>
    file.packages.some(
      (pkg) => pkg.startsWith("AppImage") || pkg === "linuxdeploy-gtk-hook",
    ),
  )) {
    helpers.push({
      packages: file.packages,
      ...(await copyNotice(
        path.join(upstream.vendor, file.file),
        destination,
        `upstream/${file.file}`,
      )),
      origin: file.url,
    });
  }
  if (managedRuntime.length) {
    // Not from the host package database: attributed to the pinned llama.cpp
    // build in application.json, which must already be in this directory.
    const application = await json(path.join(destination, "application.json"));
    assert.ok(
      application.packages.some(
        (pkg) =>
          pkg.ecosystem === "managed-runtime" && pkg.name === "llama.cpp",
      ),
      `${RUNTIME_LOCATION} is bundled but application.json does not attribute llama.cpp`,
    );
  }
  const manifest = {
    schema: 1,
    packages: [...packages.values()].sort((a, b) =>
      a.name.localeCompare(b.name),
    ),
    managedRuntime: {
      location: RUNTIME_LOCATION,
      attributedIn: "application.json",
      files: managedRuntime,
    },
    generatedCaches: generated,
    commonLicenses: common,
    helpers,
  };
  await writeFile(
    path.join(destination, "system.json"),
    `${JSON.stringify(manifest, null, 2)}\n`,
  );
  console.log(`Collected notices for ${packages.size} bundled system packages`);
  return manifest;
}

if (
  process.argv[1] &&
  path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)
) {
  const [kind, directory] = process.argv.slice(2);
  assert.ok(
    directory && ["application", "appdir"].includes(kind),
    "Usage: node scripts/native-notices.mjs application OUTPUT | appdir APPDIR",
  );
  if (kind === "application") await applicationNotices(path.resolve(directory));
  else await appdirNotices(path.resolve(directory));
}
