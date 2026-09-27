// Offline publisher authentication helpers. Production trust provisioning and installer/publisher integration are separate.
import assert from 'node:assert/strict';
import { createHash, createPublicKey, sign, verify } from 'node:crypto';
import { constants } from 'node:fs';
import { open } from 'node:fs/promises';
import path from 'node:path';

export const IDENTITY = Object.freeze({ repository: 'Shadowfetchapps/ShadowCode', repositoryId: '1377099349', ownerId: '209457103', target: 'x86_64-unknown-linux-gnu', channel: 'stable' });
export const LIMITS = Object.freeze({ envelope: 4096, trust: 4096, manifest: 1024 * 1024, checksums: 4096, publicKey: 1024, signature: 64, artifact: 32 * 1024 ** 3, keys: 8 });
const HEX = /^[a-f0-9]{64}$/;
const VERSION = /^(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})\.(0|[1-9][0-9]{0,8})$/;
const EPOCH = /^[1-9][0-9]{0,8}$/;
const HASH = bytes => createHash('sha256').update(bytes).digest('hex');
export { HASH as digestBytes };

function lines(bytes, limit, label) {
  assert(Buffer.isBuffer(bytes), `${label}: bytes required`);
  assert(bytes.length > 0 && bytes.length <= limit, `${label}: size limit`);
  assert(bytes.every(byte => byte === 10 || byte === 9 || (byte >= 32 && byte <= 126)), `${label}: non-ASCII or control byte`);
  assert(bytes.at(-1) === 10, `${label}: final newline required`);
  return bytes.toString('ascii').slice(0, -1).split('\n');
}
function field(line, name) {
  assert(typeof line === 'string' && line.startsWith(`${name}=`), `Expected ${name}`);
  const value = line.slice(name.length + 1);
  assert(value.length && !/[\t=]/.test(value), `Invalid ${name}`);
  return value;
}
export function compareVersions(a, b) {
  assert.match(a, VERSION, 'Invalid stable version');
  assert.match(b, VERSION, 'Invalid stable version');
  assert.equal(a.trim(), a, 'Version whitespace'); assert.equal(b.trim(), b, 'Version whitespace');
  const left = a.split('.').map(Number), right = b.split('.').map(Number);
  for (let i = 0; i < 3; i++) if (left[i] !== right[i]) return left[i] < right[i] ? -1 : 1;
  return 0;
}
export function assetNames(version) {
  assert.match(version, VERSION, 'Invalid stable version');
  assert.equal(version.trim(), version, 'Version whitespace');
  return [
    ['appimage', `ShadowCode_${version}_amd64.AppImage`],
    ['deb', `ShadowCode_${version}_amd64.deb`],
    ['runtime-sources', `ShadowCode_${version}_appimage-runtime-sources.tar.gz`],
  ];
}
function fixedIdentity(rows, offset = 1) {
  assert.equal(field(rows[offset], 'repository'), IDENTITY.repository, 'Wrong publisher repository');
  assert.equal(field(rows[offset + 1], 'repository-id'), IDENTITY.repositoryId, 'Wrong repository ID');
  assert.equal(field(rows[offset + 2], 'owner-id'), IDENTITY.ownerId, 'Wrong owner ID');
}
export function parseEnvelope(bytes) {
  const rows = lines(bytes, LIMITS.envelope, 'Envelope');
  assert.equal(rows.length, 16, 'Envelope field count');
  assert.equal(rows[0], 'ShadowCode-Release-Auth-v1', 'Envelope schema');
  fixedIdentity(rows);
  const version = field(rows[4], 'version');
  assert.match(version, VERSION);
  assert.equal(field(rows[5], 'tag'), `v${version}`, 'Version/tag mismatch');
  const commit = field(rows[6], 'commit');
  assert.match(commit, /^[a-f0-9]{40}$/);
  assert.equal(field(rows[7], 'target'), IDENTITY.target, 'Wrong target');
  assert.equal(field(rows[8], 'channel'), IDENTITY.channel, 'Wrong channel');
  const keyId = field(rows[9], 'key-id'), keyEpoch = field(rows[10], 'key-epoch');
  assert.match(keyId, HEX); assert.match(keyEpoch, EPOCH);
  const manifestHash = field(rows[11], 'manifest-sha256'), checksumsHash = field(rows[12], 'checksums-sha256');
  assert.match(manifestHash, HEX); assert.match(checksumsHash, HEX);
  const assets = assetNames(version).map(([role, name], index) => {
    assert(rows[13 + index].startsWith('asset='), 'Missing asset');
    const parts = rows[13 + index].slice(6).split('\t');
    assert.equal(parts.length, 4, 'Asset field count');
    assert.equal(parts[0], role, 'Unexpected/duplicate asset role');
    assert.equal(parts[1], name, 'Unexpected/path-bearing asset name');
    assert.match(parts[2], /^[1-9][0-9]{0,10}$/);
    const size = Number(parts[2]);
    assert(size <= LIMITS.artifact, 'Artifact size limit');
    assert.match(parts[3], HEX);
    return { role, name, size, hash: parts[3] };
  });
  return { version, tag: `v${version}`, commit, keyId, keyEpoch: Number(keyEpoch), manifestHash, checksumsHash, assets, envelopeHash: HASH(bytes) };
}
export function parseTrustPolicy(bytes) {
  const rows = lines(bytes, LIMITS.trust, 'Trust policy');
  assert(rows.length >= 10 && rows.length <= 9 + LIMITS.keys, 'Trust policy field count');
  assert.equal(rows[0], 'ShadowCode-Release-Trust-v1', 'Trust policy schema');
  fixedIdentity(rows);
  assert.equal(field(rows[4], 'target'), IDENTITY.target);
  assert.equal(field(rows[5], 'channel'), IDENTITY.channel);
  const minimumEpoch = field(rows[6], 'minimum-epoch'), minimumVersion = field(rows[7], 'minimum-version');
  assert.match(minimumEpoch, EPOCH); assert.match(minimumVersion, VERSION);
  assert.equal(rows[8], 'keys=ed25519-spki-sha256', 'Trust key encoding');
  const seen = new Set();
  const keys = rows.slice(9).map(row => {
    assert(row.startsWith('key='), 'Unknown trust field');
    const [epoch, id, minimum, maximum, extra] = row.slice(4).split('\t');
    assert.equal(extra, undefined); assert.match(epoch ?? '', EPOCH); assert.match(id ?? '', HEX);
    assert.match(minimum ?? '', VERSION); assert.match(maximum ?? '', VERSION);
    assert(compareVersions(minimum, maximum) <= 0, 'Invalid key version interval');
    assert(!seen.has(id), 'Duplicate trust key'); seen.add(id);
    return { epoch: Number(epoch), id, minimum, maximum };
  });
  return { minimumEpoch: Number(minimumEpoch), minimumVersion, keys };
}
export function publicKeyIdentity(pem) {
  const key = createPublicKey(pem);
  assert.equal(key.asymmetricKeyType, 'ed25519', 'Only Ed25519 keys are allowed');
  return HASH(key.export({ format: 'der', type: 'spki' }));
}
async function boundedFile(file, limit) {
  // O_NOFOLLOW also closes the lstat/open symlink race. Inputs are never executed.
  const handle = await open(file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const info = await handle.stat();
    assert(info.isFile() && info.size > 0 && info.size <= limit, `Invalid file or size: ${path.basename(file)}`);
    const bytes = Buffer.alloc(limit + 1);
    let used = 0;
    while (used < bytes.length) {
      const { bytesRead } = await handle.read(bytes, used, bytes.length - used, null);
      if (!bytesRead) break;
      used += bytesRead;
    }
    assert(used > 0 && used <= limit && used === info.size, 'File size changed or exceeded limit');
    return bytes.subarray(0, used);
  } finally { await handle.close(); }
}
async function assetHash(file) {
  const handle = await open(file, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    const before = await handle.stat();
    assert(before.isFile() && before.size > 0 && before.size <= LIMITS.artifact, 'Invalid artifact or size');
    const hash = createHash('sha256');
    let size = 0;
    for await (const chunk of handle.createReadStream({ autoClose: false })) {
      size += chunk.length; assert(size <= LIMITS.artifact, 'Artifact size limit'); hash.update(chunk);
    }
    const after = await handle.stat();
    assert.equal(size, before.size, 'Artifact size changed');
    assert.equal(after.mtimeNs ?? after.mtimeMs, before.mtimeNs ?? before.mtimeMs, 'Artifact changed');
    return { hash: hash.digest('hex'), size };
  } finally { await handle.close(); }
}
function canonicalChecksums(assets) { return Buffer.from(assets.map(asset => `${asset.hash}  ${asset.name}\n`).join('')); }
export async function createEnvelope({ bundleDir, version, commit, keyId, keyEpoch }) {
  const assets = [];
  for (const [role, name] of assetNames(version)) assets.push({ role, name, ...await assetHash(path.join(bundleDir, name)) });
  const sums = await boundedFile(path.join(bundleDir, 'SHA256SUMS'), LIMITS.checksums);
  assert(sums.equals(canonicalChecksums(assets)), 'Checksums must contain the exact canonical asset set');
  const manifestBytes = await boundedFile(path.join(bundleDir, 'RELEASE-MANIFEST.json'), LIMITS.manifest);
  const manifest = JSON.parse(manifestBytes.toString('utf8'));
  assert(manifestBytes.equals(Buffer.from(`${JSON.stringify(manifest, null, 2)}\n`)), 'Manifest must be canonical JSON without duplicate fields');
  assert.equal(manifest.schema, 1); assert.equal(manifest.tag, `v${version}`);
  assert.equal(manifest.commit, commit); assert.equal(manifest.target, IDENTITY.target);
  assert.deepEqual(manifest.assets, Object.fromEntries(assets.map(a => [a.name, a.hash])), 'Manifest asset bindings');
  const rows = ['ShadowCode-Release-Auth-v1', `repository=${IDENTITY.repository}`, `repository-id=${IDENTITY.repositoryId}`, `owner-id=${IDENTITY.ownerId}`,
    `version=${version}`, `tag=v${version}`, `commit=${commit}`, `target=${IDENTITY.target}`, `channel=${IDENTITY.channel}`, `key-id=${keyId}`, `key-epoch=${keyEpoch}`,
    `manifest-sha256=${HASH(manifestBytes)}`, `checksums-sha256=${HASH(sums)}`,
    ...assets.map(a => `asset=${a.role}\t${a.name}\t${a.size}\t${a.hash}`)];
  const bytes = Buffer.from(`${rows.join('\n')}\n`); parseEnvelope(bytes); return bytes;
}
export function signEnvelope(bytes, privateKey) {
  const metadata = parseEnvelope(bytes);
  assert.equal(publicKeyIdentity(privateKey), metadata.keyId, 'Signing key fingerprint mismatch');
  return sign(null, bytes, privateKey);
}
async function verifyEnvelopeInternal({ bytes, signature, trustDir, historical = false }) {
  const metadata = parseEnvelope(bytes);
  assert(Buffer.isBuffer(signature) && signature.length === LIMITS.signature, 'Signature must be exactly 64 bytes');
  const trust = parseTrustPolicy(await boundedFile(path.join(trustDir, 'policy'), LIMITS.trust));
  const entry = trust.keys.find(key => key.id === metadata.keyId);
  assert(entry && entry.epoch === metadata.keyEpoch, 'Unknown signing key or epoch');
  assert(compareVersions(metadata.version, entry.minimum) >= 0 && compareVersions(metadata.version, entry.maximum) <= 0, 'Outside key version interval');
  if (!historical) {
    assert(metadata.keyEpoch >= trust.minimumEpoch, 'Retired signing epoch');
    assert(compareVersions(metadata.version, trust.minimumVersion) >= 0, 'Below trusted version floor');
  }
  const pem = await boundedFile(path.join(trustDir, `${metadata.keyId}.pem`), LIMITS.publicKey);
  const publicKey = createPublicKey(pem);
  assert.equal(publicKey.asymmetricKeyType, 'ed25519', 'Only Ed25519 keys are allowed');
  assert(pem.equals(Buffer.from(publicKey.export({ type: 'spki', format: 'pem' }))), 'Trusted key must be canonical public SPKI PEM');
  assert.equal(publicKeyIdentity(pem), metadata.keyId, 'Public key fingerprint mismatch');
  assert(verify(null, bytes, publicKey, signature), 'Invalid release signature');
  return metadata;
}
export async function verifyEnvelope({ bytes, signature, trustDir }) {
  // Historical floor exceptions are deliberately not exposed as an install API.
  return verifyEnvelopeInternal({ bytes, signature, trustDir });
}
export async function verifyBundle({ bundleDir, trustDir, artifact, previousDir, expectedVersion, expectedCommit }) {
  assert(trustDir, 'An independently trusted key directory is required');
  const bytes = await boundedFile(path.join(bundleDir, 'RELEASE-AUTH'), LIMITS.envelope);
  const signature = await boundedFile(path.join(bundleDir, 'RELEASE-AUTH.sig'), LIMITS.signature);
  const metadata = await verifyEnvelope({ bytes, signature, trustDir });
  if (expectedVersion !== undefined) assert.equal(metadata.version, expectedVersion, 'Unexpected version');
  if (expectedCommit !== undefined) assert.equal(metadata.commit, expectedCommit, 'Unexpected commit');
  if (previousDir) {
    const previous = await verifyEnvelopeInternal({ bytes: await boundedFile(path.join(previousDir, 'RELEASE-AUTH'), LIMITS.envelope), signature: await boundedFile(path.join(previousDir, 'RELEASE-AUTH.sig'), LIMITS.signature), trustDir, historical: true });
    assert(metadata.keyEpoch >= previous.keyEpoch, 'Signing epoch rollback');
    const comparison = compareVersions(metadata.version, previous.version);
    assert(comparison >= 0, 'Release downgrade');
    if (comparison === 0) assert.equal(metadata.envelopeHash, previous.envelopeHash, 'Changed release under an accepted version');
  }
  const manifest = await boundedFile(path.join(bundleDir, 'RELEASE-MANIFEST.json'), LIMITS.manifest);
  const sums = await boundedFile(path.join(bundleDir, 'SHA256SUMS'), LIMITS.checksums);
  assert.equal(HASH(manifest), metadata.manifestHash, 'Manifest digest mismatch');
  assert.equal(HASH(sums), metadata.checksumsHash, 'Checksums digest mismatch');
  assert(sums.equals(canonicalChecksums(metadata.assets)), 'Checksums asset set mismatch');
  const selected = metadata.assets.find(assetInfo => assetInfo.name === artifact);
  assert(selected, 'Artifact must be one exact authenticated basename');
  const observed = await assetHash(path.join(bundleDir, selected.name));
  assert.equal(observed.size, selected.size, 'Artifact size mismatch');
  assert.equal(observed.hash, selected.hash, 'Artifact digest mismatch');
  return metadata;
}
