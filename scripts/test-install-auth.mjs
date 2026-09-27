import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, cp, mkdir, writeFile, readFile, rm, readlink, readdir, stat, chmod, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { provision, signFixture, COMMIT } from './installer-auth-fixtures.mjs';
import { assetNames, digestBytes, IDENTITY, createEnvelope, signEnvelope, publicKeyIdentity } from './native-release-auth.mjs';
import { prepareRelease } from './prepare-native-release.mjs';
import { signRelease } from './sign-native-release.mjs';
import { GATES, REQUIRED_GATES, scriptDigest } from './native-release-verification.mjs';

const sourceRoot = fileURLToPath(new URL('../', import.meta.url));
const runtimeCommit = 'ab'.repeat(20);
async function missing(file) { await assert.rejects(stat(file), { code: 'ENOENT' }); }
async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'shadowcode-install-auth-'));
  t.after(async () => { await chmod(root, 0o700); spawnSync('chmod', ['-R', 'u+w', root]); await rm(root, { recursive: true, force: true }); });
  const bundle = path.join(root, 'installer'), home = path.join(root, 'home'), key = path.join(root, 'fixture-private.pem');
  await mkdir(bundle); await mkdir(home);
  for (const file of ['scripts/install-appimage.sh', 'scripts/install-release-state.sh', 'scripts/native-release-auth-lib.sh', 'scripts/verify-native-release.sh', 'scripts/native-release-auth.mjs', 'scripts/installer-auth-fixtures.mjs', 'scripts/test-install-appimage.sh', 'assets/icons/shadow-agent.svg', 'packaging/shadow-agent.desktop']) {
    const destination = path.join(bundle, file); await mkdir(path.dirname(destination), { recursive: true }); await cp(path.join(sourceRoot, file), destination);
  }
  await provision(bundle, key);
  const libraryParent = path.join(home, '.local/lib'), library = path.join(libraryParent, 'shadowcode');
  const marker = path.join(root, 'candidate-executed');
  const installer = path.join(bundle, 'scripts/install-appimage.sh');
  const env = { ...process.env, HOME: home, XDG_DATA_HOME: path.join(home, '.local/share') };
  delete env.SHADOWCODE_SHA256SUMS; delete env.SHADOWCODE_GIT_SHA;
  const run = (args, extra = {}) => spawnSync('/bin/bash', [installer, ...args], { env: { ...env, ...extra }, encoding: 'utf8', timeout: 20000 });
  return { root, bundle, home, key, library, libraryParent, marker, installer, env, run, state: path.join(libraryParent, '.shadowcode-release-state'), journal: path.join(libraryParent, '.shadowcode-install-intent') };
}
async function candidate(f, version = '0.28.0', suffix = '') {
  const directory = path.join(f.root, `release-${version}${suffix}`); await mkdir(directory, { recursive: true });
  const file = path.join(directory, `ShadowCode_${version}_amd64.AppImage`);
  await writeFile(file, `#!/usr/bin/env bash
set -euo pipefail
printf '%s\\n' "$0" >> '${f.marker}'
if [[ "\${1:-}" == --appimage-extract-and-run && "\${2:-}" == --version ]]; then printf 'ShadowCode ${version}\\n'; exit 0; fi
if [[ "\${1:-}" == --appimage-extract && "\${2:-}" == usr/lib/shadowcode ]]; then
  directory=squashfs-root/usr/lib/shadowcode
  mkdir -p "$directory/NOTICES"
  printf '#!/bin/sh\\necho "version: fixture commit ${runtimeCommit.slice(0, 9)}"\\n' > "$directory/llama-server"
  chmod 755 "$directory/llama-server"
  printf 'commit=${runtimeCommit}\\n' > "$directory/COMMIT"
  printf 'llama\\n' > "$directory/architectures.txt"
  printf 'MIT fixture\\n' > "$directory/NOTICES/llama.cpp-LICENSE"
  printf 'library' > "$directory/library.so"
  exit 0
fi
exit 1
`, { mode: 0o755 });
  await signFixture(file, f.key); return file;
}
function passed(result) { assert.equal(result.error, undefined); assert.equal(result.status, 0, result.stderr || result.stdout); }
function refused(result) { assert.equal(result.error, undefined, 'Installer timed out'); assert.notEqual(result.status, 0, result.stdout); }
async function accepted(f) { return readlink(path.join(f.state, 'accepted')); }
async function faultWrapper(f) {
  const bin = path.join(f.root, 'fault-bin'); await mkdir(bin);
  await writeFile(path.join(bin, 'mv'), `#!/bin/bash
set -euo pipefail
source_path="\${@: -2:1}"
target_path="\${@: -1}"
boundary=''
[[ "$target_path" != "$HOME/.local/lib/.shadowcode-install-intent" ]] || boundary=intent
[[ "$target_path" != "$HOME/.local/lib/.shadowcode-release-state" ]] || boundary=initial-state
[[ "$source_path" != */receipt || "$target_path" != */records/* ]] || boundary=record
[[ "$target_path" != "$HOME/.local/lib/.shadowcode-release-state/accepted" ]] || boundary=accepted
[[ "$target_path" != "$HOME/.local/lib/shadowcode.previous" ]] || boundary=old-runtime
[[ "$source_path" != */squashfs-root/usr/lib/shadowcode ]] || boundary=new-runtime
[[ "$source_path" != "$HOME/.local/lib/.shadowcode-install-intent/phase.pending" ]] || boundary=activation
[[ "$target_path" != "$HOME/Applications/ShadowCode.AppImage" ]] || boundary=link
if [[ "$boundary" == "$AUTH_FAULT" && "$AUTH_FAULT_MODE" == fail ]]; then echo "Injected failure at $boundary" >&2; exit 74; fi
/usr/bin/mv "$@"
if [[ "$boundary" == "$AUTH_FAULT" && "$AUTH_FAULT_MODE" == kill ]]; then echo "Injected SIGKILL at $boundary" >&2; kill -KILL "$PPID"; fi
`, { mode: 0o755 });
  await writeFile(path.join(bin, 'sync'), `#!/bin/bash
set -euo pipefail
if [[ "$AUTH_FAULT" == accepted-sync && "\${@: -1}" == "$HOME/.local/lib/.shadowcode-release-state" ]]; then echo 'Injected accepted state flush failure' >&2; exit 75; fi
exec /usr/bin/sync "$@"
`, { mode: 0o755 });
  return { PATH: `${bin}:${process.env.PATH}`, AUTH_FAULT_MODE: 'kill' };
}

test('rejects jointly replaced checksums and candidate before any executable runs', async t => {
  const f = await fixture(t), file = await candidate(f);
  await writeFile(file, `${await readFile(file, 'utf8')}# changed\n`);
  const sum = spawnSync('sha256sum', [path.basename(file)], { cwd: path.dirname(file), encoding: 'utf8' }).stdout;
  await writeFile(path.join(path.dirname(file), 'SHA256SUMS'), sum);
  refused(f.run([file])); await missing(f.marker); await missing(f.state);
});
test('creates signed accepted state and executes only the verified private snapshot', async t => {
  const f = await fixture(t), file = await candidate(f);
  passed(f.run([file]));
  assert.match(await accepted(f), /^records\/[a-f0-9]{64}$/);
  for (const executed of (await readFile(f.marker, 'utf8')).trim().split('\n')) assert(executed.includes('/.shadowcode-install.') && executed.includes('/verified-release/'), 'Original download was executed');
  assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
  assert.match(await readFile(path.join(f.home, '.local/share/applications/shadow-agent.desktop'), 'utf8'), new RegExp(`X-ShadowCode-GitSha=${COMMIT}`));
  await missing(f.journal);
});
test('--unverified does not bypass missing publisher metadata', async t => {
  const f = await fixture(t), file = await candidate(f);
  await rm(path.join(path.dirname(file), 'RELEASE-AUTH.sig'));
  refused(f.run(['--unverified', file])); await missing(f.marker); await missing(f.state);
});
test('accepted version and identical retry survive; older or changed same-version signed releases fail', async t => {
  const f = await fixture(t), first = await candidate(f, '0.28.1');
  passed(f.run([first])); const receipt = await accepted(f);
  passed(f.run([first])); assert.equal(await accepted(f), receipt);
  const old = await candidate(f, '0.28.0'); await rm(f.marker); refused(f.run([old])); await missing(f.marker);
  const changed = await candidate(f, '0.28.1', '-changed'); await writeFile(changed, `${await readFile(changed, 'utf8')}# another signed build\n`); await signFixture(changed, f.key);
  refused(f.run([changed])); await missing(f.marker); assert.equal(await accepted(f), receipt);
});
test('missing managed state is refused while an unsigned legacy app can migrate forward', async t => {
  const f = await fixture(t), file = await candidate(f);
  await mkdir(path.join(f.home, 'Applications'), { recursive: true });
  await writeFile(path.join(f.home, 'Applications/ShadowCode-0.27.0-x86_64.AppImage'), 'legacy bytes');
  await symlink('ShadowCode-0.27.0-x86_64.AppImage', path.join(f.home, 'Applications/ShadowCode.AppImage'));
  passed(f.run([file])); await rm(f.state, { recursive: true }); await rm(f.marker);
  refused(f.run([file])); await missing(f.marker);
});
for (const boundary of ['initial-state', 'new-runtime']) test(`first-install SIGKILL at ${boundary} recovers absence without lowering accepted state`, async t => {
  const f = await fixture(t), file = await candidate(f), faults = await faultWrapper(f);
  const result = f.run([file], { ...faults, AUTH_FAULT: boundary }); refused(result); assert.match(result.stderr, /Injected SIGKILL/);
  const receipt = await accepted(f); await rm(path.dirname(file), { recursive: true });
  passed(f.run(['--recover'])); assert.equal(await accepted(f), receipt);
  await missing(f.library); await missing(path.join(f.home, 'Applications/ShadowCode.AppImage')); await missing(f.journal);
});
for (const boundary of ['record', 'accepted', 'old-runtime', 'new-runtime']) test(`upgrade SIGKILL at ${boundary} preserves the proven high-water and recovers prior runtime`, async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const prior = await accepted(f), faults = await faultWrapper(f);
  const result = f.run([next], { ...faults, AUTH_FAULT: boundary }); refused(result); assert.match(result.stderr, /Injected SIGKILL/);
  const current = await accepted(f); assert.equal(current === prior, boundary === 'record');
  await rm(path.dirname(next), { recursive: true }); passed(f.run(['--recover'])); assert.equal(await accepted(f), current);
  assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
  await missing(f.journal); await missing(`${f.library}.previous`);
  if (boundary !== 'record') { await rm(f.marker); refused(f.run([first])); await missing(f.marker); }
});
test('failed accepted-pointer publication does not mutate the previous runtime and remains recoverable', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const prior = await accepted(f), faults = await faultWrapper(f);
  refused(f.run([next], { ...faults, AUTH_FAULT: 'accepted', AUTH_FAULT_MODE: 'fail' }));
  assert.equal(await accepted(f), prior); passed(f.run(['--recover']));
  assert.equal(await accepted(f), prior); assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
});
test('activation marker recovers only while launcher and desktop identities remain unchanged', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const faults = await faultWrapper(f);
  refused(f.run([next], { ...faults, AUTH_FAULT: 'activation' })); const receipt = await accepted(f);
  const schema = path.join(f.journal, 'schema');
  await writeFile(schema, '2\n');
  const legacy = f.run(['--recover']); refused(legacy); assert.match(legacy.stderr, /activation already started/);
  await writeFile(schema, '3\n');
  const desktop = path.join(f.home, '.local/share/applications/shadow-agent.desktop');
  const prior = await readFile(desktop, 'utf8');
  await writeFile(desktop, `${prior}external edit\n`);
  const changed = f.run(['--recover']); refused(changed); assert.match(changed.stderr, /desktop integration changed after activation began/);
  await stat(f.journal); await stat(`${f.library}.previous`);
  await writeFile(desktop, prior);
  passed(f.run(['--recover'])); assert.equal(await accepted(f), receipt);
  assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
  await missing(f.journal); await missing(`${f.library}.previous`);
});
test('interruption after active-link replacement recovers only with unchanged desktop integration', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const faults = await faultWrapper(f);
  refused(f.run([next], { ...faults, AUTH_FAULT: 'link' })); const receipt = await accepted(f);
  const desktop = path.join(f.home, '.local/share/applications/shadow-agent.desktop');
  const priorDesktop = await readFile(desktop, 'utf8');
  await writeFile(desktop, `${priorDesktop}external edit\n`);
  const changed = f.run(['--recover']); refused(changed); assert.match(changed.stderr, /desktop integration changed after activation began/);
  assert.equal(await accepted(f), receipt); await stat(f.journal); await stat(`${f.library}.previous`);
  await writeFile(desktop, priorDesktop);
  passed(f.run(['--recover']));
  assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
  assert.equal(await accepted(f), receipt); await missing(f.journal); await missing(`${f.library}.previous`);
});
test('first-install interruption after active-link replacement restores recorded absence', async t => {
  const f = await fixture(t), first = await candidate(f), faults = await faultWrapper(f);
  refused(f.run([first], { ...faults, AUTH_FAULT: 'link' }));
  const receipt = await accepted(f);
  assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
  passed(f.run(['--recover']));
  await missing(path.join(f.home, 'Applications/ShadowCode.AppImage'));
  await missing(path.join(f.home, 'Applications/ShadowCode-0.28.0-x86_64.AppImage'));
  await missing(f.library); await missing(f.journal);
  assert.equal(await accepted(f), receipt);
});
test('state-pointer traversal and receipt tampering refuse execution and preserve evidence', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const pointer = await accepted(f), acceptedPath = path.join(f.state, 'accepted');
  await rm(f.marker); await rm(acceptedPath); await symlink('../../outside', acceptedPath);
  refused(f.run([next])); await missing(f.marker);
  await rm(acceptedPath); await symlink(pointer, acceptedPath);
  const auth = path.join(f.state, pointer, 'RELEASE-AUTH'); await chmod(auth, 0o600); await writeFile(auth, 'tampered'); await chmod(auth, 0o400);
  refused(f.run([next])); await missing(f.marker); assert.equal(await readFile(auth, 'utf8'), 'tampered');
});

test('legacy schema1 field set recovers without auth policy, state or original candidate', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const faults = await faultWrapper(f);
  refused(f.run([next], { ...faults, AUTH_FAULT: 'new-runtime' }));
  // All schema1 fields/identities are retained unchanged in schema2. Derive the
  // historical field set from a real interrupted replacement, without shipping
  // an extra unsigned installer executable in the repository.
  for (const field of ['accepted-prior', 'accepted-candidate', 'accepted-record-sha256', 'state-root']) await rm(path.join(f.journal, field));
  await writeFile(path.join(f.journal, 'schema'), '1\n');
  await rm(f.state, { recursive: true });
  await rm(path.dirname(next), { recursive: true }); await rm(path.join(f.bundle, 'release'), { recursive: true });
  await rm(f.marker); passed(f.run(['--recover'])); await missing(f.marker); await missing(f.state);
  assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
});
test('receipt recovery accepts a retained historical key after current epoch and version floors advance', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const faults = await faultWrapper(f);
  refused(f.run([next], { ...faults, AUTH_FAULT: 'new-runtime' })); const receipt = await accepted(f);
  const policy = path.join(f.bundle, 'release/trust/policy');
  await writeFile(policy, (await readFile(policy, 'utf8')).replace('minimum-epoch=1', 'minimum-epoch=2').replace('minimum-version=0.28.0', 'minimum-version=0.28.2'));
  await rm(f.marker); passed(f.run(['--recover'])); await missing(f.marker); assert.equal(await accepted(f), receipt);
  const result = f.run([next]); refused(result); assert.match(result.stderr, /retired signing epoch/); await missing(f.marker);
});
for (const [boundary, mode] of [['intent', 'kill'], ['initial-state', 'fail']]) test(`first-install ${mode} before accepted-state publication recovers recorded absence`, async t => {
  const f = await fixture(t), file = await candidate(f), faults = await faultWrapper(f);
  const result = f.run([file], { ...faults, AUTH_FAULT: boundary, AUTH_FAULT_MODE: mode }); refused(result); assert.match(result.stderr, /Injected/);
  await missing(f.state); await stat(f.journal); await missing(f.library);
  await rm(path.dirname(file), { recursive: true }); await rm(f.marker);
  passed(f.run(['--recover'])); await missing(f.marker); await missing(f.journal); await missing(f.state);
});
test('a replaced original download after staging is never executed or installed', async t => {
  const f = await fixture(t), file = await candidate(f), bin = path.join(f.root, 'copy-race-bin');
  const original = await readFile(file); await mkdir(bin);
  await writeFile(path.join(bin, 'chmod'), `#!/bin/bash
set -euo pipefail
if [[ "\${1:-}" == 500 && "\${2:-}" == */verified-release/*.AppImage ]]; then
  printf '#!/bin/sh\\nexit 91\\n' > "$AUTH_MUTATE_DOWNLOAD"
fi
exec /usr/bin/chmod "$@"
`, { mode: 0o755 });
  passed(f.run([file], { PATH: `${bin}:${process.env.PATH}`, AUTH_MUTATE_DOWNLOAD: file }));
  assert.notDeepEqual(await readFile(file), original);
  assert.deepEqual(await readFile(path.join(f.home, 'Applications/ShadowCode-0.28.0-x86_64.AppImage')), original);
  for (const executed of (await readFile(f.marker, 'utf8')).trim().split('\n')) assert(executed.includes('/verified-release/'));
});
test('original installer acceptance scenarios retain runtime, lock, rollback and recovery checks under signatures', async t => {
  const f = await fixture(t);
  const result = spawnSync('/bin/bash', [path.join(f.bundle, 'scripts/test-install-appimage.sh')], { env: f.env, encoding: 'utf8', timeout: 120000 });
  passed(result); assert.match(result.stdout, /AppImage installer checks passed/);
  assert.match(result.stdout, /SKIP installer-real-runtime/);
});

test('a flush failure after pointer advancement preserves the higher receipt and journal for recovery', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const prior = await accepted(f), faults = await faultWrapper(f);
  const result = f.run([next], { ...faults, AUTH_FAULT: 'accepted-sync' }); refused(result); assert.match(result.stderr, /Injected accepted state flush failure/);
  const current = await accepted(f); assert.notEqual(current, prior); await stat(f.journal);
  assert.equal(await readlink(path.join(f.home, 'Applications/ShadowCode.AppImage')), 'ShadowCode-0.28.0-x86_64.AppImage');
  passed(f.run(['--recover'])); assert.equal(await accepted(f), current);
  await rm(f.marker); refused(f.run([first])); await missing(f.marker);
});
test('schema2 receipt-binding tampering prevents recovery mutation and preserves every transaction artifact', async t => {
  const f = await fixture(t), first = await candidate(f), next = await candidate(f, '0.28.1');
  passed(f.run([first])); const faults = await faultWrapper(f);
  refused(f.run([next], { ...faults, AUTH_FAULT: 'new-runtime' }));
  const field = path.join(f.journal, 'accepted-record-sha256'), original = await readFile(field);
  const before = await readFile(path.join(f.library, 'COMMIT')), previous = await readFile(path.join(`${f.library}.previous`, 'COMMIT'));
  await writeFile(field, `${'0'.repeat(64)}\n`);
  const result = f.run(['--recover']); refused(result); assert.match(result.stderr, /candidate receipt bytes changed/);
  assert.deepEqual(await readFile(path.join(f.library, 'COMMIT')), before); assert.deepEqual(await readFile(path.join(`${f.library}.previous`, 'COMMIT')), previous);
  await stat(f.journal); await stat(path.join(f.home, 'Applications/ShadowCode-0.28.1-x86_64.AppImage'));
  await writeFile(field, original); passed(f.run(['--recover']));
});
test('missing provisioned trust or install cutover policy never falls back to unsigned execution', async t => {
  const f = await fixture(t), file = await candidate(f), policy = path.join(f.bundle, 'release/install-policy');
  const original = await readFile(policy); await rm(policy);
  let result = f.run([file]); refused(result); assert.match(result.stderr, /install policy is not provisioned/); await missing(f.marker);
  await writeFile(policy, original); await rm(path.join(f.bundle, 'release/trust'), { recursive: true });
  result = f.run([file]); refused(result); assert.match(result.stderr, /Publisher authentication failed/); await missing(f.marker); await missing(f.state);
});

test('symlink launcher cannot adopt substitute trust beside the link', async t => {
  const f = await fixture(t), file = await candidate(f), substitute = path.join(f.root, 'substitute');
  await cp(f.bundle, substitute, { recursive: true });
  const otherKey = path.join(f.root, 'other-fixture-private.pem');
  await provision(substitute, otherKey); await signFixture(file, otherKey);
  const launcher = path.join(substitute, 'scripts/install-appimage.sh');
  await rm(launcher); await symlink(f.installer, launcher);
  const result = spawnSync('/bin/bash', [launcher, file], { env: f.env, encoding: 'utf8', timeout: 20000 });
  refused(result); await missing(f.marker); await missing(f.state);
});
test('symlink launcher uses original installer trust even when link directory has no policy', async t => {
  const f = await fixture(t), file = await candidate(f), location = path.join(f.root, 'launcher/scripts');
  await mkdir(location, { recursive: true });
  const launcher = path.join(location, 'install-appimage.sh'); await symlink(f.installer, launcher);
  const result = spawnSync('/bin/bash', [launcher, file], { env: f.env, encoding: 'utf8', timeout: 20000 });
  passed(result); assert.match(await accepted(f), /^records\/[a-f0-9]{64}$/);
});
test('signed installer refuses --unverified explicitly even for a valid signed candidate', async t => {
  const f = await fixture(t), file = await candidate(f);
  const result = f.run(['--unverified', file]); refused(result);
  assert.match(result.stderr, /--unverified is not supported.*Publisher signatures are required/);
  await missing(f.marker); await missing(f.state);
});

test('a signed candidate below the fixed first-authenticated boundary never executes', async t => {
  const f = await fixture(t), file = await candidate(f);
  await writeFile(path.join(f.bundle, 'release/install-policy'), 'ShadowCode-Install-Policy-v1\nfirst-authenticated-version=0.28.1\n');
  const result = f.run([file]); refused(result); assert.match(result.stderr, /before the first authenticated release/);
  await missing(f.marker); await missing(f.state);
});


test('current publisher handoff and signer produce an installable authenticated bundle', async t => {
  const f = await fixture(t), file = await candidate(f), version = '0.28.0';
  const packages = assetNames(version).map(([, name]) => path.join(path.dirname(file), name));
  const artifacts = Object.fromEntries(await Promise.all(packages.map(async file => [path.basename(file), digestBytes(await readFile(file))])));
  const context = { repo: IDENTITY.repository, tag: `v${version}`, commit: COMMIT, runId: '991', runAttempt: '1' };
  // Synthetic passed receipts exercise format interoperability only; no CI
  // execution or real package qualification is claimed by this fixture.
  const verification = { schema: 1, commit: COMMIT, run_id: context.runId, run_attempt: context.runAttempt, artifacts,
    gates: Object.fromEntries(REQUIRED_GATES.map(gate => [gate, { schema: 1, gate, commit: COMMIT, run_id: context.runId, run_attempt: context.runAttempt,
      script_sha256: scriptDigest(gate), status: 'passed', exit_code: 0, required_skips: [], optional_checks: [], ...(GATES[gate].artifacts ? { artifacts } : {}),
    }])),
  };
  const unsigned = path.join(f.root, 'unsigned-handoff'), signed = path.join(f.root, 'signed-handoff');
  await prepareRelease({ files: packages, checksums: path.join(path.dirname(file), 'SHA256SUMS'), source: { cargo_lock_sha256: 'd'.repeat(64), ui_lock_sha256: 'e'.repeat(64), runtime_pin: `commit=${'f'.repeat(40)}` }, verification, context, outputDir: unsigned });
  await signRelease({ bundleDir: unsigned, trustDir: path.join(f.bundle, 'release/trust'), privateKey: await readFile(f.key), context, outputDir: signed });
  passed(f.run([path.join(signed, path.basename(file))]));
  const receipt = await accepted(f);
  assert.deepEqual(await readFile(path.join(f.state, receipt, 'RELEASE-AUTH')), await readFile(path.join(signed, 'RELEASE-AUTH')));
  assert.deepEqual(await readFile(path.join(f.home, 'Applications/ShadowCode-0.28.0-x86_64.AppImage')), await readFile(file));
});


test('candidate-adjacent and environment-provided trust cannot replace the installer bundle policy', async t => {
  const f = await fixture(t), file = await candidate(f), adjacent = path.dirname(file), otherKey = path.join(f.root, 'adjacent-fixture-private.pem');
  await provision(adjacent, otherKey); await signFixture(file, otherKey);
  const result = f.run([file], { ROOT: adjacent, TRUST: path.join(adjacent, 'release/trust'), SHADOWCODE_RELEASE_TRUST: path.join(adjacent, 'release/trust'), SHADOWCODE_INSTALL_POLICY: path.join(adjacent, 'release/install-policy') });
  refused(result); assert.match(result.stderr, /unknown signing key/); await missing(f.marker); await missing(f.state);
});

test('required installer CI gate retains runtime qualification and includes authentication fixtures', async () => {
  assert.equal(GATES.installer.script, 'node --test --test-reporter=tap scripts/test-install-auth.mjs\nbash scripts/test-install-appimage.sh');
  assert.equal(GATES.installer.artifacts, true); assert.equal(GATES.installer.unchanged, true);
  const native = await readFile(new URL('../.github/workflows/native.yml', import.meta.url), 'utf8');
  for (const file of ['scripts/install-release-state.sh', 'scripts/installer-auth-fixtures.mjs', 'scripts/test-install-auth.mjs', 'release/**']) assert(native.includes(`'${file}'`));
  for (const name of ['native', 'ci']) {
    const workflow = await readFile(new URL(`../.github/workflows/${name}.yml`, import.meta.url), 'utf8');
    assert.match(workflow, /node --test --test-reporter=tap scripts\/test-install-auth\.mjs\n\s+bash scripts\/test-install-appimage\.sh/);
  }
});


for (const role of ['deb', 'runtime-sources']) test(`signed ${role} asset never reaches AppImage execution`, async t => {
  const f = await fixture(t), app = await candidate(f), version = '0.28.0', directory = path.dirname(app);
  const target = path.join(directory, assetNames(version).find(([name]) => name === role)[1]);
  await writeFile(target, `#!/bin/sh\nprintf executed > '${f.marker}'\nexit 12\n`);
  const assets = Object.fromEntries(await Promise.all(assetNames(version).map(async ([, name]) => [name, digestBytes(await readFile(path.join(directory, name)))])));
  await writeFile(path.join(directory, 'SHA256SUMS'), Object.entries(assets).map(([name, hash]) => `${hash}  ${name}\n`).join(''));
  const manifest = JSON.parse(await readFile(path.join(directory, 'RELEASE-MANIFEST.json'))); manifest.assets = assets;
  await writeFile(path.join(directory, 'RELEASE-MANIFEST.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  const privatePem = await readFile(f.key), envelope = await createEnvelope({ bundleDir: directory, version, commit: COMMIT, keyId: publicKeyIdentity(privatePem), keyEpoch: 1 });
  await writeFile(path.join(directory, 'RELEASE-AUTH'), envelope); await writeFile(path.join(directory, 'RELEASE-AUTH.sig'), signEnvelope(envelope, privatePem));
  const result = f.run([target]); refused(result); assert.match(result.stderr, /Only the authenticated AppImage role/);
  await missing(f.marker); await missing(f.state);
});
