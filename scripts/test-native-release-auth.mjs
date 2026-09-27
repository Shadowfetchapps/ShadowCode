import test from 'node:test';
import assert from 'node:assert/strict';
import { generateKeyPairSync, sign } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, readFile, rm, stat, readdir, symlink, truncate } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { assetNames, createEnvelope, signEnvelope, verifyBundle, parseEnvelope, parseTrustPolicy, publicKeyIdentity, digestBytes, IDENTITY, LIMITS } from './native-release-auth.mjs';

const verifier = fileURLToPath(new URL('./verify-native-release.sh', import.meta.url));
function keyPair() {
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const privatePem = privateKey.export({ type: 'pkcs8', format: 'pem' });
  const publicPem = publicKey.export({ type: 'spki', format: 'pem' });
  return { privatePem, publicPem, id: publicKeyIdentity(publicPem) };
}
async function trustPolicy(directory, keys, { minimumVersion = '0.32.0', minimumEpoch = 1 } = {}) {
  await mkdir(directory, { recursive: true });
  const rows = ['ShadowCode-Release-Trust-v1', `repository=${IDENTITY.repository}`, `repository-id=${IDENTITY.repositoryId}`, `owner-id=${IDENTITY.ownerId}`, `target=${IDENTITY.target}`, `channel=${IDENTITY.channel}`, `minimum-epoch=${minimumEpoch}`, `minimum-version=${minimumVersion}`, 'keys=ed25519-spki-sha256'];
  for (const key of keys) {
    rows.push(`key=${key.epoch ?? 1}\t${key.id}\t${key.minimum ?? '0.0.0'}\t${key.maximum ?? '999999999.999999999.999999999'}`);
    await writeFile(path.join(directory, `${key.id}.pem`), key.publicPem);
  }
  await writeFile(path.join(directory, 'policy'), `${rows.join('\n')}\n`);
}
async function makeBundle(directory, key, { version = '0.32.0', commit = 'a'.repeat(40), epoch = 1, marker } = {}) {
  await mkdir(directory, { recursive: true });
  const hashes = {};
  for (const [role, name] of assetNames(version)) {
    const bytes = role === 'appimage' ? Buffer.from(`#!/bin/sh\nprintf executed > '${marker}'\n`) : Buffer.from(`fixture ${role} ${version} ${commit}\n`);
    hashes[name] = digestBytes(bytes);
    await writeFile(path.join(directory, name), bytes, { mode: 0o755 });
  }
  await writeFile(path.join(directory, 'SHA256SUMS'), Object.entries(hashes).map(([name, hash]) => `${hash}  ${name}\n`).join(''));
  await writeFile(path.join(directory, 'RELEASE-MANIFEST.json'), `${JSON.stringify({ schema: 1, tag: `v${version}`, commit, target: IDENTITY.target, assets: hashes }, null, 2)}\n`);
  const bytes = await createEnvelope({ bundleDir: directory, version, commit, keyId: key.id, keyEpoch: epoch });
  await writeFile(path.join(directory, 'RELEASE-AUTH'), bytes);
  await writeFile(path.join(directory, 'RELEASE-AUTH.sig'), signEnvelope(bytes, key.privatePem));
  return { version, commit, artifact: assetNames(version)[0][1] };
}
async function fixture(t) {
  const root = await mkdtemp(path.join(tmpdir(), 'shadowcode-auth-test-'));
  t.after(() => rm(root, { recursive: true, force: true }));
  const key = keyPair(), bundleDir = path.join(root, 'bundle'), trustDir = path.join(root, 'trust'), marker = path.join(root, 'candidate-executed');
  await trustPolicy(trustDir, [key]);
  const release = await makeBundle(bundleDir, key, { marker });
  return { root, key, bundleDir, trustDir, marker, ...release };
}
function shell(f, extra = {}) {
  const stageDir = extra.stageDir ?? path.join(f.root, 'verified-stage');
  const args = [verifier, '--bundle-dir', extra.bundleDir ?? f.bundleDir, '--trust-dir', extra.trustDir ?? f.trustDir, '--artifact', extra.artifact ?? f.artifact, '--stage-dir', stageDir];
  for (const [option, field] of [['--previous-dir', 'previousDir'], ['--expect-version', 'expectedVersion'], ['--expect-commit', 'expectedCommit']]) if (extra[field] !== undefined) args.push(option, extra[field]);
  return { ...spawnSync('/bin/bash', args, { encoding: 'utf8', timeout: 15000, env: { ...process.env, LC_ALL: 'C', ...extra.env } }), stageDir };
}
async function notExists(file) { await assert.rejects(stat(file), { code: 'ENOENT' }); }
async function accepted(f, extra = {}) {
  const metadata = await verifyBundle({ ...f, ...extra });
  const result = shell(f, extra);
  assert.equal(result.status, 0, result.stderr || String(result.error));
  assert.match(result.stdout, /Publisher signature verified/);
  await notExists(f.marker);
  const artifact = extra.artifact ?? f.artifact;
  assert.deepEqual(await readFile(path.join(result.stageDir, artifact)), await readFile(path.join(extra.bundleDir ?? f.bundleDir, artifact)));
  assert.equal((await stat(path.join(result.stageDir, artifact))).mode & 0o777, 0o400);
  assert.equal((await stat(result.stageDir)).mode & 0o777, 0o700);
  assert.deepEqual((await readdir(result.stageDir)).sort(), [artifact, 'RELEASE-AUTH', 'RELEASE-AUTH.sig', 'RELEASE-MANIFEST.json', 'SHA256SUMS'].sort());
  return { metadata, result };
}
async function refused(f, extra = {}, reason) {
  await assert.rejects(verifyBundle({ ...f, ...extra }));
  const result = shell(f, extra);
  assert.notEqual(result.status, 0, 'Shell verifier accepted invalid release');
  assert(!result.error, `Unexpected process failure: ${result.error}`);
  if (reason) assert.match(result.stderr, reason);
  await notExists(result.stageDir);
  await notExists(f.marker);
  assert(!(await readdir(f.root)).some(name => name.startsWith('.shadowcode-auth.')), 'Failed staging directory leaked');
}
async function mutateEnvelope(f, mutate, { resign = true, directory = f.bundleDir } = {}) {
  const next = Buffer.from(mutate((await readFile(path.join(directory, 'RELEASE-AUTH'))).toString()));
  await writeFile(path.join(directory, 'RELEASE-AUTH'), next);
  if (resign) await writeFile(path.join(directory, 'RELEASE-AUTH.sig'), sign(null, next, f.key.privatePem));
}

test('Node and actual OpenSSL verify the full binding without executing an executable candidate', async t => {
  const f = await fixture(t);
  const { metadata, result } = await accepted(f, { expectedVersion: f.version, expectedCommit: f.commit });
  assert.equal(metadata.assets.length, 3);
  const snapshot = await readFile(path.join(result.stageDir, f.artifact));
  await writeFile(path.join(f.bundleDir, f.artifact), 'download changed after verification');
  assert.deepEqual(await readFile(path.join(result.stageDir, f.artifact)), snapshot, 'Verified stage depends on mutable source');
  await notExists(f.marker);
});
test('all three artifact roles verify independently; consumers need only the selected artifact', async t => {
  const f = await fixture(t);
  for (const [role, artifact] of assetNames(f.version)) await accepted(f, { artifact, stageDir: path.join(f.root, `stage-${role}`) });
  for (const [, name] of assetNames(f.version).slice(1)) await rm(path.join(f.bundleDir, name));
  await accepted(f);
});
test('missing production trust is refused; no adjacent-key fallback exists', async t => {
  const f = await fixture(t);
  await rm(f.trustDir, { recursive: true });
  await refused(f, {}, /trusted key directory/);
});
test('missing OpenSSL fails closed before staging or candidate execution', async t => {
  const f = await fixture(t), runtimePath = path.join(f.root, 'empty-runtime-path');
  await mkdir(runtimePath);
  const result = shell(f, { env: { PATH: runtimePath } });
  assert.notEqual(result.status, 0); assert.match(result.stderr, /OpenSSL 3 is required/);
  await notExists(result.stageDir); await notExists(f.marker);
});
test('the offline shell verifier needs only its declared runtime utilities, without developer tools', async t => {
  const f = await fixture(t), runtimePath = path.join(f.root, 'runtime-path');
  await mkdir(runtimePath);
  for (const binary of ['openssl', 'realpath', 'dirname', 'basename', 'mktemp', 'rm', 'stat', 'dd', 'chmod', 'tr', 'wc', 'tail', 'od', 'sha256sum', 'cut', 'cmp', 'mv']) {
    await symlink(`/usr/bin/${binary}`, path.join(runtimePath, binary));
  }
  await accepted(f, { env: { PATH: runtimePath } });
});
test('unknown key and wrong public-key bytes cannot satisfy the pin', async t => {
  const f = await fixture(t), other = keyPair();
  await trustPolicy(f.trustDir, [other]);
  await refused(f, {}, /unknown signing key/);
  await trustPolicy(f.trustDir, [f.key]);
  await writeFile(path.join(f.trustDir, `${f.key.id}.pem`), other.publicPem);
  await refused(f, {}, /fingerprint mismatch/);
});
test('a non-Ed25519 public key is refused', async t => {
  const f = await fixture(t), pair = generateKeyPairSync('ec', { namedCurve: 'prime256v1' });
  await writeFile(path.join(f.trustDir, `${f.key.id}.pem`), pair.publicKey.export({ type: 'spki', format: 'pem' }));
  await refused(f, {}, /only Ed25519/);
});
test('verification rejects matching private-key containers and extra PEM blocks; signing still uses private PEM', async t => {
  const f = await fixture(t), file = path.join(f.trustDir, `${f.key.id}.pem`);
  for (const pem of [f.key.privatePem, `${f.key.publicPem}${f.key.privatePem}`, `${f.key.publicPem}${f.key.publicPem}`]) {
    await writeFile(file, pem); await refused(f, {}, /public key|public SPKI/);
  }
  await writeFile(file, f.key.publicPem);
  await accepted(f);
});
test('modified, truncated, oversized and missing signatures are refused', async t => {
  const f = await fixture(t), file = path.join(f.bundleDir, 'RELEASE-AUTH.sig'), original = await readFile(file);
  for (const bytes of [Buffer.from(original.map((b, i) => i === 0 ? b ^ 1 : b)), original.subarray(0, 63), Buffer.concat([original, Buffer.from('x')])]) {
    await writeFile(file, bytes); await refused(f);
  }
  await rm(file); await refused(f);
});
test('changed package bytes are refused even at the same length', async t => {
  const f = await fixture(t), file = path.join(f.bundleDir, f.artifact), bytes = await readFile(file);
  bytes[bytes.length - 1] ^= 1; await writeFile(file, bytes);
  await refused(f, {}, /artifact digest\/size mismatch/);
});
test('jointly replaced package and SHA256SUMS do not authenticate the replacement', async t => {
  const f = await fixture(t), file = path.join(f.bundleDir, f.artifact), original = await readFile(file), replacement = Buffer.from(original);
  replacement[replacement.length - 1] ^= 1; await writeFile(file, replacement);
  const sums = path.join(f.bundleDir, 'SHA256SUMS');
  await writeFile(sums, (await readFile(sums, 'utf8')).replace(digestBytes(original), digestBytes(replacement)));
  await refused(f, {}, /checksums digest mismatch/);
});
test('the exact audit-manifest bytes are authenticated', async t => {
  const f = await fixture(t), file = path.join(f.bundleDir, 'RELEASE-MANIFEST.json');
  await writeFile(file, `${await readFile(file, 'utf8')} `);
  await refused(f, {}, /manifest digest mismatch/);
});
for (const [name, from, to] of [
  ['repository', 'repository=Shadowfetchapps/ShadowCode', 'repository=another/ShadowCode'],
  ['repository ID', 'repository-id=1377099349', 'repository-id=1377099350'],
  ['owner ID', 'owner-id=209457103', 'owner-id=209457104'],
  ['target', 'target=x86_64-unknown-linux-gnu', 'target=aarch64-unknown-linux-gnu'],
  ['channel', 'channel=stable', 'channel=preview'],
  ['tag', 'tag=v0.32.0', 'tag=v0.32.1'],
  ['version format', 'version=0.32.0', 'version=00.32.0'],
  ['commit format', `commit=${'a'.repeat(40)}`, 'commit=HEAD'],
]) test(`even a valid signature cannot authorize the wrong ${name}`, async t => {
  const f = await fixture(t); await mutateEnvelope(f, bytes => bytes.replace(from, to)); await refused(f);
});
test('explicit version and commit expectations bind caller intent', async t => {
  const f = await fixture(t);
  await refused(f, { expectedVersion: '0.33.0' }, /unexpected version/);
  await refused(f, { expectedCommit: 'b'.repeat(40) }, /unexpected commit/);
});
for (const [name, mutate] of [
  ['extra field', bytes => `${bytes}url=https://evil.invalid/package\n`],
  ['duplicate field', bytes => bytes.replace('owner-id=209457103', 'repository-id=1377099349')],
  ['out-of-order fields', bytes => bytes.replace('key-id=', 'key-epoch=')],
  ['missing final newline', bytes => bytes.slice(0, -1)],
  ['NUL', bytes => `${bytes.slice(0, -1)}\0\n`],
  ['CRLF', bytes => bytes.replaceAll('\n', '\r\n')],
  ['Unicode', bytes => bytes.replace('stable', 'stablé')],
  ['oversized metadata', bytes => `${bytes}${'x'.repeat(4096)}\n`],
  ['traversal asset', bytes => bytes.replace('ShadowCode_0.32.0_amd64.AppImage\t', '../ShadowCode_0.32.0_amd64.AppImage\t')],
  ['duplicate asset', bytes => bytes.replace(/asset=deb[^\n]+/, bytes.split('\n')[13])],
  ['extra asset column', bytes => bytes.replace(/(asset=appimage[^\n]+)/, '$1\textra')],
  ['extra empty asset column', bytes => bytes.replace(/(asset=appimage[^\n]+)/, '$1\t')],
  ['oversized artifact declaration', bytes => bytes.replace(/(asset=appimage\t[^\t]+\t)\d+/, '$134359738369')],
]) test(`strict envelope rejects ${name}`, async t => {
  const f = await fixture(t); await mutateEnvelope(f, mutate); await refused(f);
});
test('a signed checksum file still must name exactly the canonical three assets', async t => {
  const f = await fixture(t), sums = path.join(f.bundleDir, 'SHA256SUMS');
  const replacement = Buffer.from(`${await readFile(sums, 'utf8')}${'0'.repeat(64)}  extra.AppImage\n`);
  await writeFile(sums, replacement);
  await mutateEnvelope(f, bytes => bytes.replace(/checksums-sha256=[a-f0-9]{64}/, `checksums-sha256=${digestBytes(replacement)}`));
  await refused(f, {}, /checksums asset set mismatch/);
});
test('manifest construction rejects duplicate JSON keys instead of silently selecting one', async t => {
  const f = await fixture(t), file = path.join(f.bundleDir, 'RELEASE-MANIFEST.json');
  await writeFile(file, (await readFile(file, 'utf8')).replace('"schema": 1,', '"schema": 0,\n  "schema": 1,'));
  await assert.rejects(createEnvelope({ ...f, keyId: f.key.id, keyEpoch: 1 }), /canonical JSON/);
});
test('unknown, duplicate and oversized trust fields are refused', async t => {
  const f = await fixture(t), file = path.join(f.trustDir, 'policy'), original = await readFile(file, 'utf8');
  for (const content of [`${original}anything=allowed\n`, `${original}${original.split('\n').at(-2)}\n`, `${original}${'x'.repeat(4096)}\n`]) {
    await writeFile(file, content); await refused(f);
  }
});
test('trusted version floor, epoch floor and key version ranges are enforced', async t => {
  const f = await fixture(t);
  await trustPolicy(f.trustDir, [f.key], { minimumVersion: '0.33.0' }); await refused(f, {}, /version floor/);
  await trustPolicy(f.trustDir, [f.key], { minimumEpoch: 2 }); await refused(f, {}, /retired signing epoch/);
  await trustPolicy(f.trustDir, [{ ...f.key, minimum: '0.33.0' }]); await refused(f, {}, /key version interval/);
  await trustPolicy(f.trustDir, [{ ...f.key, maximum: '0.31.0' }]); await refused(f, {}, /key version interval/);
});
test('signed older release cannot replace a newer accepted release', async t => {
  const f = await fixture(t), previousDir = path.join(f.root, 'previous');
  await makeBundle(previousDir, f.key, { version: '0.33.0', marker: f.marker });
  await refused(f, { previousDir }, /release downgrade/);
});
test('identical signed retry is permitted; another signed release at the same version is refused', async t => {
  const f = await fixture(t);
  await accepted(f, { previousDir: f.bundleDir });
  await rm(path.join(f.root, 'verified-stage'), { recursive: true });
  const previousDir = path.join(f.root, 'previous');
  await makeBundle(previousDir, f.key, { commit: 'b'.repeat(40), marker: f.marker });
  await refused(f, { previousDir }, /changed release under accepted version/);
});
test('accepted-history tampering is detected cryptographically', async t => {
  const f = await fixture(t), previousDir = path.join(f.root, 'previous');
  await makeBundle(previousDir, f.key, { marker: f.marker });
  await mutateEnvelope(f, bytes => bytes.replace('a'.repeat(40), 'b'.repeat(40)), { directory: previousDir, resign: false });
  await refused(f, { previousDir }, /invalid release signature/);
});
test('reviewed key rotation can retain old keys for history without accepting them for new installs', async t => {
  const f = await fixture(t), next = keyPair(), previousDir = path.join(f.root, 'previous');
  await makeBundle(previousDir, f.key, { marker: f.marker });
  await trustPolicy(f.trustDir, [{ ...f.key, maximum: '0.32.9' }, { ...next, epoch: 2, minimum: '0.33.0' }], { minimumEpoch: 2, minimumVersion: '0.33.0' });
  await refused(f, {}, /retired signing epoch/);
  const current = await makeBundle(f.bundleDir, next, { version: '0.33.0', epoch: 2, marker: f.marker });
  await accepted({ ...f, ...current }, { previousDir });
});
test('newer version cannot roll back the accepted signing epoch', async t => {
  const f = await fixture(t), next = keyPair(), previousDir = path.join(f.root, 'previous');
  await trustPolicy(f.trustDir, [f.key, { ...next, epoch: 2 }]);
  await makeBundle(previousDir, next, { version: '0.33.0', epoch: 2, marker: f.marker });
  const current = await makeBundle(f.bundleDir, f.key, { version: '0.34.0', marker: f.marker });
  await refused({ ...f, ...current }, { previousDir }, /signing epoch rollback/);
});
test('symlinks are refused for downloaded metadata, artifacts and trusted public key', async t => {
  const f = await fixture(t);
  for (const file of [path.join(f.bundleDir, 'RELEASE-AUTH'), path.join(f.bundleDir, f.artifact), path.join(f.trustDir, `${f.key.id}.pem`)]) {
    const bytes = await readFile(file), target = `${file}.source`;
    await writeFile(target, bytes); await rm(file); await symlink(target, file);
    await refused(f, {}, /regular non-symlink/);
    await rm(file); await writeFile(file, bytes);
  }
});
test('a stable FIFO input is refused without blocking or executing it', async t => {
  const f = await fixture(t), file = path.join(f.bundleDir, f.artifact);
  await rm(file);
  assert.equal(spawnSync('/usr/bin/mkfifo', [file]).status, 0);
  await refused(f, {}, /regular non-symlink/);
});
for (const kind of ['symlink', 'fifo']) test(`snapshot refuses a ${kind} swapped after pathname validation`, async t => {
  const f = await fixture(t), source = path.join(f.bundleDir, f.artifact), runtimePath = path.join(f.root, 'race-runtime');
  // A PATH wrapper synchronizes the real filesystem race immediately before
  // real dd opens the input. Production code contains no fixture-only hooks.
  await mkdir(runtimePath);
  const seen = path.join(f.root, 'swap-seen'), replacement = path.join(f.root, 'replacement');
  await writeFile(replacement, await readFile(source));
  await writeFile(path.join(runtimePath, 'dd'), `#!/bin/bash
for argument in "$@"; do
  if [[ "$argument" == "if=$AUTH_TEST_SWAP_SOURCE" && ! -e "$AUTH_TEST_SWAP_SEEN" ]]; then
    /usr/bin/rm -- "$AUTH_TEST_SWAP_SOURCE"
    if [[ "$AUTH_TEST_SWAP_KIND" == symlink ]]; then /usr/bin/ln -s -- "$AUTH_TEST_SWAP_REPLACEMENT" "$AUTH_TEST_SWAP_SOURCE"; else /usr/bin/mkfifo -- "$AUTH_TEST_SWAP_SOURCE"; fi
    /usr/bin/touch -- "$AUTH_TEST_SWAP_SEEN"
  fi
done
exec /usr/bin/dd "$@"
`, { mode: 0o755 });
  const result = shell(f, { env: { PATH: `${runtimePath}:${process.env.PATH}`, AUTH_TEST_SWAP_SOURCE: source, AUTH_TEST_SWAP_KIND: kind, AUTH_TEST_SWAP_REPLACEMENT: replacement, AUTH_TEST_SWAP_SEEN: seen } });
  assert.equal(result.error, undefined, 'Swapped input blocked until the test timeout');
  assert.notEqual(result.status, 0); await stat(seen);
  await notExists(result.stageDir); await notExists(f.marker);
  assert(!(await readdir(f.root)).some(name => name.startsWith('.shadowcode-auth.')));
});
test('selected artifact path traversal and unauthenticated names are refused', async t => {
  const f = await fixture(t);
  for (const artifact of ['../outside.AppImage', 'another.AppImage']) await refused(f, { artifact });
});
test('a pre-existing stage is preserved and never overwritten', async t => {
  const f = await fixture(t), stageDir = path.join(f.root, 'verified-stage');
  await mkdir(stageDir); await writeFile(path.join(stageDir, 'owned'), 'preserve');
  const result = shell(f, { stageDir }); assert.notEqual(result.status, 0); assert.match(result.stderr, /already exists/);
  assert.equal(await readFile(path.join(stageDir, 'owned'), 'utf8'), 'preserve'); await notExists(f.marker);
});
test('oversize manifest/checksums and sparse oversized candidate are bounded before copying', async t => {
  const f = await fixture(t), manifest = path.join(f.bundleDir, 'RELEASE-MANIFEST.json'), original = await readFile(manifest);
  await writeFile(manifest, Buffer.alloc(LIMITS.manifest + 1)); await refused(f); await writeFile(manifest, original);
  const sums = path.join(f.bundleDir, 'SHA256SUMS'), originalSums = await readFile(sums);
  await writeFile(sums, Buffer.alloc(LIMITS.checksums + 1)); await refused(f); await writeFile(sums, originalSums);
  await truncate(path.join(f.bundleDir, f.artifact), LIMITS.artifact + 1); await refused(f, {}, /input size limit/);
});
test('parser rejects unsupported schemas and signing refuses a different key', async t => {
  const f = await fixture(t), bytes = await readFile(path.join(f.bundleDir, 'RELEASE-AUTH'));
  assert.throws(() => parseEnvelope(Buffer.from(bytes.toString().replace('Auth-v1', 'Auth-v2'))));
  assert.throws(() => parseTrustPolicy(Buffer.from('ShadowCode-Release-Trust-v2\n')));
  assert.throws(() => signEnvelope(bytes, keyPair().privatePem), /fingerprint mismatch/);
});
