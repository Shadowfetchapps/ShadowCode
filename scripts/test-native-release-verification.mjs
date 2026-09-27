import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { GATES, REQUIRED_GATES, executeScript, inspectLine, readVerification, runGate, scriptDigest, validateVerification } from './native-release-verification.mjs';

const commit = 'a'.repeat(40);
const artifacts = { 'test.AppImage': 'b'.repeat(64), 'test.deb': 'c'.repeat(64), 'sources.tar.gz': 'd'.repeat(64) };
function complete() {
  return { schema: 1, commit, run_id: '123', run_attempt: '2', artifacts: { ...artifacts }, gates: Object.fromEntries(REQUIRED_GATES.map(gate => [gate, {
    schema: 1, gate, commit, run_id: '123', run_attempt: '2', script_sha256: scriptDigest(gate), status: 'passed', exit_code: 0, required_skips: [], optional_checks: [], ...(GATES[gate].artifacts ? { artifacts: { ...artifacts } } : {}),
  }])) };
}
async function scratch(fn) {
  const directory = await mkdtemp(path.join(tmpdir(), 'release-verification-'));
  try { await fn(directory); } finally { await rm(directory, { recursive: true, force: true }); }
}

test('all required passed receipts produce a scoped verification result', () => {
  const verified = validateVerification(complete(), commit, artifacts);
  assert.equal(verified.status, 'required_gates_passed');
  assert.deepEqual(Object.keys(verified.gates), REQUIRED_GATES);
  assert.match(verified.scope, /excluded checks are not verified/);
  assert(!JSON.stringify(verified).includes('123'), 'run identity is validated but must not make immutable manifest retries nondeterministic');
});

for (const status of ['failed', 'skipped', 'running']) {
  test(`required ${status} receipt is refused`, () => {
    const value = complete(); value.gates['native-behavior'].status = status;
    assert.throws(() => validateVerification(value, commit, artifacts), /did not pass/);
  });
}
test('missing gate, hidden skip and failed exit cannot be claimed passed', () => {
  const missing = complete(); delete missing.gates['native-window'];
  assert.throws(() => validateVerification(missing, commit, artifacts), /gate set mismatch/);
  const skipped = complete(); skipped.gates.interface.required_skips = ['Tests 1 skipped'];
  assert.throws(() => validateVerification(skipped, commit, artifacts), /checks skipped/);
  const failed = complete(); failed.gates.interface.exit_code = 1;
  assert.throws(() => validateVerification(failed, commit, artifacts), /command failed/);
});
test('receipts are bound to commit, workflow attempt, exact command and artifact bytes', () => {
  for (const [field, changed, error] of [
    ['commit', 'e'.repeat(40), /Stale/], ['run_id', '999', /Wrong workflow run/],
    ['run_attempt', '1', /Wrong workflow attempt/], ['script_sha256', '0'.repeat(64), /command changed/],
  ]) {
    const value = complete(); value.gates.packages[field] = changed;
    assert.throws(() => validateVerification(value, commit, artifacts), error);
  }
  const value = complete(); value.gates['packaged-behavior'].artifacts['test.AppImage'] = 'f'.repeat(64);
  assert.throws(() => validateVerification(value, commit, artifacts), /artifact mismatch/);
  assert.throws(() => validateVerification(complete(), commit, { ...artifacts, 'test.AppImage': '0'.repeat(64) }), /package set or bytes changed/);
  assert.throws(() => validateVerification(complete(), commit, { ...artifacts, extra: '0'.repeat(64) }), /package set or bytes changed/);
});
test('missing or malformed receipt files fail closed', () => scratch(async directory => {
  await assert.rejects(readVerification(directory, commit, '123', '2', artifacts), /receipt missing or invalid/);
  for (const [gate, receipt] of Object.entries(complete().gates)) await writeFile(path.join(directory, `${gate}.json`), JSON.stringify(receipt));
  assert.equal((await readVerification(directory, commit, '123', '2', artifacts)).commit, commit);
  await writeFile(path.join(directory, 'packages.json'), '{broken');
  await assert.rejects(readVerification(directory, commit, '123', '2', artifacts), /receipt missing or invalid: packages/);
}));

test('recorder refuses command failures and required skips even with exit zero', () => scratch(async directory => {
  for (const [code, line, status] of [[7, '', 'failed'], [0, 'ok 3 - runtime # SKIP unavailable', 'skipped'], [0, '# skipped 1', 'skipped'], [0, 'Tests 3 passed | 1 skipped (4)', 'skipped'], [0, 'SKIP installer-real-runtime: not built', 'skipped']]) {
    await assert.rejects(runGate({ gate: 'interface', directory, commit, runId: '123', attempt: '2', execute: async (_script, inspect) => { inspect(line); return code; } }), new RegExp(status));
    const receipt = JSON.parse(await readFile(path.join(directory, 'interface.json'), 'utf8'));
    assert.equal(receipt.status, status);
    assert.equal(receipt.exit_code, code);
  }
}));
test('rerun invalidates old success before work and records thrown failures', () => scratch(async directory => {
  const args = { gate: 'interface', directory, commit, runId: '123', attempt: '2' };
  await runGate({ ...args, execute: async () => 0 });
  await assert.rejects(runGate({ ...args, execute: async () => {
    await assert.rejects(readFile(path.join(directory, 'interface.json')), /ENOENT/);
    throw new Error('worker exited');
  } }), /worker exited/);
  assert.equal(JSON.parse(await readFile(path.join(directory, 'interface.json'), 'utf8')).status, 'failed');
}));
test('package checks bind observed bytes and fail if they change during execution', () => scratch(async directory => {
  let calls = 0;
  await assert.rejects(runGate({ gate: 'packaged-behavior', directory, commit, runId: '123', attempt: '2', execute: async () => 0,
    hashes: async () => ++calls === 1 ? artifacts : { ...artifacts, 'test.AppImage': 'f'.repeat(64) },
  }), /Packages changed/);
  const receipt = await runGate({ gate: 'packaged-behavior', directory, commit, runId: '123', attempt: '2', execute: async () => 0, hashes: async () => artifacts });
  assert.deepEqual(receipt.artifacts, artifacts);
}));
test('only explicitly scoped optional checks are excluded and their omissions are recorded', () => {
  const skips = new Set(), optional = new Set();
  inspectLine('native-source', 'test live_install_and_status_of_the_real_server ... ignored', skips, optional);
  inspectLine('native-window', '  ok  no Ready cloud row on this machine: consent step skipped', skips, optional);
  assert.equal(skips.size, 0); assert.equal(optional.size, 2);
  inspectLine('managed-runtime', 'ok 1 - built runtime # SKIP not built', skips, optional);
  assert.equal(skips.size, 1);
  inspectLine('native-source', 'test required_cancellation_regression ... ignored', skips, optional);
  assert.equal(skips.size, 2, 'newly ignored tests must not silently enter the optional scope');
  inspectLine('interface', '\x1b[2K\x1b[32m Tests \x1b[0m 10 passed | 1 skipped (11)', skips, optional);
  assert.equal(skips.size, 3, 'terminal decoration must not hide skipped checks');
});
test('actual zero-exit subprocess output cannot hide a skipped required integration check', () => scratch(async directory => {
  await assert.rejects(runGate({ gate: 'managed-runtime', directory, commit, runId: '123', attempt: '2',
    execute: (_script, inspect) => executeScript("printf 'ok 1 - unavailable runtime # SKIP not built\\n'", inspect),
  }), /skipped/);
  const receipt = JSON.parse(await readFile(path.join(directory, 'managed-runtime.json'), 'utf8'));
  assert.equal(receipt.exit_code, 0);
  assert.equal(receipt.status, 'skipped');
}));
test('every release gate is invoked once by the workflow and every command script parses', async () => {
  const workflow = await readFile(new URL('../.github/workflows/release.yml', import.meta.url), 'utf8');
  const invoked = [...workflow.matchAll(/run: node scripts\/native-release-verification\.mjs run ([\w-]+)/g)].map(match => match[1]);
  assert.deepEqual(invoked, REQUIRED_GATES);
  for (const [gate, { script }] of Object.entries(GATES)) {
    const result = spawnSync('bash', ['-n'], { input: script, encoding: 'utf8' });
    assert.equal(result.status, 0, `${gate}: ${result.stderr}`);
  }
  assert(GATES.packages.script.indexOf('build-native.mjs') < GATES.packages.script.indexOf('test-native-packaging-env.mjs'), 'linuxdeploy qualification must run after the helper is built/cached');
});
