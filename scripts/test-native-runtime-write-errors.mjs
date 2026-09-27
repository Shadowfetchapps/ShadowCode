// Compile the actual pinned+patched extraction functions, with a fixture reader
// and scoped stdio failures. No AppImage, container, model or full runtime build.
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import { after, before, test } from "node:test";

const root = fileURLToPath(new URL("../", import.meta.url));
const exec = promisify(execFile);
const options = { timeout: 10000, maxBuffer: 2 * 1024 * 1024 };
let scratch;
let executable;

before(async () => {
  assert.equal(process.platform, "linux", "This qualifies the Linux runtime");
  const recipe = path.join(root, "packaging/native-runtime");
  const manifest = JSON.parse(
    await readFile(path.join(recipe, "manifest.json")),
  );
  assert.match(manifest.upstream.commit, /^[a-f0-9]{40}$/);
  assert.match(manifest.upstream.sha256, /^[a-f0-9]{64}$/);
  const archive = path.join(
    root,
    "target/native-runtime",
    `type2-runtime-${manifest.upstream.commit}.tar.gz`,
  );
  // An absent or changed archive is a failure, never an optional skipped gate.
  // The normal pinned runtime build obtains it before this focused test runs.
  const archiveBytes = await readFile(archive);
  assert.equal(
    createHash("sha256").update(archiveBytes).digest("hex"),
    manifest.upstream.sha256,
    "The upstream archive must match the reviewed source pin",
  );
  scratch = await mkdtemp(path.join(tmpdir(), "shadowcode-runtime-writes-"));
  const verifiedArchive = path.join(scratch, "upstream.tar.gz");
  await writeFile(verifiedArchive, archiveBytes);
  const sourceDirectory = path.join(scratch, "src/runtime");
  await mkdir(sourceDirectory, { recursive: true });
  const { stdout: upstream } = await exec(
    "tar",
    [
      "-xOf",
      verifiedArchive,
      `type2-runtime-${manifest.upstream.commit}/src/runtime/runtime.c`,
    ],
    options,
  );
  const sourceFile = path.join(sourceDirectory, "runtime.c");
  await writeFile(sourceFile, upstream);
  await exec(
    "patch",
    [
      "--batch",
      "--fuzz=0",
      "-p1",
      "-i",
      path.join(recipe, "isolated-extraction.patch"),
    ],
    { ...options, cwd: scratch },
  );
  const source = await readFile(sourceFile, "utf8");
  const start = source.indexOf("bool extract_appimage(");
  const end = source.indexOf("void build_mount_point(", start);
  assert.ok(
    start >= 0 && end > start,
    "Expected complete extraction/cleanup functions",
  );
  const functions = source.slice(start, end);
  assert.ok(functions.includes("bool rm_recursive("));
  assert.ok(functions.includes("int rm_recursive_callback("));
  // The actual extract-and-run caller must still refuse execution when this
  // function returns false. Keep this assumption visible if upstream changes.
  const gate = source.indexOf(
    "if (!extract_appimage(appimage_path, prefix, NULL, false, verbose)) {",
  );
  const fork = source.indexOf("if ((pid = fork())", gate);
  assert.ok(gate >= 0 && fork > gate);
  assert.ok(source.slice(gate, fork).includes("goto extraction_cleanup;"));
  await writeFile(path.join(scratch, "extraction_under_test.inc"), functions);
  executable = path.join(scratch, "runtime-write-errors");
  await exec(
    "cc",
    [
      "-std=c11",
      "-O2",
      "-Wall",
      "-Wextra",
      "-Werror",
      // These two comparisons are pre-existing in the pinned upstream/patch.
      "-Wno-sign-compare",
      "-I",
      scratch,
      path.join(root, "scripts/fixtures/native-runtime-write-errors.c"),
      "-Wl,--wrap=fopen",
      "-Wl,--wrap=fwrite",
      "-Wl,--wrap=fflush",
      "-Wl,--wrap=fclose",
      "-o",
      executable,
    ],
    { ...options, timeout: 30000 },
  );
});

after(async () => {
  if (scratch) await rm(scratch, { recursive: true, force: true });
});

for (const scenario of [
  "success",
  "short_write",
  "zero_write",
  "flush_error",
  "close_error",
  "short_and_close",
  "dev_full_unbuffered",
  "dev_full_buffered",
  "cancelled",
]) {
  test(`actual runtime extractor: ${scenario}`, async (t) => {
    const { stdout } = await exec(executable, [scenario, scratch], options);
    const result = JSON.parse(stdout);
    assert.equal(result.case, scenario);
    assert.equal(result.pass, true);
    assert.equal(result.extracted, scenario === "success");
    assert.equal(result.payload_attempts, scenario === "success" ? 1 : 0);
    assert.equal(result.payload_ran, scenario === "success");
    assert.equal(result.opened, result.closed);
    assert.equal(result.closed, scenario === "cancelled" ? 0 : 1);
    assert.equal(result.owned_removed, true);
    assert.equal(result.peer_preserved, true);
    t.diagnostic(JSON.stringify(result));
  });
}
