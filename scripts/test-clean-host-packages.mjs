// Exercise exact CI packages in a disposable, network-disabled runtime image.
// The image contains only runtime dependencies; the host never installs them.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { readFileSync, realpathSync } from "node:fs";
import path from "node:path";
import { validateCleanHostMcp } from "./clean-host-mcp-receipt.mjs";
import { runCleanHostContainer } from "./clean-host-container.mjs";

const [appimageArg, debArg, sumsArg] = process.argv.slice(2);
if (!appimageArg || !debArg || !sumsArg)
  throw new Error(
    "Usage: node scripts/test-clean-host-packages.mjs APPIMAGE DEB SHA256SUMS",
  );
const appimage = realpathSync(appimageArg);
const deb = realpathSync(debArg);
const manifest = readFileSync(sumsArg, "utf8");
const sums = new Map(
  manifest
    .trim()
    .split("\n")
    .map((line) => {
      const match = /^([a-f0-9]{64})  ([^/\n]+)$/.exec(line);
      assert.ok(match, "Invalid SHA256SUMS entry");
      return [match[2], match[1]];
    }),
);
for (const file of [appimage, deb]) {
  const expected = sums.get(path.basename(file));
  assert.ok(expected, `No checksum for ${path.basename(file)}`);
  assert.equal(
    createHash("sha256").update(readFileSync(file)).digest("hex"),
    expected,
  );
}

const executable = process.env.SHADOW_CONTAINER_CLI || "docker";
const image =
  process.env.SHADOW_CLEAN_HOST_RUNTIME_IMAGE || "shadowcode-clean-runtime:ci";
const marker = "__SHADOW_CLEAN_STATUS__";
function container(args, label) {
  return runCleanHostContainer({
    executable,
    label,
    args: ["--network=none", "--security-opt=no-new-privileges", ...args],
  });
}
function statusContainer(args, label) {
  const output = container(args, label);
  validateCleanHostMcp(output);
  const start = output.indexOf(`${marker}\n`);
  assert.ok(start >= 0, `${label} did not print a status receipt`);
  const receipt = JSON.parse(output.slice(start + marker.length + 1));
  assert.equal(receipt.workspace, "/tmp/project");
  assert.equal(receipt.permissions.network, false);
  assert.deepEqual(receipt.jobs, []);
  console.log(
    `${label}: offline CLI status, MCP stdio and isolated project passed`,
  );
}

const common = [
  "--mount",
  `type=bind,src=${appimage},dst=/opt/ShadowCode.AppImage,readonly`,
];
statusContainer(
  [
    "--cap-drop=ALL",
    "--user",
    "1000:1000",
    "--env",
    "HOME=/tmp",
    "--env",
    "APPIMAGE_EXTRACT_AND_RUN=1",
    "--tmpfs",
    "/tmp:rw,exec,nosuid,nodev,size=512m,mode=1777",
    ...common,
    image,
    "sh",
    "-ceu",
    `
    ! command -v node; ! command -v cargo; ! command -v rustc
    mkdir -p /tmp/project /tmp/profile
    git -C /tmp/project init -q
    printf 'clean-host fixture\\n' > /tmp/project/README.md
    /opt/ShadowCode.AppImage --help >/dev/null
    /usr/local/bin/shadowcode-clean-mcp-smoke /opt/ShadowCode.AppImage /tmp/mcp-profile /tmp/mcp-project
    printf '${marker}\\n'
    /opt/ShadowCode.AppImage --profile /tmp/profile --workspace /tmp/project --json status
  `,
  ],
  "AppImage clean-host",
);

statusContainer(
  [
    "--mount",
    `type=bind,src=${deb},dst=/opt/ShadowCode.deb,readonly`,
    image,
    "sh",
    "-ceu",
    `
    ! command -v node; ! command -v cargo; ! command -v rustc
    dpkg -i /opt/ShadowCode.deb >/dev/null
    test -x /usr/bin/shadowcode
    test -f /usr/share/applications/com.shadowfetch.shadowcode.desktop
    test -f /usr/share/icons/hicolor/256x256/apps/shadowcode.png
    runuser -u nobody -- env HOME=/tmp sh -ceu '
      mkdir -p /tmp/project /tmp/profile
      git -C /tmp/project init -q
      printf "clean-host fixture\\n" > /tmp/project/README.md
      /usr/bin/shadowcode --help >/dev/null
      /usr/local/bin/shadowcode-clean-mcp-smoke /usr/bin/shadowcode /tmp/mcp-profile /tmp/mcp-project
      printf "${marker}\\n"
      /usr/bin/shadowcode --profile /tmp/profile --workspace /tmp/project --json status
    '
  `,
  ],
  "Debian package clean-host",
);

// Removing and purging the package takes only its own files: settings,
// history and projects stay, and nothing reaches the network.
const purgeMarker = "__SHADOW_PURGE_KEEPS_USER_DATA__";
assert.ok(
  container(
    [
      "--tmpfs",
      "/tmp:rw,nosuid,nodev,size=512m,mode=1777",
      "--mount",
      `type=bind,src=${deb},dst=/opt/ShadowCode.deb,readonly`,
      image,
      "sh",
      "-ceu",
      `
    dpkg -i /opt/ShadowCode.deb >/dev/null
    # The slim image excludes /usr/share/man; the package still lists it.
    dpkg -L shadow-code | grep -qx /usr/share/man/man1/shadowcode.1.gz
    test -s /usr/share/bash-completion/completions/shadowcode
    grep -q 'originally created by Shadowfetch' /usr/share/doc/shadow-code/copyright
    runuser -u nobody -- env HOME=/tmp/home sh -ceu '
      mkdir -p /tmp/home/project
      git -C /tmp/home/project init -q
      /usr/bin/shadowcode --workspace /tmp/home/project --json status >/dev/null
      /usr/bin/shadowcode --workspace /tmp/home/project config updates.check false >/dev/null
    '
    dpkg --purge shadow-code >/dev/null
    if dpkg -s shadow-code >/dev/null 2>&1; then exit 1; fi
    test ! -e /usr/bin/shadowcode
    test ! -e /usr/lib/shadowcode
    test ! -e /usr/share/doc/shadow-code
    test ! -e /usr/share/man/man1/shadowcode.1.gz
    grep -q 'check: false' /tmp/home/.config/shadow-agent/config.yaml
    test -f /tmp/home/.local/state/shadow-agent/shadow-agent.db
    test -d /tmp/home/project/.git
    printf '${purgeMarker}\\n'
  `,
    ],
    "Debian package purge",
  )
    .split("\n")
    .includes(purgeMarker),
  "Purging the Debian package must leave user data in place",
);
console.log("Debian package purge: packaged files removed, user data kept");

const guiMarker = "__SHADOW_CLEAN_WINDOW__";
function windowContainer(args, label) {
  const output = container(args, label);
  assert.ok(
    output.split("\n").includes(guiMarker),
    `${label} did not open a visible window`,
  );
  console.log(`${label}: first GUI window opened with a fresh profile`);
}
windowContainer(
  [
    "--cap-drop=ALL",
    "--user",
    "1000:1000",
    "--env",
    "HOME=/tmp",
    "--env",
    "GDK_BACKEND=x11",
    "--env",
    "APPIMAGE_EXTRACT_AND_RUN=1",
    "--tmpfs",
    "/tmp:rw,exec,nosuid,nodev,size=1g,mode=1777",
    "--shm-size=512m",
    ...common,
    image,
    "timeout",
    "75s",
    "xvfb-run",
    "-a",
    "-s",
    "-screen 0 1440x1100x24",
    "dbus-run-session",
    "--",
    "/usr/local/bin/shadowcode-clean-gui-smoke",
    "/opt/ShadowCode.AppImage",
  ],
  "AppImage clean-host GUI",
);
windowContainer(
  [
    "--env",
    "HOME=/tmp",
    "--env",
    "GDK_BACKEND=x11",
    "--tmpfs",
    "/tmp:rw,nosuid,nodev,size=1g,mode=1777",
    "--shm-size=512m",
    "--mount",
    `type=bind,src=${deb},dst=/opt/ShadowCode.deb,readonly`,
    image,
    "sh",
    "-ceu",
    "dpkg -i /opt/ShadowCode.deb >/dev/null; exec runuser -u nobody -- env HOME=/tmp GDK_BACKEND=x11 timeout 75s xvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- /usr/local/bin/shadowcode-clean-gui-smoke /usr/bin/shadowcode",
  ],
  "Debian package clean-host GUI",
);
