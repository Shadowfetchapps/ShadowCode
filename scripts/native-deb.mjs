// Debian package finishing: what dpkg and lintian expect of a package that
// distributions ship. Build-time only; no Node code is shipped.
//
// build-native.mjs calls finishDebianPackage() on the extracted Tauri .deb
// (after the managed llama.cpp runtime is added, before md5sums and the
// repack). check-native-package.mjs verifies the result.
import assert from "node:assert/strict";
import {
  chmod,
  copyFile,
  lstat,
  mkdir,
  readFile,
  readdir,
  writeFile,
} from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { gunzipSync, gzipSync } from "node:zlib";
import { RUNTIME_LOCATION } from "./llama-runtime.mjs";
import { DESKTOP_FILE, METAINFO_FILE } from "./native-desktop-metadata.mjs";

const root = fileURLToPath(new URL("../", import.meta.url));

export const DEB_PACKAGE = "shadow-code";
export const DEB_DOC = `usr/share/doc/${DEB_PACKAGE}`;
export const HOMEPAGE = "https://github.com/Shadowfetchapps/ShadowCode";
export const MAN_PAGE = "usr/share/man/man1/shadowcode.1.gz";
export const COMPLETIONS = {
  bash: "usr/share/bash-completion/completions/shadowcode",
  zsh: "usr/share/zsh/vendor-completions/_shadowcode",
  fish: "usr/share/fish/vendor_completions.d/shadowcode.fish",
};
/** hicolor sizes shipped as PNG (plus scalable SVG). Tauri installs 32,
 * 128, 256 and 512 from tauri.conf.json; the rest are added here. */
export const ICON_SIZES = [16, 22, 24, 32, 48, 64, 96, 128, 256, 512];
export const ICON_NAME = "shadowcode";
export const DESKTOP_KEYWORDS = "agent;AI;coding;LLM;llama.cpp;git;";
export const LINTIAN_OVERRIDES = `usr/share/lintian/overrides/${DEB_PACKAGE}`;
/** Lintian findings that are not defects of this package; each is
 * explained in the shipped overrides file. */
export const LINTIAN_OVERRIDE_TEXT = `# unsafe-libyaml (a Rust translation of libyaml, used through serde_yaml_ng)
# is compiled into the executable; there is no system library to link.
${DEB_PACKAGE}: embedded-library libyaml [usr/bin/shadowcode]
# Verbatim third-party license texts kept for attribution; the inventory
# with their SHA-256 digests is notices/application.json.
${DEB_PACKAGE}: extra-license-file [usr/share/doc/shadowcode/notices/*]
`;

/** Shared objects, including versioned SONAMEs and dlopen()ed modules. */
export const isSharedLibrary = (file) => /\.so(?:\.\d+)*$/.test(file);

/** A deb control file as ordered [field, value] pairs; continuation lines
 * stay in the value with their leading space. */
export function parseControl(text) {
  const fields = [];
  for (const line of text.replace(/\n+$/, "").split("\n")) {
    if (/^[ \t]/.test(line)) {
      assert.ok(fields.length, "Control continuation line without a field");
      fields[fields.length - 1][1] += `\n${line}`;
      continue;
    }
    const at = line.indexOf(":");
    assert.ok(at > 0, `Malformed control line: ${line}`);
    fields.push([line.slice(0, at), line.slice(at + 1).trim()]);
  }
  return fields;
}

export function formatControl(fields) {
  return `${fields.map(([name, value]) => `${name}: ${value}`).join("\n")}\n`;
}

export function controlField(fields, name) {
  return fields.find(
    ([field]) => field.toLowerCase() === name.toLowerCase(),
  )?.[1];
}

export function setControlField(fields, name, value, after = "Description") {
  const index = fields.findIndex(
    ([field]) => field.toLowerCase() === name.toLowerCase(),
  );
  if (index >= 0) fields[index][1] = value;
  else {
    const at = fields.findIndex(([field]) => field === after);
    fields.splice(at < 0 ? fields.length : at, 0, [name, value]);
  }
  return fields;
}

/** Words wrapped to `width` columns. */
export function wrap(text, width) {
  const lines = [];
  let line = "";
  for (const word of text.split(/\s+/).filter(Boolean)) {
    if (line && line.length + 1 + word.length > width) {
      lines.push(line);
      line = word;
    } else line = line ? `${line} ${word}` : word;
  }
  if (line) lines.push(line);
  return lines;
}

/** Description: synopsis plus an extended description wrapped below 80
 * columns (Tauri writes the long description as one line). */
export function wrapDescription(value) {
  const [synopsis, ...rest] = value.split("\n");
  const paragraphs = rest
    .map((line) => line.replace(/^ /, ""))
    .join("\n")
    .split(/\n\.\n/)
    .map((paragraph) => wrap(paragraph.replace(/\n/g, " "), 78))
    .filter((lines) => lines.length);
  return [
    synopsis.trim(),
    ...paragraphs.flatMap((lines, index) => [
      ...(index ? [" ."] : []),
      ...lines.map((line) => ` ${line}`),
    ]),
  ].join("\n");
}

/** The newest GLIBC_x.y symbol version any `readelf --version-info` output
 * requires, or null. */
export function glibcFloor(outputs) {
  let floor = null;
  for (const output of outputs)
    for (const [, version] of output.matchAll(/Name: GLIBC_(\d+(?:\.\d+)+)\b/g))
      if (!floor || compareVersions(version, floor) > 0) floor = version;
  return floor;
}

export function compareVersions(left, right) {
  const a = left.split(".").map(Number);
  const b = right.split(".").map(Number);
  for (let i = 0; i < Math.max(a.length, b.length); i++) {
    const diff = (a[i] || 0) - (b[i] || 0);
    if (diff) return Math.sign(diff);
  }
  return 0;
}

/** Depends with `entries` added or replacing an entry for the same
 * package; other entries keep their order. */
export function withDependencies(depends, entries) {
  const list = (depends || "")
    .split(",")
    .map((entry) => entry.trim())
    .filter(Boolean);
  const name = (entry) => entry.split(/[\s(]/)[0];
  for (const entry of entries) {
    const at = list.findIndex((existing) => name(existing) === name(entry));
    if (at >= 0) list[at] = entry;
    else list.push(entry);
  }
  return list.join(", ");
}

/** `<release>` entries of the AppStream metadata, newest first. */
export function appstreamReleases(xml) {
  const releases = [];
  for (const [, attributes, body] of xml.matchAll(
    /<release\s([^>]*?)(?:\/>|>([\s\S]*?)<\/release>)/g,
  )) {
    const version = /version="([^"]+)"/.exec(attributes)?.[1];
    // AppStream allows a day or a UTC time; two releases on one day need
    // times so the Debian changelog stays in order.
    const date = /date="(\d{4}-\d{2}-\d{2}(?:T\d{2}:\d{2}(?::\d{2})?Z)?)"/.exec(
      attributes,
    )?.[1];
    assert.ok(
      version && date,
      `AppStream release without version/date: ${attributes}`,
    );
    const text = [...(body || "").matchAll(/<p>([\s\S]*?)<\/p>/g)]
      .map(([, p]) =>
        p
          .replace(/<[^>]+>/g, "")
          .replace(/&lt;/g, "<")
          .replace(/&gt;/g, ">")
          .replace(/&quot;/g, '"')
          .replace(/&apos;/g, "'")
          .replace(/&amp;/g, "&")
          .replace(/\s+/g, " ")
          .trim(),
      )
      .filter(Boolean);
    releases.push({ version, date, text });
  }
  return releases;
}

const DAYS = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS = [
  "Jan",
  "Feb",
  "Mar",
  "Apr",
  "May",
  "Jun",
  "Jul",
  "Aug",
  "Sep",
  "Oct",
  "Nov",
  "Dec",
];
/** RFC 5322 date in UTC, as Debian changelog trailers need: an AppStream
 * day (`2026-09-28`, midnight) or UTC time (`2026-09-28T17:59:32Z`). */
export function changelogDate(isoDate) {
  const match =
    /^(\d{4})-(\d{2})-(\d{2})(?:T(\d{2}):(\d{2})(?::(\d{2}))?Z)?$/.exec(isoDate);
  assert.ok(match, `Invalid date ${isoDate}`);
  const [year, month, day, hour = 0, minute = 0, second = 0] = match
    .slice(1)
    .map((part) => (part === undefined ? undefined : Number(part)));
  assert.ok(hour < 24 && minute < 60 && second < 60, `Invalid time ${isoDate}`);
  const date = new Date(Date.UTC(year, month - 1, day, hour, minute, second));
  assert.equal(date.getUTCDate(), day, `Invalid date ${isoDate}`);
  const two = (value) => String(value).padStart(2, "0");
  return `${DAYS[date.getUTCDay()]}, ${two(day)} ${MONTHS[month - 1]} ${year} ${two(hour)}:${two(minute)}:${two(second)} +0000`;
}

/** A Debian changelog built from the AppStream release history, so the
 * package, software centres and the release notes tell the same story. */
export function debianChangelog({ pkg = DEB_PACKAGE, maintainer, releases }) {
  assert.ok(releases.length, "No releases for the Debian changelog");
  return releases
    .map(({ version, date, text }) => {
      const bullets = [
        ...text,
        `Release notes: ${HOMEPAGE}/releases/tag/v${version}`,
      ].flatMap((item) =>
        wrap(item, 74).map(
          (line, index) => `${index ? "    " : "  * "}${line}`,
        ),
      );
      return `${pkg} (${version}) stable; urgency=medium\n\n${bullets.join("\n")}\n\n -- ${maintainer}  ${changelogDate(date)}\n`;
    })
    .join("\n");
}

/** Text as a DEP-5 multi-line field body: one leading space, empty lines as
 * " .". */
export function dep5Body(text) {
  return text
    .replace(/\n+$/, "")
    .split("\n")
    .map((line) => (line.trim() ? ` ${line.replace(/\s+$/, "")}` : " ."))
    .join("\n");
}

/** The MIT permission text of a license file (from "Permission is hereby
 * granted" to the end). */
export function mitPermission(text) {
  const at = text.indexOf("Permission is hereby granted");
  assert.ok(at >= 0, "Not an MIT license text");
  return text.slice(at);
}

/** /usr/share/doc/shadow-code/copyright in the machine-readable format.
 * ShadowCode is Apache-2.0 with a NOTICE that must travel with every copy;
 * the bundled llama.cpp runtime is MIT. Every other component's license
 * text is listed with a SHA-256 digest under notices/. */
export function debianCopyright({ notice, llamaLicense, runtimeCommit }) {
  const holder = /^Copyright \(c\) (.+)$/m.exec(llamaLicense)?.[1];
  assert.ok(holder, "llama.cpp license lacks its copyright line");
  return `Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/
Upstream-Name: ShadowCode
Upstream-Contact: ${HOMEPAGE}/issues
Source: ${HOMEPAGE}
Comment: ShadowCode was originally created by Shadowfetch. Its NOTICE file,
 reproduced here, must accompany every copy (Apache-2.0 section 4(d)):
 .
${dep5Body(notice)}
 .
 The executable also contains Rust and JavaScript libraries under their own
 licenses (MIT, Apache-2.0, BSD, MPL-2.0, ISC, Unicode and others). Their
 license texts and an inventory with versions, sources and SHA-256 digests
 are in /usr/share/doc/shadowcode/notices (application.json). The full
 ShadowCode license text is also there as ShadowCode-LICENSE.

Files: *
Copyright: 2026 Shadowfetch
License: Apache-2.0

Files: ${RUNTIME_LOCATION}/*
Copyright: ${holder}
License: MIT
Comment: The managed llama.cpp runtime, built from llama.cpp commit
 ${runtimeCommit}. It also contains cpp-httplib (MIT), nlohmann/json (MIT),
 SPIRV-Headers (MIT), stb_image (MIT or public domain), miniaudio (MIT-0 or
 public domain) and subprocess.h (Unlicense); their license texts are in
 /${RUNTIME_LOCATION}/NOTICES.

License: Apache-2.0
 Licensed under the Apache License, Version 2.0 (the "License"); you may not
 use this file except in compliance with the License. You may obtain a copy
 of the License at
 .
 http://www.apache.org/licenses/LICENSE-2.0
 .
 Unless required by applicable law or agreed to in writing, software
 distributed under the License is distributed on an "AS IS" BASIS, WITHOUT
 WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied. See the
 License for the specific language governing permissions and limitations
 under the License.
 .
 On Debian systems, the complete text of the Apache License, Version 2.0 can
 be found in "/usr/share/common-licenses/Apache-2.0".

License: MIT
${dep5Body(mitPermission(llamaLicense))}
`;
}

/** gzip at maximum compression without a name or timestamp (`gzip -9n`),
 * so repeated builds are byte-identical. */
export function gzipMax(bytes) {
  const out = gzipSync(bytes, { level: 9 });
  // zlib writes mtime 0 and no name; XFL 2 marks maximum compression.
  assert.equal(out[8], 2, "gzip is not at maximum compression");
  return out;
}

/** Width and height of a PNG from its IHDR chunk. */
export function pngSize(bytes) {
  assert.equal(
    bytes.subarray(0, 8).toString("hex"),
    "89504e470d0a1a0a",
    "Not a PNG",
  );
  return { width: bytes.readUInt32BE(16), height: bytes.readUInt32BE(20) };
}

async function walk(directory, relative = "") {
  const entries = [];
  for (const entry of await readdir(path.join(directory, relative), {
    withFileTypes: true,
  })) {
    const file = path.join(relative, entry.name);
    if (entry.isDirectory()) {
      entries.push({ file, type: "directory" });
      entries.push(...(await walk(directory, file)));
    } else if (entry.isSymbolicLink()) entries.push({ file, type: "symlink" });
    else if (entry.isFile()) entries.push({ file, type: "file" });
  }
  return entries;
}

async function isElf(file) {
  const bytes = await readFile(file);
  return bytes.subarray(0, 4).toString("latin1") === "\x7fELF";
}

async function put(work, relative, bytes) {
  const file = path.join(work, relative);
  await mkdir(path.dirname(file), { recursive: true });
  await writeFile(file, bytes);
}

/**
 * Finish an extracted Tauri .deb in `work` (the output of
 * `dpkg-deb --raw-extract`): Debian copyright and changelog, manual page,
 * shell completions, hicolor icons, lintian overrides, a stripped runtime,
 * the libc/libstdc++ dependencies its binaries need, a wrapped description,
 * and the permissions dpkg expects. `exec(binary, args)` resolves to
 * `{ stdout }`.
 */
export async function finishDebianPackage(work, { exec, checkout = root }) {
  const controlPath = path.join(work, "DEBIAN/control");
  const fields = parseControl(await readFile(controlPath, "utf8"));
  assert.equal(controlField(fields, "Package"), DEB_PACKAGE);
  const version = controlField(fields, "Version");
  const maintainer = controlField(fields, "Maintainer");
  assert.ok(version && maintainer, "deb control lacks Version or Maintainer");
  const executable = path.join(work, "usr/bin/shadowcode");

  // The runtime's binaries: stripped like dh_strip does, libraries not
  // executable. The main executable is already stripped by Cargo.
  const runtime = path.join(work, RUNTIME_LOCATION);
  const elves = [executable];
  for (const entry of await walk(runtime)) {
    if (entry.type !== "file") continue;
    const file = path.join(runtime, entry.file);
    if (!(await isElf(file))) continue;
    elves.push(file);
    const library = isSharedLibrary(entry.file);
    await exec("strip", [
      "--remove-section=.comment",
      "--remove-section=.note",
      ...(library ? ["--strip-unneeded"] : []),
      file,
    ]);
    await chmod(file, library ? 0o644 : 0o755);
  }
  const versions = [];
  let cxx = false;
  for (const file of elves) {
    versions.push(
      (await exec("readelf", ["--version-info", "--wide", file])).stdout,
    );
    const dynamic = (await exec("readelf", ["--dynamic", file])).stdout;
    cxx ||= /Shared library: \[(?:libstdc\+\+\.so\.6|libgcc_s\.so\.1)\]/.test(
      dynamic,
    );
  }
  const floor = glibcFloor(versions);
  assert.ok(floor, "No GLIBC symbol versions found in the package binaries");
  setControlField(
    fields,
    "Depends",
    withDependencies(controlField(fields, "Depends"), [
      `libc6 (>= ${floor})`,
      ...(cxx ? ["libgcc-s1", "libstdc++6"] : []),
    ]),
  );
  setControlField(
    fields,
    "Section",
    controlField(fields, "Section") || "devel",
  );
  setControlField(fields, "Homepage", HOMEPAGE);
  setControlField(
    fields,
    "Description",
    wrapDescription(controlField(fields, "Description")),
    "",
  );
  await writeFile(controlPath, formatControl(fields));

  // Documentation Debian requires under /usr/share/doc/<package>.
  const releases = appstreamReleases(
    await readFile(path.join(checkout, "packaging", METAINFO_FILE), "utf8"),
  );
  assert.equal(
    releases[0]?.version,
    version,
    "The newest AppStream release must be the packaged version",
  );
  await put(
    work,
    `${DEB_DOC}/changelog.gz`,
    gzipMax(Buffer.from(debianChangelog({ maintainer, releases }))),
  );
  const commit = /^commit=([0-9a-f]{40})$/m.exec(
    await readFile(path.join(runtime, "COMMIT"), "utf8"),
  )?.[1];
  await put(
    work,
    `${DEB_DOC}/copyright`,
    debianCopyright({
      notice: await readFile(path.join(checkout, "NOTICE"), "utf8"),
      llamaLicense: await readFile(
        path.join(runtime, "NOTICES/llama.cpp-LICENSE"),
        "utf8",
      ),
      runtimeCommit: commit,
    }),
  );

  // Manual page and completions, generated by the packaged executable from
  // the same definitions as --help.
  const generated = async (args) => {
    const { stdout } = await exec(executable, args, {
      env: { PATH: "/usr/bin:/bin", HOME: "/nonexistent", LANG: "C.UTF-8" },
    });
    assert.ok(
      stdout.length > 100,
      `shadowcode ${args.join(" ")} printed nothing`,
    );
    return Buffer.from(stdout);
  };
  await put(work, MAN_PAGE, gzipMax(await generated(["manpage"])));
  for (const [shell, file] of Object.entries(COMPLETIONS))
    await put(work, file, await generated(["completions", shell]));

  // hicolor icons at the standard sizes, named after the desktop entry's
  // Icon=, plus the scalable original.
  const desktopPath = path.join(work, "usr/share/applications", DESKTOP_FILE);
  const desktop = await readFile(desktopPath, "utf8");
  assert.match(desktop, new RegExp(`^Icon=${ICON_NAME}$`, "m"));
  // Search terms for application menus and software centres.
  if (!/^Keywords=/m.test(desktop))
    await writeFile(
      desktopPath,
      `${desktop.replace(/\n*$/, "\n")}Keywords=${DESKTOP_KEYWORDS}\n`,
    );
  for (const size of ICON_SIZES) {
    const target = `usr/share/icons/hicolor/${size}x${size}/apps/${ICON_NAME}.png`;
    const source = path.join(
      checkout,
      `assets/icons/hicolor/${size}x${size}/apps/shadow-agent.png`,
    );
    assert.deepEqual(pngSize(await readFile(source)), {
      width: size,
      height: size,
    });
    await mkdir(path.dirname(path.join(work, target)), { recursive: true });
    await copyFile(source, path.join(work, target));
  }
  await mkdir(path.join(work, "usr/share/icons/hicolor/scalable/apps"), {
    recursive: true,
  });
  await copyFile(
    path.join(checkout, "assets/icons/hicolor/scalable/apps/shadow-agent.svg"),
    path.join(work, `usr/share/icons/hicolor/scalable/apps/${ICON_NAME}.svg`),
  );
  await put(work, LINTIAN_OVERRIDES, LINTIAN_OVERRIDE_TEXT);

  // dpkg installs modes as packed: directories 0755, files 0644 unless
  // executable. The build's umask must not leak group-write bits.
  await chmod(work, 0o755);
  for (const entry of await walk(work)) {
    const file = path.join(work, entry.file);
    if (entry.type === "directory") await chmod(file, 0o755);
    else if (entry.type === "file") {
      const top = entry.file.split(path.sep)[0];
      const mode = (await lstat(file)).mode;
      const executableBit =
        top !== "DEBIAN" && mode & 0o111 && !isSharedLibrary(entry.file);
      await chmod(file, executableBit ? 0o755 : 0o644);
    }
  }
  return { floor, cxx, version };
}

/** One `dpkg-deb --contents` line as { mode, owner, name, target }. */
export function parseContentsLine(line) {
  const match =
    /^([-dlcbps][-rwxsStT]{9})\s+(\S+)\s+\d+\s+\S+\s+\S+\s+\.\/(.*?)(?: -> (.*))?$/.exec(
      line,
    );
  assert.ok(match, `Unexpected dpkg-deb --contents line: ${line}`);
  const [, mode, owner, name, target] = match;
  return { mode, owner, name: name.replace(/\/$/, ""), target };
}

const gunzipText = async (file) => {
  const bytes = await readFile(file);
  assert.equal(bytes[8], 2, `${file} is not gzip -9 compressed`);
  return gunzipSync(bytes).toString("utf8");
};

/**
 * What a distribution needs from the .deb, checked on the built package and
 * its extraction: ownership and modes, maintainer scripts, copyright and
 * changelog, manual page and completions matching the executable, hicolor
 * icons, lintian overrides, a stripped runtime and the libc floor.
 */
export async function verifyDebianPackage({ debPath, extracted, exec }) {
  const field = async (name) =>
    (await exec("dpkg-deb", ["--field", debPath, name])).stdout.trim();
  const version = await field("Version");
  const entries = (await exec("dpkg-deb", ["--contents", debPath])).stdout
    .split("\n")
    .filter(Boolean)
    .map(parseContentsLine);
  const byName = new Map(entries.map((entry) => [entry.name, entry]));
  for (const entry of entries) {
    assert.equal(
      entry.owner,
      "root/root",
      `${entry.name} is not owned by root`,
    );
    if (entry.mode.startsWith("d"))
      assert.equal(
        entry.mode,
        "drwxr-xr-x",
        `${entry.name}/ has mode ${entry.mode}`,
      );
    else if (entry.mode.startsWith("-"))
      assert.ok(
        isSharedLibrary(entry.name)
          ? entry.mode === "-rw-r--r--"
          : ["-rw-r--r--", "-rwxr-xr-x"].includes(entry.mode),
        `${entry.name} has mode ${entry.mode}`,
      );
    else assert.ok(entry.mode.startsWith("l"), `${entry.name} is not a file`);
  }
  const required = [
    `${DEB_DOC}/copyright`,
    `${DEB_DOC}/changelog.gz`,
    MAN_PAGE,
    ...Object.values(COMPLETIONS),
    ...ICON_SIZES.map(
      (size) => `usr/share/icons/hicolor/${size}x${size}/apps/${ICON_NAME}.png`,
    ),
    `usr/share/icons/hicolor/scalable/apps/${ICON_NAME}.svg`,
    LINTIAN_OVERRIDES,
    `usr/share/applications/${DESKTOP_FILE}`,
    `usr/share/metainfo/${METAINFO_FILE}`,
  ];
  for (const name of required)
    assert.ok(byName.has(name), `The deb lacks /${name}`);
  assert.match(
    await readFile(
      path.join(extracted, "usr/share/applications", DESKTOP_FILE),
      "utf8",
    ),
    /^Keywords=.+;$/m,
    "The launcher needs Keywords for menu search",
  );
  assert.ok(
    !entries.some(
      (entry) => entry.name === "etc" || entry.name.startsWith("etc/"),
    ),
    "The deb must not ship configuration files; user data stays untouched",
  );
  const controlFiles = (
    await exec("sh", [
      "-c",
      'dpkg-deb --ctrl-tarfile "$1" | tar -t',
      "sh",
      debPath,
    ])
  ).stdout
    .split("\n")
    .map((name) => name.replace(/^\.\//, ""))
    .filter((name) => name && name !== ".");
  assert.deepEqual(
    controlFiles.sort(),
    ["control", "md5sums"],
    "No maintainer scripts or conffiles: removing or purging the package touches only its own files",
  );

  const copyright = await readFile(
    path.join(extracted, DEB_DOC, "copyright"),
    "utf8",
  );
  for (const text of [
    "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/",
    "License: Apache-2.0",
    "originally created by Shadowfetch",
    "/usr/share/common-licenses/Apache-2.0",
    "/usr/share/doc/shadowcode/notices",
  ])
    assert.ok(copyright.includes(text), `copyright lacks "${text}"`);
  const changelog = await gunzipText(
    path.join(extracted, DEB_DOC, "changelog.gz"),
  );
  assert.equal(
    changelog.split("\n")[0],
    `${DEB_PACKAGE} (${version}) stable; urgency=medium`,
  );
  // Entries fit 80 columns; only the " -- maintainer  date" trailer may not.
  assert.ok(
    changelog
      .split("\n")
      .every((line) => line.length <= 80 || line.startsWith(" -- ")),
    "changelog entries must fit 80 columns",
  );

  const executable = path.join(extracted, "usr/bin/shadowcode");
  const env = { PATH: "/usr/bin:/bin", HOME: "/nonexistent", LANG: "C.UTF-8" };
  const man = await gunzipText(path.join(extracted, MAN_PAGE));
  assert.match(man, /^\.TH SHADOWCODE 1 /m);
  assert.equal(
    man,
    (await exec(executable, ["manpage"], { env })).stdout,
    "The manual page must be the packaged executable's own",
  );
  for (const [shell, file] of Object.entries(COMPLETIONS))
    assert.equal(
      await readFile(path.join(extracted, file), "utf8"),
      (await exec(executable, ["completions", shell], { env })).stdout,
      `The ${shell} completions must match the packaged executable`,
    );
  for (const size of ICON_SIZES)
    assert.deepEqual(
      pngSize(
        await readFile(
          path.join(
            extracted,
            `usr/share/icons/hicolor/${size}x${size}/apps/${ICON_NAME}.png`,
          ),
        ),
      ),
      { width: size, height: size },
      `${size}x${size} icon has the wrong size`,
    );
  assert.match(
    await readFile(
      path.join(
        extracted,
        `usr/share/icons/hicolor/scalable/apps/${ICON_NAME}.svg`,
      ),
      "utf8",
    ),
    /<svg[\s>]/,
  );

  // Binaries: the runtime is stripped, and Depends names the newest glibc
  // symbol version any of them needs.
  const elves = [];
  for (const entry of entries.filter((entry) => entry.mode.startsWith("-"))) {
    const file = path.join(extracted, entry.name);
    if (await isElf(file)) elves.push({ name: entry.name, file });
  }
  const versions = [];
  let cxx = false;
  for (const { name, file } of elves) {
    versions.push(
      (await exec("readelf", ["--version-info", "--wide", file])).stdout,
    );
    cxx ||= /Shared library: \[(?:libstdc\+\+\.so\.6|libgcc_s\.so\.1)\]/.test(
      (await exec("readelf", ["--dynamic", file])).stdout,
    );
    if (name.startsWith(`${RUNTIME_LOCATION}/`)) {
      const sections = (
        await exec("readelf", ["--section-headers", "--wide", file])
      ).stdout;
      assert.doesNotMatch(
        sections,
        /\s\.symtab\s|\s\.debug_/,
        `${name} is not stripped`,
      );
    }
  }
  const floor = glibcFloor(versions);
  const depends = await field("Depends");
  const libc = /(?:^|,\s*)libc6 \(>= ([\d.]+)\)/.exec(depends)?.[1];
  assert.ok(
    libc && compareVersions(libc, floor) >= 0,
    `Depends must require libc6 (>= ${floor}); it has ${depends}`,
  );
  if (cxx)
    for (const name of ["libgcc-s1", "libstdc++6"])
      assert.ok(
        depends.split(/,\s*/).some((entry) => entry.split(/[\s(]/)[0] === name),
        `Depends must name ${name}`,
      );
  assert.ok(await field("Section"), "control lacks Section");
  assert.equal(await field("Homepage"), HOMEPAGE);
  const description = await field("Description");
  assert.ok(
    description.split("\n").every((line) => line.length <= 80),
    "Description lines must fit 80 columns",
  );
  return {
    files: entries.length,
    glibc: floor,
    depends,
    recommends: await field("Recommends"),
    section: await field("Section"),
    runtimeBinariesStripped: elves.filter(({ name }) =>
      name.startsWith(`${RUNTIME_LOCATION}/`),
    ).length,
  };
}

/**
 * lintian's verdict when it is installed: errors fail, warnings are
 * reported. `required` fails when lintian is missing.
 */
export async function lintDebianPackage(debPath, { exec, required = false }) {
  const version = await exec("lintian", ["--version"]).catch(() => null);
  if (!version) {
    assert.ok(
      !required,
      "lintian is required (SHADOWCODE_REQUIRE_LINTIAN=1) but not installed",
    );
    return { available: false };
  }
  const result = await exec("lintian", [
    "--tag-display-limit",
    "0",
    debPath,
  ]).catch((error) => error);
  const lines = `${result.stdout || ""}`.split("\n").filter(Boolean);
  const errors = lines.filter((line) => line.startsWith("E:"));
  const warnings = lines.filter((line) => line.startsWith("W:"));
  assert.deepEqual(errors, [], "lintian reported errors");
  return {
    available: true,
    version: version.stdout.trim(),
    errors: errors.length,
    warnings,
  };
}
