// Bounded data-only staging shared by the build handoff, signer and publisher.
import assert from 'node:assert/strict';
import { constants } from 'node:fs';
import { open, mkdtemp, mkdir, rm, readFile, writeFile, readdir } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { assetNames, LIMITS, IDENTITY, verifyBundle } from './native-release-auth.mjs';
import { validateVerification } from './native-release-verification.mjs';

export const METADATA = Object.freeze(['SHA256SUMS', 'RELEASE-MANIFEST.json', 'RELEASE-AUTH', 'RELEASE-AUTH.sig']);
export const RECEIPTS_LIMIT = 1024 * 1024;
export const jsonBytes = value => Buffer.from(`${JSON.stringify(value, null, 2)}\n`);
export function releaseNames(version, signed = true) {
  return [...assetNames(version).map(([, name]) => name), ...METADATA.slice(0, signed ? 4 : 2)];
}
export function classifyFiles(files, version, signed = true) {
  const expected = releaseNames(version, signed);
  assert(Array.isArray(files), 'Required signed release assets missing');
  const result = new Map();
  for (const file of files) {
    assert.equal(typeof file, 'string');
    const name = path.basename(file);
    assert(expected.includes(name), `Unexpected release asset: ${name}`);
    assert(!result.has(name), `Duplicate release asset: ${name}`);
    result.set(name, file);
  }
  assert.deepEqual([...result.keys()].sort(), expected.sort(), 'Required signed release asset set mismatch');
  return result;
}
function limitFor(name) {
  return { 'SHA256SUMS': LIMITS.checksums, 'RELEASE-MANIFEST.json': LIMITS.manifest, 'RELEASE-AUTH': LIMITS.envelope, 'RELEASE-AUTH.sig': LIMITS.signature, 'VERIFICATION.json': RECEIPTS_LIMIT }[name] ?? LIMITS.artifact;
}
export async function snapshotFile(source, destination, limit) {
  const input = await open(source, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  let output;
  try {
    const before = await input.stat({ bigint: true });
    assert(before.isFile() && before.size > 0n && before.size <= BigInt(limit), 'Invalid snapshot file or size');
    output = await open(destination, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL, 0o400);
    const hash = createHash('sha256'), buffer = Buffer.alloc(65536);
    let size = 0;
    while (size <= Number(before.size)) {
      const { bytesRead } = await input.read(buffer, 0, Math.min(buffer.length, Number(before.size) + 1 - size), null);
      if (!bytesRead) break;
      size += bytesRead;
      assert(size <= Number(before.size) && size <= limit, 'Snapshot source grew');
      const bytes = buffer.subarray(0, bytesRead); hash.update(bytes); await output.writeFile(bytes);
    }
    const after = await input.stat({ bigint: true });
    assert(size === Number(before.size) && after.size === before.size && after.mtimeNs === before.mtimeNs && after.ctimeNs === before.ctimeNs, 'Snapshot source changed');
    return { hash: hash.digest('hex'), size };
  } finally { await output?.close(); await input.close(); }
}
export async function withReleaseSnapshot({ files, version, signed = true }, use) {
  const sources = classifyFiles(files, version, signed);
  const directory = await mkdtemp(path.join(tmpdir(), 'shadowcode-publisher-snapshot-'));
  try {
    const observed = new Map();
    for (const [name, file] of sources) observed.set(name, { file: path.join(directory, name), ...await snapshotFile(file, path.join(directory, name), limitFor(name)) });
    return await use(directory, observed);
  } finally { await rm(directory, { recursive: true, force: true }); }
}
export async function readBytes(file, limit) {
  const directory = await mkdtemp(path.join(tmpdir(), 'shadowcode-receipt-snapshot-'));
  try {
    const snapshot = path.join(directory, 'data'); await snapshotFile(file, snapshot, limit);
    return await readFile(snapshot);
  } finally { await rm(directory, { recursive: true, force: true }); }
}
export async function readJson(file, limit) {
  const bytes = await readBytes(file, limit), value = JSON.parse(bytes.toString('utf8'));
  assert(bytes.equals(jsonBytes(value)), 'Metadata must be canonical JSON without duplicate fields');
  return value;
}
export async function readReceipts(directory) { return readJson(path.join(directory, 'VERIFICATION.json'), RECEIPTS_LIMIT); }
export function validateContext({ repo, tag, commit, runId, runAttempt }) {
  assert.equal(repo, IDENTITY.repository, 'Wrong publisher repository');
  assert.match(commit, /^[a-f0-9]{40}$/);
  assert.match(tag, /^v(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$/);
  assetNames(tag.slice(1));
  assert.match(runId, /^[1-9][0-9]*$/, 'Expected workflow run required');
  assert.match(runAttempt, /^[1-9][0-9]*$/, 'Expected workflow attempt required');
  return tag.slice(1);
}
export async function validateStagedRelease({ directory, observed, verification, context, trustDir, signed }) {
  const version = validateContext(context);
  const packages = Object.fromEntries(assetNames(version).map(([, name]) => [name, observed.get(name).hash]));
  const gates = validateVerification(verification, context.commit, packages);
  assert.equal(verification.run_id, context.runId, 'Unexpected workflow run');
  assert.equal(verification.run_attempt, context.runAttempt, 'Unexpected workflow attempt');
  const manifest = await readJson(path.join(directory, 'RELEASE-MANIFEST.json'), LIMITS.manifest);
  assert.deepEqual(Object.keys(manifest).sort(), ['schema', 'tag', 'commit', 'target', 'cargo_lock_sha256', 'ui_lock_sha256', 'runtime_pin', 'verification', 'assets'].sort(), 'Manifest field set mismatch');
  assert.equal(manifest.schema, 1); assert.equal(manifest.tag, context.tag); assert.equal(manifest.commit, context.commit); assert.equal(manifest.target, IDENTITY.target);
  assert.match(manifest.cargo_lock_sha256, /^[a-f0-9]{64}$/); assert.match(manifest.ui_lock_sha256, /^[a-f0-9]{64}$/);
  assert(typeof manifest.runtime_pin === 'string' && manifest.runtime_pin.length > 0 && manifest.runtime_pin.length <= 65536, 'Invalid runtime pin');
  assert.deepEqual(manifest.assets, packages, 'Manifest package identities mismatch');
  assert.deepEqual(manifest.verification, gates, 'Signed manifest gate claims differ from required receipts');
  const sums = Buffer.from(Object.entries(packages).map(([name, hash]) => `${hash}  ${name}\n`).join(''));
  assert((await readFile(path.join(directory, 'SHA256SUMS'))).equals(sums), 'Checksums must contain the exact canonical package set');
  if (signed) {
    assert(trustDir, 'Publisher authentication requires independently trusted public keys');
    for (const [name] of Object.entries(packages)) await verifyBundle({ bundleDir: directory, trustDir, artifact: name, expectedVersion: version, expectedCommit: context.commit });
  }
  return { packages, gates, manifest };
}
export async function requireTransportLayout(directory, version, signed = false) {
  assert.deepEqual((await readdir(directory)).sort(), [...releaseNames(version, signed), 'VERIFICATION.json'].sort(), 'Unexpected transport file set');
  return releaseNames(version, signed).map(name => path.join(directory, name));
}
export async function retainBundle(directory, destination, names, verification) {
  await mkdir(destination, { mode: 0o700 }); // Refuse an existing output, including a symlink.
  try {
    for (const name of names) await snapshotFile(path.join(directory, name), path.join(destination, name), limitFor(name));
    await writeFile(path.join(destination, 'VERIFICATION.json'), jsonBytes(verification), { mode: 0o400, flag: 'wx' });
  } catch (error) { await rm(destination, { recursive: true, force: true }); throw error; }
}
