import assert from "node:assert/strict";
import { mkdtempSync, writeFileSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { runCleanHostContainer } from "./clean-host-container.mjs";

const ownedId = "a".repeat(64);
const unrelatedId = "b".repeat(64);
function fixture(mode, fn) {
  const scratch = mkdtempSync(path.join(tmpdir(), "shadow-container-owner-"));
  const stateFile = path.join(scratch, "state.json");
  const executable = path.join(scratch, "container-cli");
  writeFileSync(
    stateFile,
    JSON.stringify({ mode, unrelated: unrelatedId, owned: null, commands: [] }),
  );
  writeFileSync(
    executable,
    `#!${process.execPath}\n` +
      String.raw`
const fs = require("node:fs");
const file = process.env.SHADOW_CONTAINER_FIXTURE;
const state = JSON.parse(fs.readFileSync(file, "utf8"));
const args = process.argv.slice(2);
state.commands.push(args);
const save = () => fs.writeFileSync(file, JSON.stringify(state));
save();
const ownId = "a".repeat(64);
const hang = () => Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 120000);
if (args[0] === "run") {
  if (!args.includes("--pull=never")) process.exit(97);
  const name = args[args.indexOf("--name") + 1];
  const rawLabel = args[args.indexOf("--label") + 1];
  const [key, token] = rawLabel.split("=");
  state.owned = { Id: ownId, Name: "/" + name, Config: { Labels: { [key]: token } } };
  save();
  if (state.mode === "timeout") { process.on("SIGTERM", () => {}); hang(); }
  if (state.mode === "success") { state.owned = null; save(); console.log("package receipt"); process.exit(0); }
  if (["auto-remove-before-inspect", "remove-timeout-after-removal"].includes(state.mode)) { console.log("package receipt"); process.exit(0); }
  console.error("attached client failed");
  process.exit(17);
}
if (args[0] === "ps") {
  if (state.mode === "cleanup-timeout") hang();
  if (state.mode === "cleanup-error") { console.error("daemon unavailable"); process.exit(1); }
  if (state.mode === "invalid-id") { console.log("not-an-immutable-id"); process.exit(0); }
  if (state.mode === "foreign-container") { console.log(state.unrelated); process.exit(0); }
  if (!args.includes("--no-trunc") || !args.includes("--filter")) process.exit(99);
  const filter = args[args.indexOf("--filter") + 1];
  if (state.owned) {
    const [key, token] = filter.slice("label=".length).split("=");
    if (state.owned.Config.Labels[key] === token) console.log(state.owned.Id);
  }
  process.exit(0);
}
if (args[0] === "inspect") {
  if (state.mode === "auto-remove-before-inspect") { state.owned = null; save(); console.error("No such object"); process.exit(1); }
  if (state.mode === "foreign-container") { console.log(JSON.stringify({ Id: state.unrelated, Name: "/unrelated", Config: { Labels: {} } })); process.exit(0); }
  const view = structuredClone(state.owned);
  if (state.mode === "wrong-label") view.Config.Labels = {};
  if (state.mode === "wrong-name") view.Name = "/somebody-else";
  console.log(JSON.stringify(view));
  process.exit(0);
}
if (args[0] === "rm") {
  if (args[1] !== "--force" || args[2] !== ownId) process.exit(99);
  if (state.mode === "remove-failure") { console.error("cannot remove"); process.exit(1); }
  state.owned = null;
  save();
  if (state.mode === "remove-timeout-after-removal") hang();
  console.log(ownId);
  process.exit(0);
}
process.exit(98);
`,
    { mode: 0o755 },
  );
  try {
    fn({
      run: () =>
        runCleanHostContainer({
          executable,
          args: ["fixture-image"],
          label: "fixture",
          timeout: 400,
          cleanupTimeout: 800,
          env: { ...process.env, SHADOW_CONTAINER_FIXTURE: stateFile },
        }),
      state: () => JSON.parse(readFileSync(stateFile, "utf8")),
    });
  } finally {
    rmSync(scratch, { recursive: true, force: true });
  }
}

test("successful auto-removal needs no remove command and preserves output", () => {
  fixture("success", ({ run, state }) => {
    assert.equal(run(), "package receipt\n");
    assert.equal(state().unrelated, unrelatedId);
    assert.ok(!state().commands.some((args) => args[0] === "rm"));
  });
});

test("accepts auto-removal between listing and inspection only after confirming absence", () => {
  fixture("auto-remove-before-inspect", ({ run, state }) => {
    assert.equal(run(), "package receipt\n");
    assert.equal(state().owned, null);
    assert.equal(state().unrelated, unrelatedId);
    assert.deepEqual(
      state().commands.map((args) => args[0]),
      ["run", "ps", "inspect", "ps"],
    );
  });
});

test("reports timed-out removal client even when the daemon has removed the container", () => {
  fixture("remove-timeout-after-removal", ({ run, state }) => {
    assert.throws(run, /cleanup could not finish within its deadline/);
    assert.equal(state().owned, null);
    assert.equal(state().unrelated, unrelatedId);
    assert.equal(state().commands.at(-1)[0], "ps");
  });
});

for (const mode of ["timeout", "client-failure"]) {
  test(`${mode} removes only the verified owned immutable ID`, () => {
    fixture(mode, ({ run, state }) => {
      assert.throws(
        run,
        mode === "timeout" ? /ETIMEDOUT/ : /attached client failed/,
      );
      const result = state();
      assert.equal(result.owned, null);
      assert.equal(result.unrelated, unrelatedId);
      assert.deepEqual(
        result.commands.filter((args) => args[0] === "rm"),
        [["rm", "--force", ownedId]],
      );
      assert.equal(result.commands.at(-1)[0], "ps", "must verify removal");
    });
  });
}
for (const mode of [
  "wrong-label",
  "wrong-name",
  "foreign-container",
  "invalid-id",
]) {
  test(`refuses cleanup for ${mode}`, () => {
    fixture(mode, ({ run, state }) => {
      assert.throws(
        run,
        /identity\/ownership mismatch|ambiguous or invalid owned IDs/,
      );
      assert.equal(state().unrelated, unrelatedId);
      assert.ok(!state().commands.some((args) => args[0] === "rm"));
    });
  });
}
for (const mode of ["cleanup-error", "cleanup-timeout", "remove-failure"]) {
  test(`reports ${mode} without claiming container cleanup`, () => {
    fixture(mode, ({ run, state }) => {
      const start = Date.now();
      assert.throws(run, /cleanup command ps failed|remains after cleanup/);
      assert.ok(Date.now() - start < 5000, "cleanup commands must be bounded");
      assert.ok(state().owned);
      assert.equal(state().unrelated, unrelatedId);
    });
  });
}
