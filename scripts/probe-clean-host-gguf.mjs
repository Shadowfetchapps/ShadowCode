// Opt-in real inference qualification of both exact native packages in the
// offline Debian runtime image. Supply an already installed, small GGUF file;
// this script never downloads weights or changes the user's model store.
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { createReadStream } from "node:fs";
import { mkdir, mkdtemp, realpath, stat, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));
const [appimageArg, debArg, modelArg] = process.argv.slice(2);
if (!appimageArg || !debArg || !modelArg)
  throw new Error(
    "Usage: node scripts/probe-clean-host-gguf.mjs APPIMAGE DEB INSTALLED_GGUF",
  );
const [appimage, deb, model] = await Promise.all(
  [appimageArg, debArg, modelArg].map((name) => realpath(name)),
);
const cli = process.env.SHADOW_CONTAINER_CLI || "podman";
assert.equal(
  cli,
  "podman",
  "This opt-in probe requires Podman --userns=keep-id",
);
const image =
  process.env.SHADOW_CLEAN_HOST_RUNTIME_IMAGE || "shadowcode-clean-runtime:ci";
const artifacts = path.join(root, "artifacts/live-local");
await mkdir(artifacts, { recursive: true });
const runDir = await mkdtemp(path.join(artifacts, "clean-host-gguf-"));
const uid = process.getuid();
const gid = process.getgid();
const prompt =
  "In one short sentence, explain what def add(a, b): return a + b does. Do not call tools.";
const config = {
  local_engine: { files: ["/opt/model.gguf"], context_size: 4096 },
  network: { mode: "offline" },
  cli_agents: { enabled: false },
  trusted_workspaces: ["/work/project"],
  agent: { max_steps: 2, model_retries: 0 },
  model: {
    provider: "local",
    endpoint: "http://127.0.0.1:9/v1",
    name: "unused",
    context_limit: 4096,
  },
};

async function digest(file) {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest("hex");
}

function container(kind, work, command) {
  const pkg = kind === "appimage" ? appimage : deb;
  const name = kind === "appimage" ? "ShadowCode.AppImage" : "ShadowCode.deb";
  const args = [
    "run",
    "--rm",
    "--network=none",
    "--security-opt=no-new-privileges",
    "--userns=keep-id",
    "--user",
    kind === "appimage" ? `${uid}:${gid}` : "0:0",
    "--env",
    "HOME=/work/home",
    "--env",
    "XDG_DATA_HOME=/work/home/.local/share",
    "--env",
    "APPIMAGE_EXTRACT_AND_RUN=1",
    "--tmpfs",
    "/tmp:rw,nosuid,nodev,size=1g,mode=1777",
    "--shm-size=512m",
    "--mount",
    `type=bind,src=${pkg},dst=/opt/${name},readonly`,
    "--mount",
    `type=bind,src=${model},dst=/opt/model.gguf,readonly`,
    "--mount",
    `type=bind,src=${work},dst=/work`,
    image,
    "sh",
    "-ceu",
    command,
  ];
  if (kind === "appimage") args.splice(4, 0, "--cap-drop=ALL");
  const result = spawnSync(cli, args, {
    encoding: "utf8",
    timeout: 180_000,
    maxBuffer: 8 * 1024 * 1024,
  });
  if (result.error || result.status !== 0)
    throw new Error(
      `${kind} clean-host inference failed (${result.status}): ${result.error || ""}\n${result.stdout}\n${result.stderr}`,
    );
  return result.stdout;
}

function launch(kind, args) {
  const command =
    kind === "appimage"
      ? "/opt/ShadowCode.AppImage"
      : `setpriv --reuid=${uid} --regid=${gid} --clear-groups env HOME=/work/home XDG_DATA_HOME=/work/home/.local/share /usr/bin/shadowcode`;
  const prefix = "! command -v node; ! command -v cargo; ! command -v rustc; ";
  const install =
    kind === "deb" ? "dpkg -i /opt/ShadowCode.deb >/dev/null; " : "";
  return (
    prefix +
    install +
    command +
    " --profile /work/profile --workspace /work/project " +
    args
  );
}

async function probe(kind) {
  const work = path.join(runDir, kind);
  await mkdir(path.join(work, "profile/config"), { recursive: true });
  await mkdir(path.join(work, "project"), { recursive: true });
  await mkdir(path.join(work, "home"), { recursive: true });
  await writeFile(
    path.join(work, "profile/config/config.yaml"),
    JSON.stringify(config),
  );
  await writeFile(
    path.join(work, "project/calc.py"),
    "def add(a, b):\n    return a + b\n",
  );
  const git = spawnSync(
    "git",
    ["-C", path.join(work, "project"), "init", "-q"],
    {
      encoding: "utf8",
    },
  );
  assert.equal(git.status, 0, git.stderr);
  const catalogRaw = container(kind, work, launch(kind, "--json models"));
  await writeFile(path.join(work, "catalog.json"), catalogRaw);
  const catalog = JSON.parse(catalogRaw);
  assert.equal(catalog.local_engine.runtime.origin, "bundled");
  assert.equal(catalog.local_engine.runtime.state, "ready");
  const entry = catalog.local_engine.models.find(
    (row) => row.path === "/opt/model.gguf",
  );
  assert.ok(entry, "The mounted GGUF is absent from the packaged catalog");
  assert.equal(entry.availability, "ready", entry.reason);
  assert.match(entry.id, /^local:gguf:[a-f0-9]{64}$/);
  const eventsRaw = container(
    kind,
    work,
    launch(
      kind,
      `run --events --purpose reviewer --model ${entry.id} '${prompt}'`,
    ),
  );
  await writeFile(path.join(work, "events.ndjson"), eventsRaw);
  const lines = eventsRaw
    .trim()
    .split("\n")
    .map((line) => JSON.parse(line));
  const ready = lines.filter(
    (line) =>
      line.type === "event" && line.event?.type === "local.runtime_ready",
  );
  assert.equal(ready.length, 1, "Expected one actual local runtime launch");
  const runtime = ready[0].event.payload.runtime;
  assert.equal(runtime.backend, "cpu");
  assert.equal(runtime.cpu_fallback, false);
  assert.equal(runtime.provenance.files.model.path, "/opt/model.gguf");
  assert.match(
    runtime.provenance.files.runtime.path,
    /\/usr\/lib\/shadowcode\/llama-server$/,
  );
  const output = lines.filter((line) => line.type === "result");
  assert.equal(output.length, 1);
  const { result, exit_code: exitCode } = output[0];
  assert.equal(exitCode, 0);
  assert.equal(result.status, "completed");
  assert.equal(result.routing.route, "local_llamacpp");
  assert.equal(result.routing.model_id, entry.id);
  assert.equal(result.timings.model_requests, 1);
  assert.match(result.summary.toLowerCase(), /add|sum|a\s*\+\s*b/);
  assert.equal(result.result.verification.status, "not_run");
  return {
    package: path.basename(kind === "appimage" ? appimage : deb),
    sha256: await digest(kind === "appimage" ? appimage : deb),
    model_id: entry.id,
    runtime_origin: catalog.local_engine.runtime.origin,
    runtime_backend: runtime.backend,
    cpu_fallback: runtime.cpu_fallback,
    status: result.status,
    summary: result.summary,
    model_requests: result.timings.model_requests,
    verification: result.result.verification.status,
  };
}

const result = {
  environment: "offline Debian runtime image; no Node/Cargo/Rust in container",
  image,
  model: { bytes: (await stat(model)).size, sha256: await digest(model) },
  packages: [await probe("appimage"), await probe("deb")],
};
await writeFile(
  path.join(runDir, "result.json"),
  JSON.stringify(result, null, 2) + "\n",
);
console.log(
  JSON.stringify(
    { passed: true, report: path.join(runDir, "result.json"), ...result },
    null,
    2,
  ),
);
