// Exercise exact CI packages in a disposable, network-disabled runtime image.
// The image contains only runtime dependencies; the host never installs them.
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import { readFileSync, realpathSync } from "node:fs";
import path from "node:path";

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
  const result = spawnSync(
    executable,
    [
      "run",
      "--rm",
      "--network=none",
      "--security-opt=no-new-privileges",
      ...args,
    ],
    {
      encoding: "utf8",
      timeout: 120_000,
      maxBuffer: 2 * 1024 * 1024,
    },
  );
  if (result.error || result.status !== 0)
    throw new Error(
      `${label} failed (${result.status}): ${result.error || ""}\n${result.stdout}\n${result.stderr}`,
    );
  return result.stdout;
}
function statusContainer(args, label) {
  const output = container(args, label);
  const start = output.indexOf(`${marker}\n`);
  assert.ok(start >= 0, `${label} did not print a status receipt`);
  const receipt = JSON.parse(output.slice(start + marker.length + 1));
  assert.equal(receipt.workspace, "/tmp/project");
  assert.equal(receipt.permissions.network, false);
  assert.deepEqual(receipt.jobs, []);
  console.log(
    `${label}: installed runtime, offline CLI status and isolated project passed`,
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
    "/tmp:rw,nosuid,nodev,size=512m,mode=1777",
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
      printf "${marker}\\n"
      /usr/bin/shadowcode --profile /tmp/profile --workspace /tmp/project --json status
    '
  `,
  ],
  "Debian package clean-host",
);

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
    "/tmp:rw,nosuid,nodev,size=1g,mode=1777",
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
