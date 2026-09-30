// Debian package finishing and its checks (scripts/native-deb.mjs), on
// small real packages built with dpkg-deb. Run: node --test scripts/test-native-deb.mjs
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import {
  chmod,
  copyFile,
  mkdir,
  mkdtemp,
  readFile,
  rm,
  stat,
  symlink,
  writeFile,
} from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { gunzipSync } from "node:zlib";
import { addRuntimeToDeb } from "./llama-runtime.mjs";
import {
  COMPLETIONS,
  DEB_DOC,
  ICON_SIZES,
  LINTIAN_OVERRIDES,
  MAN_PAGE,
  appstreamReleases,
  changelogDate,
  compareVersions,
  debianChangelog,
  debianCopyright,
  dep5Body,
  finishDebianPackage,
  formatControl,
  glibcFloor,
  gzipMax,
  parseContentsLine,
  parseControl,
  pngSize,
  verifyDebianPackage,
  withDependencies,
  wrapDescription,
} from "./native-deb.mjs";
import { DESKTOP_FILE, METAINFO_FILE } from "./native-desktop-metadata.mjs";

const run = promisify(execFile);
const root = fileURLToPath(new URL("../", import.meta.url));
const metainfo = await readFile(
  path.join(root, "packaging", METAINFO_FILE),
  "utf8",
);
const releases = appstreamReleases(metainfo);
const version = releases[0].version;
const notice = await readFile(path.join(root, "NOTICE"), "utf8");
const iconBytes = await readFile(
  path.join(root, "assets/icons/hicolor/48x48/apps/shadow-agent.png"),
);
const LLAMA_LICENSE = `MIT License

Copyright (c) 2023-2026 The ggml authors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND.
`;
const TAURI_CONTROL = `Package: shadow-code
Version: ${version}
Architecture: amd64
Installed-Size: 1
Maintainer: Shadowfetch <209457103+Shadowfetchapps@users.noreply.github.com>
Priority: optional
Depends: git, libgomp1, libssl3, libasound2, libwebkit2gtk-4.1-0, libgtk-3-0
Recommends: libvulkan1, bubblewrap
Section: devel
Description: Desktop coding agent for local models and your subscriptions
 ShadowCode runs coding tasks on GGUF models on this computer through a bundled llama.cpp runtime, or through the official Codex, Claude Code, Cursor and Grok command-line tools and the Antigravity agent you are signed in to, or hundreds of OpenRouter models with your own API key, with explicit approvals, durable conversations and Git review.
`;

test("control files keep their fields and get wrapped descriptions", () => {
  const fields = parseControl(TAURI_CONTROL);
  assert.equal(formatControl(fields), TAURI_CONTROL);
  const description = wrapDescription(fields.at(-1)[1]);
  const lines = description.split("\n");
  assert.equal(
    lines[0],
    "Desktop coding agent for local models and your subscriptions",
  );
  assert.ok(lines.length > 3);
  assert.ok(
    lines.slice(1).every((line) => line.startsWith(" ") && line.length <= 80),
  );
  assert.equal(
    lines
      .slice(1)
      .map((line) => line.trim())
      .join(" "),
    fields.at(-1)[1].split("\n")[1].trim(),
  );
  // Paragraph separators survive.
  assert.equal(
    wrapDescription("Short\n first paragraph\n .\n second paragraph"),
    "Short\n first paragraph\n .\n second paragraph",
  );
  assert.equal(
    withDependencies("git, libc6 (>= 2.17), libgomp1", [
      "libc6 (>= 2.39)",
      "libstdc++6",
    ]),
    "git, libc6 (>= 2.39), libgomp1, libstdc++6",
  );
});

test("the libc floor is the newest GLIBC version any binary needs", () => {
  const readelf = (versions) =>
    versions
      .map((v) => `  0x0010:   Name: GLIBC_${v}  Flags: none  Version: 3`)
      .join("\n");
  assert.equal(
    glibcFloor([
      readelf(["2.2.5", "2.34"]),
      readelf(["2.39", "2.14"]),
      "Name: GLIBCXX_3.4.32",
    ]),
    "2.39",
  );
  assert.equal(glibcFloor(["no versions"]), null);
  assert.equal(compareVersions("2.10", "2.9"), 1);
  assert.equal(compareVersions("2.39", "2.39.0"), 0);
});

test("the Debian changelog comes from the AppStream release history", () => {
  assert.ok(releases.length >= 2);
  assert.match(version, /^\d+\.\d+\.\d+$/);
  assert.equal(changelogDate("2026-09-28"), "Mon, 28 Sep 2026 00:00:00 +0000");
  assert.equal(changelogDate("2024-02-29"), "Thu, 29 Feb 2024 00:00:00 +0000");
  assert.throws(() => changelogDate("2026-02-30"));
  // Two releases on one day carry times, so the newest stays the latest.
  assert.equal(changelogDate("2026-09-28T17:59:32Z"), "Mon, 28 Sep 2026 17:59:32 +0000");
  assert.equal(changelogDate("2026-09-28T23:00Z"), "Mon, 28 Sep 2026 23:00:00 +0000");
  assert.throws(() => changelogDate("2026-09-28T24:00:00Z"));
  assert.throws(() => changelogDate("2026-09-28 17:59"));
  const text = debianChangelog({ maintainer: "A <a@example.com>", releases });
  const lines = text.split("\n");
  assert.equal(lines[0], `shadow-code (${version}) stable; urgency=medium`);
  assert.ok(
    lines.every((line) => line.length <= 80 || line.startsWith(" -- ")),
  );
  assert.match(
    text,
    / -- A <a@example\.com> {2}\w{3}, \d{2} \w{3} \d{4} 00:00:00 \+0000\n/,
  );
  assert.ok(text.includes(`releases/tag/v${version}`));
  assert.equal([...text.matchAll(/^shadow-code \(/gm)].length, releases.length);
  // lintian refuses a newest entry that is not dated after the one before
  // (latest-changelog-entry-without-new-date): tracked dates strictly fall.
  const stamps = releases.map(({ date }) =>
    Date.parse(date.includes("T") ? date : `${date}T00:00:00Z`),
  );
  stamps.slice(1).forEach((stamp, index) =>
    assert.ok(
      stamps[index] > stamp,
      `AppStream release ${releases[index].version} must be dated after ${releases[index + 1].version}`,
    ),
  );
});

test("the copyright file carries the license and NOTICE in DEP-5 form", () => {
  const copyright = debianCopyright({
    notice,
    llamaLicense: LLAMA_LICENSE,
    runtimeCommit: "18f9f7bef960b76b693d8dcbb33cbbd6148c1631",
  });
  assert.ok(
    copyright.startsWith(
      "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/\n",
    ),
  );
  assert.ok(
    copyright.includes(" ShadowCode was originally created by Shadowfetch."),
  );
  assert.ok(
    copyright.includes(
      "Files: *\nCopyright: 2026 Shadowfetch\nLicense: Apache-2.0\n",
    ),
  );
  assert.ok(
    copyright.includes(
      "Files: usr/lib/shadowcode/*\nCopyright: 2023-2026 The ggml authors\nLicense: MIT\n",
    ),
  );
  assert.ok(
    copyright.includes("\nLicense: MIT\n Permission is hereby granted"),
  );
  // Every continuation line is indented; blank lines inside fields are " .".
  for (const paragraph of copyright.trim().split("\n\n"))
    for (const line of paragraph.split("\n"))
      assert.match(line, /^(?:[A-Za-z-]+: .*|[A-Za-z-]+:| \S.*| \.)$/, line);
  assert.equal(dep5Body("a\n\n b  \n"), " a\n .\n  b");
  assert.throws(() =>
    debianCopyright({ notice, llamaLicense: "no", runtimeCommit: "x" }),
  );
});

test("helpers read gzip headers, PNG sizes and dpkg listings", () => {
  const packed = gzipMax(Buffer.from("hello"));
  assert.equal(packed[8], 2);
  assert.equal(packed.readUInt32LE(4), 0, "no timestamp");
  assert.equal(gunzipSync(packed).toString(), "hello");
  assert.deepEqual(pngSize(iconBytes), { width: 48, height: 48 });
  assert.throws(() => pngSize(Buffer.from("GIF89a")));
  assert.deepEqual(
    parseContentsLine(
      "drwxr-xr-x root/root         0 2026-09-26 15:36 ./usr/lib/",
    ),
    {
      mode: "drwxr-xr-x",
      owner: "root/root",
      name: "usr/lib",
      target: undefined,
    },
  );
  assert.deepEqual(
    parseContentsLine(
      "lrwxrwxrwx root/root         0 2026-09-26 15:36 ./usr/lib/shadowcode/libggml.so -> libggml.so.0",
    ),
    {
      mode: "lrwxrwxrwx",
      owner: "root/root",
      name: "usr/lib/shadowcode/libggml.so",
      target: "libggml.so.0",
    },
  );
});

const MANPAGE =
  '.TH SHADOWCODE 1 "" "ShadowCode"\n.SH NAME\nshadowcode \\- test page with enough text to pass the size check\n';
const COMPLETION = (shell) =>
  `# ${shell} completion for shadowcode\n${"#".repeat(120)}\n`;

/** exec that runs real tools, but answers for the packaged executable. */
function fakeExec(calls) {
  return async (binary, args, options = {}) => {
    calls.push([binary, ...args]);
    if (binary.endsWith("/usr/bin/shadowcode")) {
      if (args[0] === "manpage") return { stdout: MANPAGE };
      if (args[0] === "completions") return { stdout: COMPLETION(args[1]) };
      throw new Error(`unexpected ${args}`);
    }
    return run(binary, args, { maxBuffer: 64_000_000, ...options });
  };
}

/** A Tauri-shaped .deb (group-writable, as a 002 umask leaves it) and a
 * managed runtime with a real shared library and a C++ executable. */
async function fixture(scratch) {
  const tree = path.join(scratch, "tauri");
  const put = async (relative, bytes, mode = 0o664) => {
    const file = path.join(tree, relative);
    await mkdir(path.dirname(file), { recursive: true, mode: 0o775 });
    await writeFile(file, bytes);
    await chmod(file, mode);
  };
  await put("DEBIAN/control", TAURI_CONTROL, 0o644);
  await mkdir(path.join(tree, "usr/bin"), { recursive: true });
  await copyFile("/usr/bin/apt", path.join(tree, "usr/bin/shadowcode"));
  await chmod(path.join(tree, "usr/bin/shadowcode"), 0o775);
  await put(
    "usr/share/applications/ShadowCode.desktop",
    "[Desktop Entry]\nType=Application\nName=ShadowCode\nExec=shadowcode\nIcon=shadowcode\nCategories=Development;\n",
  );
  await put(`usr/share/metainfo/${METAINFO_FILE}`, metainfo);
  await put(
    "usr/share/icons/hicolor/32x32/apps/shadowcode.png",
    await readFile(path.join(root, "assets/icons/shadow-agent-32.png")),
  );
  await put("usr/share/doc/shadowcode/notices/README.txt", "notices\n");
  for (const directory of ["usr", "usr/share", "usr/share/doc"])
    await chmod(path.join(tree, directory), 0o775);
  const deb = path.join(scratch, `ShadowCode_${version}_amd64.deb`);
  await run("dpkg-deb", ["--root-owner-group", "--build", tree, deb]);

  const runtime = path.join(scratch, "runtime");
  await mkdir(path.join(runtime, "NOTICES"), { recursive: true });
  await writeFile(
    path.join(runtime, "COMMIT"),
    "commit=18f9f7bef960b76b693d8dcbb33cbbd6148c1631\nbackend=cpu\n",
  );
  await writeFile(
    path.join(runtime, "NOTICES/llama.cpp-LICENSE"),
    LLAMA_LICENSE,
  );
  await copyFile(
    "/usr/lib/x86_64-linux-gnu/libz.so.1",
    path.join(runtime, "libggml.so.0.25.0"),
  );
  await chmod(path.join(runtime, "libggml.so.0.25.0"), 0o755);
  await symlink("libggml.so.0.25.0", path.join(runtime, "libggml.so.0"));
  await copyFile("/usr/bin/apt", path.join(runtime, "llama-server"));
  await chmod(path.join(runtime, "llama-server"), 0o755);
  // Debug information the package must not carry.
  await writeFile(path.join(scratch, "debug.bin"), "debug info");
  await run("objcopy", [
    "--add-section",
    `.debug_info=${path.join(scratch, "debug.bin")}`,
    path.join(runtime, "llama-server"),
  ]);
  return { deb, runtime };
}

test("a finished package passes the distribution checks", async () => {
  const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-deb-"));
  try {
    const { deb, runtime } = await fixture(scratch);
    const calls = [];
    const exec = fakeExec(calls);
    let finished;
    await addRuntimeToDeb(deb, { directory: runtime }, scratch, {
      run: (binary, args) => run(binary, args, { maxBuffer: 64_000_000 }),
      normalizeDesktop: true,
      finish: async (work) => {
        finished = await finishDebianPackage(work, { exec });
      },
    });
    assert.equal(finished.version, version);
    assert.equal(finished.cxx, true);
    assert.match(finished.floor, /^2\.\d+$/);
    assert.ok(
      calls.some(
        ([binary, ...args]) =>
          binary === "strip" && args.includes("--strip-unneeded"),
      ),
    );

    const extracted = path.join(scratch, "extracted");
    await run("dpkg-deb", ["--extract", deb, extracted]);
    const report = await verifyDebianPackage({ debPath: deb, extracted, exec });
    assert.equal(report.glibc, finished.floor);
    assert.match(
      report.depends,
      new RegExp(`libc6 \\(>= ${finished.floor.replace(".", "\\.")}\\)`),
    );
    assert.match(report.depends, /libgcc-s1, libstdc\+\+6$/);
    assert.equal(report.section, "devel");
    assert.equal(report.runtimeBinariesStripped, 2);

    const text = async (relative) =>
      readFile(path.join(extracted, relative), "utf8");
    assert.match(
      await text(`${DEB_DOC}/copyright`),
      /originally created by Shadowfetch/,
    );
    assert.equal(
      gunzipSync(await readFile(path.join(extracted, MAN_PAGE))).toString(),
      MANPAGE,
    );
    for (const [shell, file] of Object.entries(COMPLETIONS))
      assert.equal(await text(file), COMPLETION(shell));
    assert.match(await text(LINTIAN_OVERRIDES), /embedded-library libyaml/);
    for (const size of ICON_SIZES)
      assert.ok(
        await stat(
          path.join(
            extracted,
            `usr/share/icons/hicolor/${size}x${size}/apps/shadowcode.png`,
          ),
        ),
      );
    assert.ok(
      await stat(
        path.join(extracted, `usr/share/applications/${DESKTOP_FILE}`),
      ),
    );
    const control = (await run("dpkg-deb", ["--field", deb, "Description"]))
      .stdout;
    assert.ok(control.split("\n").every((line) => line.length <= 80));
    assert.equal(
      (await run("dpkg-deb", ["--field", deb, "Homepage"])).stdout.trim(),
      "https://github.com/Shadowfetchapps/ShadowCode",
    );
    // The shared library lost its execute bit; the executable kept it.
    const listing = (await run("dpkg-deb", ["--contents", deb])).stdout;
    assert.match(
      listing,
      /^-rw-r--r-- root\/root .* \.\/usr\/lib\/shadowcode\/libggml\.so\.0\.25\.0$/m,
    );
    assert.match(
      listing,
      /^-rwxr-xr-x root\/root .* \.\/usr\/lib\/shadowcode\/llama-server$/m,
    );
    assert.doesNotMatch(listing, /^drwxrwxr-x/m);
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
});

test("the checks reject what lintian and dpkg would", async () => {
  const scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-deb-bad-"));
  try {
    const { deb, runtime } = await fixture(scratch);
    const exec = fakeExec([]);
    await addRuntimeToDeb(deb, { directory: runtime }, scratch, {
      run: (binary, args) => run(binary, args, { maxBuffer: 64_000_000 }),
      normalizeDesktop: true,
      finish: (tree) => finishDebianPackage(tree, { exec }),
    });
    // The finished package's tree, control members included, to break.
    const work = path.join(scratch, "finished");
    await run("dpkg-deb", ["--raw-extract", deb, work]);
    const broken = async (label, change, message) => {
      const copy = path.join(scratch, label);
      await run("cp", ["-a", work, copy]);
      await change(copy);
      const bad = path.join(scratch, `${label}.deb`);
      await run("dpkg-deb", ["--root-owner-group", "--build", copy, bad]);
      const extracted = path.join(scratch, `${label}-x`);
      await run("dpkg-deb", ["--extract", bad, extracted]);
      await assert.rejects(
        verifyDebianPackage({ debPath: bad, extracted, exec }),
        message,
        label,
      );
    };
    await broken(
      "group-writable",
      (copy) => chmod(path.join(copy, "usr/share"), 0o775),
      /usr\/share\/ has mode drwxrwxr-x/,
    );
    await broken(
      "no-copyright",
      (copy) => rm(path.join(copy, DEB_DOC, "copyright")),
      /lacks \/usr\/share\/doc\/shadow-code\/copyright/,
    );
    await broken(
      "maintainer-script",
      async (copy) => {
        await writeFile(
          path.join(copy, "DEBIAN/postrm"),
          "#!/bin/sh\nrm -rf /home/*/.config/shadow-agent\n",
        );
        await chmod(path.join(copy, "DEBIAN/postrm"), 0o755);
      },
      /No maintainer scripts/,
    );
    await broken(
      "old-libc",
      async (copy) => {
        const file = path.join(copy, "DEBIAN/control");
        await writeFile(
          file,
          (await readFile(file, "utf8")).replace(
            /libc6 \(>= [\d.]+\)/,
            "libc6 (>= 2.17)",
          ),
        );
      },
      /Depends must require libc6/,
    );
    await broken(
      "unstripped",
      async (copy) => {
        await writeFile(path.join(scratch, "debug2.bin"), "debug info");
        await run("objcopy", [
          "--add-section",
          `.debug_info=${path.join(scratch, "debug2.bin")}`,
          path.join(copy, "usr/lib/shadowcode/llama-server"),
        ]);
      },
      /llama-server is not stripped/,
    );
    await broken(
      "stale-manpage",
      (copy) =>
        writeFile(
          path.join(copy, MAN_PAGE),
          gzipMax(Buffer.from('.TH SHADOWCODE 1 "" old\n')),
        ),
      /manual page must be the packaged executable's own/,
    );
    await broken(
      "conffile",
      async (copy) => {
        await mkdir(path.join(copy, "etc/shadowcode"), {
          recursive: true,
          mode: 0o755,
        });
        await chmod(path.join(copy, "etc"), 0o755);
        await chmod(path.join(copy, "etc/shadowcode"), 0o755);
        await writeFile(
          path.join(copy, "etc/shadowcode/policy.yaml"),
          "updates:\n  check: false\n",
        );
        await chmod(path.join(copy, "etc/shadowcode/policy.yaml"), 0o644);
      },
      /must not ship configuration files/,
    );
  } finally {
    await rm(scratch, { recursive: true, force: true });
  }
});
