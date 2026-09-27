import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { cp, mkdtemp, readFile, rm, symlink, writeFile } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { test } from 'node:test';
import { GLIB_DIRECTORY, GLIB_PROVENANCE, GLIB_README, GLIB_ARCHIVE_SHA256, requireGlibBackport, verifyGlibBackport } from './native-glib-backport.mjs';
import { glibBackportNotice } from './native-notices.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
async function fixture(t) {
  const checkout = await mkdtemp(path.join(os.tmpdir(), 'shadowcode-glib-integrity-'));
  t.after(() => rm(checkout, { recursive: true, force: true }));
  for (const relative of [GLIB_DIRECTORY, GLIB_PROVENANCE, GLIB_README]) {
    await cp(path.join(root, relative), path.join(checkout, relative), { recursive: true });
  }
  return checkout;
}
function metadata(checkout) {
  const id = 'path+file:///fixture/vendor/glib-0.18.5#glib@0.18.5';
  return { packages: [{ id, name: 'glib', version: '0.18.5', source: null, manifest_path: path.join(checkout, GLIB_DIRECTORY, 'Cargo.toml') }], resolve: { nodes: [{ id }] }, workspace_members: [] };
}

test('entire source matches published hashes plus exactly the upstream correction', async () => {
  const verified = await verifyGlibBackport(root);
  assert.equal(verified.sourceSha256, GLIB_ARCHIVE_SHA256);
  assert.equal(Object.keys(verified.originalFiles).length, 121);
  assert.equal(verified.sourceTreeSha256, '2f7ef2019fd0cb8cb2b95f3a3a4d08b63c4ea546362eae332643a6a6de0e67bf');
});

test('unpatched or additionally modified iterator is refused', async t => {
  const checkout = await fixture(t);
  const file = path.join(checkout, GLIB_DIRECTORY, 'src/variant_iter.rs');
  const corrected = await readFile(file, 'utf8');
  await writeFile(file, corrected.replace('let mut p: *mut libc::c_char', 'let p: *mut libc::c_char').replace('&mut p,', '&p,'));
  await assert.rejects(verifyGlibBackport(checkout), /exact verified correction/);
  await writeFile(file, `${corrected}\n// unrelated change\n`);
  await assert.rejects(verifyGlibBackport(checkout), /exact verified correction/);
});

test('modified original file and omitted MIT license are refused', async t => {
  const checkout = await fixture(t);
  const file = path.join(checkout, GLIB_DIRECTORY, 'LICENSE');
  await writeFile(file, 'replaced license\n');
  await assert.rejects(verifyGlibBackport(checkout), /Modified vendored GLib source: LICENSE/);
  await rm(file);
  await assert.rejects(verifyGlibBackport(checkout), /file inventory changed/);
});

test('additional files and symlinks are refused', async t => {
  const checkout = await fixture(t);
  const file = path.join(checkout, GLIB_DIRECTORY, 'unexpected.rs');
  await writeFile(file, '');
  await assert.rejects(verifyGlibBackport(checkout), /file inventory changed/);
  await rm(file);
  await symlink('LICENSE', file);
  await assert.rejects(verifyGlibBackport(checkout), /Symlink in vendored GLib/);
});

test('altered claimed archive or correction identity is refused', async t => {
  const checkout = await fixture(t);
  const file = path.join(checkout, GLIB_PROVENANCE);
  const original = JSON.parse(await readFile(file, 'utf8'));
  for (const field of ['sourceSha256', 'patchCommit', 'patchedSha256']) {
    await writeFile(file, JSON.stringify({ ...original, [field]: '0'.repeat(original[field].length) }));
    await assert.rejects(verifyGlibBackport(checkout));
  }
});

test('changing another source and its matching manifest hash cannot rewrite published provenance', async t => {
  const checkout = await fixture(t);
  const changed = '// modified upstream source\n';
  await writeFile(path.join(checkout, GLIB_DIRECTORY, 'src/variant.rs'), changed);
  const file = path.join(checkout, GLIB_PROVENANCE);
  const provenance = JSON.parse(await readFile(file, 'utf8'));
  provenance.originalFiles['src/variant.rs'] = createHash('sha256').update(changed).digest('hex');
  await writeFile(file, JSON.stringify(provenance));
  await assert.rejects(verifyGlibBackport(checkout), /inventory does not match the published source/);
});

test('graph requires the sole resolved exact path crate outside workspace membership', () => {
  assert.equal(requireGlibBackport(metadata(root), root).version, '0.18.5');
  for (const mutate of [
    m => { m.packages[0].source = 'registry+https://github.com/rust-lang/crates.io-index'; },
    m => { m.packages[0].manifest_path = '/some/other/glib/Cargo.toml'; },
    m => { m.packages[0].version = '0.20.0'; },
    m => { m.workspace_members.push(m.packages[0].id); },
    m => { m.resolve.nodes = []; },
    m => { m.packages.push({ ...m.packages[0], id: 'another' }); m.resolve.nodes.push({ id: 'another' }); },
  ]) {
    const m = metadata(root); mutate(m);
    assert.throws(() => requireGlibBackport(m, root));
  }
});

test('path crate emits the unchanged MIT license, original source hash and explicit patch provenance', async t => {
  const checkout = await fixture(t);
  const destination = path.join(checkout, 'notices');
  const notice = await glibBackportNotice(metadata(checkout), destination, checkout);
  assert.equal(notice.name, 'glib');
  assert.equal(notice.version, '0.18.5');
  assert.equal(notice.license, 'MIT');
  assert.equal(notice.sourceKind, 'vendored-backport');
  assert.equal(notice.sourceSha256, GLIB_ARCHIVE_SHA256);
  assert.equal(notice.modifications[0].advisory, 'RUSTSEC-2024-0429');
  assert.equal(notice.notices.length, 3);
  const originalLicense = await readFile(path.join(checkout, GLIB_DIRECTORY, 'LICENSE'));
  assert.deepEqual(await readFile(path.join(destination, notice.notices[0].file)), originalLicense);
  assert.equal(notice.notices[0].sha256, createHash('sha256').update(originalLicense).digest('hex'));
  assert.deepEqual(await readFile(path.join(destination, notice.notices[1].file)), await readFile(path.join(checkout, GLIB_DIRECTORY, 'COPYRIGHT')));
  assert.deepEqual(JSON.parse(await readFile(path.join(destination, notice.provenance.file))), JSON.parse(await readFile(path.join(checkout, GLIB_PROVENANCE))));
});

test('notice generation refuses missing/replaced or tampered local GLib instead of silently skipping it', async t => {
  const checkout = await fixture(t);
  const destination = path.join(checkout, 'notices');
  const missing = metadata(checkout); missing.packages = [];
  await assert.rejects(glibBackportNotice(missing, destination, checkout), /exactly one resolved/);
  const registry = metadata(checkout); registry.packages[0].source = 'registry+https://github.com/rust-lang/crates.io-index';
  await assert.rejects(glibBackportNotice(registry, destination, checkout), /verified local backport/);
  await writeFile(path.join(checkout, GLIB_DIRECTORY, 'LICENSE'), 'wrong');
  await assert.rejects(glibBackportNotice(metadata(checkout), destination, checkout), /Modified vendored/);
});
