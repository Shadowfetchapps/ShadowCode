// Source verification for the one explicitly vendored Cargo dependency.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { lstat, readFile, readdir } from 'node:fs/promises';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

export const GLIB_DIRECTORY = 'vendor/glib-0.18.5';
export const GLIB_PROVENANCE = 'vendor/glib-0.18.5.provenance.json';
export const GLIB_README = 'vendor/glib-0.18.5.README.md';
export const GLIB_ARCHIVE_SHA256 = '233daaf6e83ae6a12a52055f568f9d7cf4671dabb78ff9560ab6da230ce00ee5';
export const GLIB_FIX_COMMIT = 'b5a4071e439bef2b5eea76c3aa25e5ae84839e34';
export const GLIB_ADVISORY = 'RUSTSEC-2024-0429';
const root = fileURLToPath(new URL('../', import.meta.url));
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const original = String.raw`            let p: *mut libc::c_char = std::ptr::null_mut();
            let s = b"&s\0";
            ffi::g_variant_get_child(
                self.variant.to_glib_none().0,
                i,
                s as *const u8 as *const _,
                &p,`;
const corrected = original.replace('let p:', 'let mut p:').replace('                &p,', '                &mut p,');

async function filesAt(directory, relative = '') {
  const files = [];
  for (const entry of await readdir(path.join(directory, relative), { withFileTypes: true })) {
    const name = relative ? `${relative}/${entry.name}` : entry.name;
    assert(!entry.isSymbolicLink(), `Symlink in vendored GLib: ${name}`);
    if (entry.isDirectory()) files.push(...await filesAt(directory, name));
    else {
      assert(entry.isFile(), `Non-file in vendored GLib: ${name}`);
      files.push(name);
    }
  }
  return files.sort();
}

export async function verifyGlibBackport(checkout = root) {
  const directory = path.join(checkout, GLIB_DIRECTORY);
  const directoryInfo = await lstat(directory);
  assert(directoryInfo.isDirectory() && !directoryInfo.isSymbolicLink(), 'Vendored GLib must be a real directory');
  const provenance = JSON.parse(await readFile(path.join(checkout, GLIB_PROVENANCE), 'utf8'));
  assert.equal(provenance.schema, 1);
  assert.equal(provenance.name, 'glib');
  assert.equal(provenance.version, '0.18.5');
  assert.equal(provenance.source, 'https://static.crates.io/crates/glib/glib-0.18.5.crate');
  assert.equal(provenance.sourceSha256, GLIB_ARCHIVE_SHA256);
  assert.equal(provenance.sourceCommit, '42b9caf98e03ded086362d9653ca58fe94dc8658');
  assert.equal(provenance.patchCommit, GLIB_FIX_COMMIT);
  assert.equal(provenance.advisory, GLIB_ADVISORY);
  assert.equal(provenance.patchedFile, 'src/variant_iter.rs');
  assert.equal(provenance.patchedSha256, 'a0f5ee8acb8faa089bcdfbc9a57372609fce7654026ccef7d9a224d05a654ccc');
  assert.equal(provenance.originalFiles['src/variant_iter.rs'], '1fd02859333761c45321b32f28b24233446b97d0022a90d3a937ed162585b90e');
  const expected = Object.keys(provenance.originalFiles).sort();
  assert.equal(expected.length, 121, 'Original GLib archive file count changed');
  // Bind the entire inventory to the independently verified published archive,
  // so editing a source and its adjacent manifest hash cannot authorize it.
  assert.equal(hash(expected.map(file => `${file}\0${provenance.originalFiles[file]}\n`).join('')),
    '5da7556cd83c4d07950e32b0bbc212fdf3bd1697f9b2751c3892a33cf25e6c6b',
    'Original GLib archive inventory does not match the published source');
  assert.deepEqual(await filesAt(directory), expected, 'Vendored GLib file inventory changed');
  const tree = [];
  for (const file of expected) {
    const bytes = await readFile(path.join(directory, file));
    const sha256 = hash(bytes);
    const expectedHash = provenance.originalFiles[file];
    assert.match(expectedHash, /^[a-f0-9]{64}$/);
    if (file === provenance.patchedFile) {
      assert.equal(sha256, provenance.patchedSha256, 'GLib iterator does not contain the exact verified correction');
      const source = bytes.toString('utf8');
      assert.equal(source.split(corrected).length, 2, 'Expected exactly one mutable out-pointer correction');
      assert.equal(hash(source.replace(corrected, original)), expectedHash, 'GLib differs from upstream beyond the two-line correction');
    } else assert.equal(sha256, expectedHash, `Modified vendored GLib source: ${file}`);
    tree.push(`${file}\0${sha256}\n`);
  }
  return { ...provenance, sourceTreeSha256: hash(tree.join('')) };
}

// A path dependency must not silently disappear from license inventories or
// be replaced by another registry/path copy that merely has the same version.
export function requireGlibBackport(metadata, checkout = root) {
  const resolved = new Set(metadata.resolve.nodes.map(node => node.id));
  const packages = metadata.packages.filter(pkg => pkg.name === 'glib' && resolved.has(pkg.id));
  assert.equal(packages.length, 1, 'Expected exactly one resolved GLib dependency');
  const pkg = packages[0];
  assert.equal(pkg.version, '0.18.5');
  assert.equal(pkg.source, null, 'GLib must resolve to the verified local backport');
  assert.equal(path.resolve(pkg.manifest_path), path.resolve(checkout, GLIB_DIRECTORY, 'Cargo.toml'), 'Unexpected GLib source path');
  assert(!metadata.workspace_members.includes(pkg.id), 'Vendored GLib must remain outside the application workspace');
  return pkg;
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  assert.equal(process.argv.length, 2, 'Usage: node scripts/native-glib-backport.mjs');
  const result = await verifyGlibBackport();
  console.log(`Verified glib ${result.version}: 121 source files, exact upstream two-line ${result.advisory} backport; tree SHA256 ${result.sourceTreeSha256}`);
}
