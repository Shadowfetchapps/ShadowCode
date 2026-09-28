// Deterministic PATH for AppImage/Debian packaging.
// linuxdeploy walks every PATH directory and calls boost::filesystem::status
// on each entry. A caller PATH that includes /usr/local/bin/node →
// /root/.hermes/... dies with Permission denied. Packaging must construct
// PATH itself; it must not depend on the human sanitizing the shell.
import { lstatSync, readlinkSync, realpathSync, statSync } from "node:fs";
import { chmod, copyFile, mkdir, readFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import path from "node:path";
import { homedir } from "node:os";
import { fileURLToPath } from "node:url";

export const SYSTEM_PACKAGING_DIRS = ["/usr/bin", "/bin", "/usr/sbin", "/sbin"];
const BLOCKED_DIRS = new Set([
  "/usr/local/bin",
  "/usr/local/sbin",
  "/snap/bin",
]);

function resolveExistingDir(dir) {
  try {
    const resolved = path.resolve(dir);
    const st = lstatSync(resolved);
    if (st.isDirectory()) return resolved;
    // Debian /bin → /usr/bin: keep the PATH name linuxdeploy will scan.
    if (st.isSymbolicLink() && statSync(resolved).isDirectory())
      return resolved;
    return null;
  } catch {
    return null;
  }
}

function entryResolvesToHijack(file) {
  try {
    const st = lstatSync(file);
    if (st.isSymbolicLink()) {
      const dest = readlinkSync(file);
      if (dest.includes(".hermes") || dest.startsWith("/root/")) return true;
      try {
        const real = realpathSync(file);
        return (
          real.includes(`${path.sep}.hermes${path.sep}`) ||
          real.startsWith("/root/")
        );
      } catch {
        // Broken node/npm links are the linuxdeploy Permission-denied case.
        return true;
      }
    }
    return false;
  } catch {
    return false;
  }
}

export function isUnsafePackagingDir(dir) {
  const resolved = resolveExistingDir(dir);
  if (!resolved) return true;
  if (BLOCKED_DIRS.has(resolved)) return true;
  if (resolved.split(path.sep).includes(".hermes")) return true;
  if (resolved === "/root" || resolved.startsWith(`/root${path.sep}`))
    return true;
  return ["node", "npm", "npx"].some((name) =>
    entryResolvesToHijack(path.join(resolved, name)),
  );
}

export function packagingDirs(root, options = {}) {
  const execDir = options.execDir ?? path.dirname(process.execPath);
  // Rustup installs compiler/package-manager proxies here, including on GitHub
  // runners. Keep the selected Cargo home without inheriting the caller PATH.
  const cargoHome =
    options.cargoHome ??
    process.env.CARGO_HOME ??
    path.join(homedir(), ".cargo");
  const extras = [
    path.join(root, "tools/rust-dev/extracted/usr/bin"),
    path.join(root, "..", "tools/rust-dev/extracted/usr/bin"),
    path.join(root, "target/release"),
    path.join(root, "target/debug"),
    path.join(root, "target/.tauri"),
    execDir,
    path.join(cargoHome, "bin"),
  ];
  const seen = new Set();
  const dirs = [];
  for (const candidate of [...extras, ...SYSTEM_PACKAGING_DIRS]) {
    const resolved = resolveExistingDir(candidate);
    if (!resolved || seen.has(resolved) || isUnsafePackagingDir(resolved))
      continue;
    seen.add(resolved);
    dirs.push(resolved);
  }
  return dirs;
}

export function packagingPath(root, options = {}) {
  return packagingDirs(root, options).join(":");
}

export function applyPackagingPath(root, options = {}) {
  const next = packagingPath(root, options);
  process.env.PATH = next;
  return next;
}

/** Environment for one `tauri bundle` run. Tauri's AppImage step lets
 * linuxdeploy's AppImage plugin download the newest upstream type2-runtime
 * ("continuous"), so the intermediate image changed whenever upstream
 * published one and the runtime check in build-native.mjs failed. Pin it to
 * the reviewed, source-built runtime that the final repack uses anyway.
 *
 * That intermediate image still has Tauri's `ShadowCode.desktop`; the desktop
 * ID the AppStream metadata names only exists after build-native.mjs
 * normalizes the AppDir. Some appimagetool builds validate the whole tree and
 * reject the intermediate image for it, so validation is skipped here only.
 * The final repack validates the metadata against the normalized launcher,
 * and check-native-package.mjs validates it strictly in both packages. */
export function bundleEnvironment(format, runtimeFile) {
  if (format !== "appimage") return {};
  if (!runtimeFile || !path.isAbsolute(runtimeFile))
    throw new Error("The AppImage bundle needs the absolute path of the pinned runtime");
  return { LDAI_RUNTIME_FILE: runtimeFile, LDAI_NO_APPSTREAM: "1" };
}

/** linuxdeploy's GTK plugin, reviewed at upstream commit 7a3fbc31 (its MIT
 * license is pinned in licenses/native/sources.json). Tauri downloads the
 * plugin from upstream master whenever `target/.tauri` lacks it, so a fresh
 * machine and a machine with an older cached copy bundled different bytes. */
export const GTK_PLUGIN = "packaging/linuxdeploy/linuxdeploy-plugin-gtk.sh";
export const GTK_PLUGIN_SHA256 =
  "b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171";

/** Put the reviewed GTK plugin where Tauri looks for it, after checking its
 * digest, so every build uses the same script. */
export async function installPinnedGtkPlugin(root) {
  const bytes = await readFile(path.join(root, GTK_PLUGIN));
  const digest = createHash("sha256").update(bytes).digest("hex");
  if (digest !== GTK_PLUGIN_SHA256)
    throw new Error(`${GTK_PLUGIN} does not match its reviewed SHA-256`);
  const tools = path.join(root, "target/.tauri");
  await mkdir(tools, { recursive: true });
  const target = path.join(tools, "linuxdeploy-plugin-gtk.sh");
  await copyFile(path.join(root, GTK_PLUGIN), target);
  await chmod(target, 0o755);
  return target;
}

const self = fileURLToPath(import.meta.url);
if (process.argv[1] && path.resolve(process.argv[1]) === self) {
  const root = process.argv[3]
    ? path.resolve(process.argv[3])
    : path.resolve(path.dirname(self), "..");
  if (process.argv[2] === "--print") process.stdout.write(packagingPath(root));
  else if (process.argv[2] === "--apply")
    process.stdout.write(applyPackagingPath(root));
  else {
    console.error(
      "Usage: node scripts/native-packaging-env.mjs --print|--apply [repo-root]",
    );
    process.exit(2);
  }
}
