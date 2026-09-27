import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, mkdir, copyFile, readFile, writeFile, rm, chmod, utimes } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { GATES, REQUIRED_GATES, executeScript, inspectLine, readVerification, runGate as checkedRunGate, scriptDigest, validateVerification } from './native-release-verification.mjs';

// Recorder unit fixtures use synthetic commit IDs. The CLI fixtures below
// exercise the production source guard in actual disposable Git repositories.
const runGate = args => checkedRunGate({ verifySource: () => {}, ...args });

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

async function sourceFixture(directory) {
  const env = { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: '/dev/null' };
  const git = (...args) => {
    const result = spawnSync('git', ['-c', 'commit.gpgsign=false', '-c', 'core.hooksPath=/dev/null', ...args], { cwd: directory, env, encoding: 'utf8' });
    assert.equal(result.status, 0, result.stderr);
    return result.stdout.trim();
  };
  for (const name of ['scripts', 'ui', 'src-tauri', 'packaging', 'artifacts/bin']) await mkdir(path.join(directory, name), { recursive: true });
  await copyFile(new URL('./native-release-verification.mjs', import.meta.url), path.join(directory, 'scripts/native-release-verification.mjs'));
  await writeFile(path.join(directory, '.gitignore'), '/artifacts/\n');
  await writeFile(path.join(directory, 'tracked.txt'), 'committed source\n');
  await utimes(path.join(directory, 'tracked.txt'), 946684800, 946684800);
  await writeFile(path.join(directory, 'Cargo.toml'), 'version = "0.1.0"\n');
  await writeFile(path.join(directory, 'ui/package.json'), '{"version":"0.1.0"}\n');
  await writeFile(path.join(directory, 'src-tauri/tauri.conf.json'), '{"version":"0.1.0"}\n');
  await writeFile(path.join(directory, 'packaging/shadow-agent.desktop'), 'X-ShadowCode-Version=0.1.0\n');
  git('init', '--quiet'); git('config', 'user.name', 'Release fixture'); git('config', 'user.email', 'fixture@example.invalid');
  git('add', '.'); git('commit', '--quiet', '-m', 'Fixture');
  const original = git('rev-parse', 'HEAD');
  const quote = text => `'${text.replaceAll("'", "'\\''")}'`;
  const wrapper = path.join(directory, 'artifacts/bin/node');
  await writeFile(wrapper, `#!/bin/sh\nif [ ! -e artifacts/entered ]; then\n  touch artifacts/entered\n  case "$SOURCE_MUTATION" in\n    content) printf 'changed during check\\n' > tracked.txt ;;\n    head) git -c commit.gpgsign=false -c core.hooksPath=/dev/null commit --quiet --allow-empty -m 'Changed during check' ;;\n    restored-stat) ${quote(process.execPath)} -e 'const fs=require("node:fs"); fs.writeFileSync("tracked.txt", "X".repeat(fs.statSync("tracked.txt").size)); fs.utimesSync("tracked.txt",946684800,946684800)'; git status --porcelain > artifacts/stat-status ;;\n  esac\nfi\nexec ${quote(process.execPath)} "$@"\n`);
  await chmod(wrapper, 0o755);
  return {
    git,
    run(mutation = '') {
      return spawnSync(process.execPath, ['scripts/native-release-verification.mjs', 'run', 'release-tag'], {
        cwd: directory, encoding: 'utf8', timeout: 15000,
        env: { ...env, PATH: `${path.dirname(wrapper)}:${env.PATH}`, GITHUB_SHA: original, GITHUB_REF_NAME: 'v0.1.0', GITHUB_RUN_ID: '123', GITHUB_RUN_ATTEMPT: '2', SOURCE_MUTATION: mutation },
      });
    },
  };
}

test('release CLI accepts clean committed source and ignored build outputs', () => scratch(async directory => {
  const fixture = await sourceFixture(directory);
  const index = await readFile(path.join(directory, '.git/index'));
  const result = fixture.run();
  assert.equal(result.status, 0, result.stderr);
  assert.deepEqual(await readFile(path.join(directory, '.git/index')), index, 'source comparison must not rewrite the shared index');
  const receipt = JSON.parse(await readFile(path.join(directory, 'artifacts/release-verification/release-tag.json'), 'utf8'));
  assert.equal(receipt.status, 'passed');
}));

for (const kind of ['unstaged', 'staged', 'untracked', 'assume-unchanged', 'skip-worktree', 'restored-stat']) {
  test(`release CLI refuses ${kind} source before running a check`, () => scratch(async directory => {
    const fixture = await sourceFixture(directory);
    if (kind === 'untracked') await writeFile(path.join(directory, 'extra-source.rs'), '// not committed\n');
    else {
      if (kind === 'assume-unchanged' || kind === 'skip-worktree') fixture.git('update-index', `--${kind}`, 'tracked.txt');
      if (kind === 'restored-stat') {
        fixture.git('config', 'core.trustctime', 'false'); fixture.git('config', 'core.checkStat', 'minimal');
        await writeFile(path.join(directory, 'tracked.txt'), 'X'.repeat(Buffer.byteLength('committed source\n')));
        await utimes(path.join(directory, 'tracked.txt'), 946684800, 946684800);
        assert.equal(fixture.git('status', '--porcelain'), '', 'ordinary status can cache the restored-stat edit as clean');
      } else await writeFile(path.join(directory, 'tracked.txt'), 'not committed\n');
      if (kind === 'staged') fixture.git('add', 'tracked.txt');
    }
    const result = fixture.run();
    assert.notEqual(result.status, 0, 'dirty source must not receive a passing release receipt');
    await assert.rejects(readFile(path.join(directory, 'artifacts/entered')), /ENOENT/);
    const receipt = JSON.parse(await readFile(path.join(directory, 'artifacts/release-verification/release-tag.json'), 'utf8'));
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.exit_code, null);
  }));
}

for (const mutation of ['content', 'head', 'restored-stat']) {
  test(`release CLI refuses ${mutation} changes made during a successful check`, () => scratch(async directory => {
    const fixture = await sourceFixture(directory);
    if (mutation === 'restored-stat') {
      fixture.git('config', 'core.trustctime', 'false'); fixture.git('config', 'core.checkStat', 'minimal');
    }
    const result = fixture.run(mutation);
    assert.notEqual(result.status, 0, 'source changed while a release check ran');
    const receipt = JSON.parse(await readFile(path.join(directory, 'artifacts/release-verification/release-tag.json'), 'utf8'));
    assert.equal(receipt.status, 'failed');
    assert.equal(receipt.exit_code, 0, 'the underlying version check succeeded');
  }));
}

test('release CLI invalidates a previous receipt when HEAD already changed', () => scratch(async directory => {
  const fixture = await sourceFixture(directory);
  assert.equal(fixture.run().status, 0);
  fixture.git('commit', '--quiet', '--allow-empty', '-m', 'New head');
  assert.notEqual(fixture.run().status, 0);
  const receipt = JSON.parse(await readFile(path.join(directory, 'artifacts/release-verification/release-tag.json'), 'utf8'));
  assert.equal(receipt.status, 'failed');
  assert.equal(receipt.exit_code, null);
}));

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
test('GLib backport verification is a required release gate with no optional skipped regression', () => {
  assert(REQUIRED_GATES.includes('glib-backport'));
  const missing = complete(); delete missing.gates['glib-backport'];
  assert.throws(() => validateVerification(missing, commit, artifacts), /gate set mismatch/);
  for (const status of ['failed', 'skipped']) {
    const value = complete(); value.gates['glib-backport'].status = status;
    assert.throws(() => validateVerification(value, commit, artifacts), /did not pass: glib-backport/);
  }
  const skips = new Set(), optional = new Set();
  inspectLine('glib-backport', 'test next_and_next_back_share_one_bounded_cursor ... ignored', skips, optional);
  assert.equal(skips.size, 1);
  assert.equal(optional.size, 0);
});
test('clean-host package qualification is required and bound to the verified artifacts', () => {
  const gate = 'clean-host-packages';
  assert(REQUIRED_GATES.includes(gate));
  assert.match(GATES[gate].scope, /visible first GUI window.*GUI interaction.*remain separate/);
  assert.match(GATES[gate].script, /docker build -f scripts\/clean-host-runtime\.Dockerfile/);
  assert.match(GATES[gate].script, /test-clean-host-packages\.mjs.*SHA256SUMS/);
  const missing = complete(); delete missing.gates[gate];
  assert.throws(() => validateVerification(missing, commit, artifacts), /gate set mismatch/);
  for (const status of ['failed', 'skipped']) {
    const value = complete(); value.gates[gate].status = status;
    assert.throws(() => validateVerification(value, commit, artifacts), /did not pass: clean-host-packages/);
  }
  const changed = complete(); changed.gates[gate].artifacts['test.deb'] = 'f'.repeat(64);
  assert.throws(() => validateVerification(changed, commit, artifacts), /artifact mismatch: clean-host-packages/);
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
  inspectLine('native-source', 'test live_three_model_compare_cancels_queued_models ... ignored', skips, optional);
  inspectLine('native-window', '  ok  no Ready cloud row on this machine: consent step skipped', skips, optional);
  assert.equal(skips.size, 0); assert.equal(optional.size, 3);
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


test('built-project cleanup has a dedicated required UI scope and unexpected ignores remain refused', () => {
  const gate = 'built-project-cleanup';
  assert(REQUIRED_GATES.includes(gate));
  assert.match(GATES[gate].scope, /node_modules.*dist/);
  assert.match(GATES[gate].scope, /Cargo.*separate/);
  const fixture = 'worktrees::cleanup::tests::actual_built_project_cleanup_is_bounded_and_completes';
  assert(GATES['native-source'].script.endsWith(` -- --skip ${fixture}`));
  for (const name of ['native-source', gate]) {
    const skips = new Set(), optional = new Set();
    inspectLine(name, `test ${fixture} ... ignored, opt-in fixture`, skips, optional);
    assert.equal(skips.size, 1); assert.equal(optional.size, 0);
  }
  const missing = complete(); delete missing.gates[gate];
  assert.throws(() => validateVerification(missing, commit, artifacts), /gate set mismatch/);
  for (const status of ['failed', 'skipped']) {
    const value = complete(); value.gates[gate].status = status;
    assert.throws(() => validateVerification(value, commit, artifacts), /did not pass: built-project-cleanup/);
  }
});
test('built-project cleanup command requires one executed test and preserves cargo failure', () => scratch(async directory => {
  const script = GATES['built-project-cleanup']?.script;
  assert.equal(typeof script, 'string');
  const cargo = path.join(directory, 'cargo');
  await writeFile(cargo, '#!/bin/sh\nprintf "%s\\n" "$*" > "$CAPTURE"\nprintf "%s\\n" "$CARGO_TERM_COLOR/$SHADOWCODE_CLEANUP_BUILT_SCOPE/$SHADOWCODE_CLEANUP_BUILT_FIXTURE" >> "$CAPTURE"\nprintf "%s\\n" "$FIXTURE_OUTPUT"\nexit "$FIXTURE_EXIT"\n');
  await chmod(cargo, 0o755);
  const passed = 'test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.00s';
  for (const [output, code, succeeds] of [
    [passed, '0', true], [passed, '7', false],
    [passed.replace('1 passed', '0 passed'), '0', false],
    [passed.replace('0 ignored', '1 ignored'), '0', false],
    ['test result: FAILED. 0 passed; 1 failed; 0 ignored;', '0', false], ['', '0', false],
  ]) {
    const result = spawnSync('bash', ['-euo', 'pipefail', '-c', script], { cwd: directory, encoding: 'utf8', timeout: 10000,
      env: { ...process.env, PATH: `${directory}:${process.env.PATH}`, CAPTURE: path.join(directory, 'arguments'), GITHUB_WORKSPACE: directory, FIXTURE_OUTPUT: output, FIXTURE_EXIT: code },
    });
    assert.equal(result.status === 0, succeeds, `${output}: ${result.stderr}`);
  }
  const invocation = await readFile(path.join(directory, 'arguments'), 'utf8');
  assert.equal(invocation, `+1.95.0 test -p shadowcode-core --release --lib worktrees::cleanup::tests::actual_built_project_cleanup_is_bounded_and_completes --locked -- --ignored --exact --nocapture\nnever/ui/${directory}\n`);
}));


test('native CI retains auth-library triggers and runs built-project cleanup only after UI/toolchain setup', async () => {
  const workflow = await readFile(new URL('../.github/workflows/native.yml', import.meta.url), 'utf8');
  for (const file of ['scripts/native-release-auth-lib.sh', 'scripts/publisher-fixtures.mjs', 'scripts/test-publisher-auth.mjs']) assert(workflow.includes(`'${file}'`));
  assert.match(workflow, /run: node --test scripts\/test-native-release\.mjs scripts\/test-native-release-verification\.mjs scripts\/test-publisher-auth\.mjs scripts\/test-native-release-signing\.mjs/);
  const command = 'run: node scripts/native-release-verification.mjs run built-project-cleanup';
  const position = workflow.indexOf(command);
  assert(position > workflow.indexOf('npm --prefix ui ci && npm --prefix ui run build'));
  assert(position > workflow.indexOf('rustup toolchain install 1.95.0'));
  assert.equal(workflow.split(command).length, 2);
  assert.match(workflow, /cargo \+1\.95\.0 test --workspace --locked -- --skip worktrees::cleanup::tests::actual_built_project_cleanup_is_bounded_and_completes\n/);
});
