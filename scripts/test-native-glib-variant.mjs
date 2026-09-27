// Run optimized Rust regressions without adding GTK/dev dependencies to core.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { requireGlibBackport, verifyGlibBackport } from './native-glib-backport.mjs';

const root = fileURLToPath(new URL('../', import.meta.url));
assert.equal(process.platform, 'linux', 'The optimized GLib regression requires the native Linux build environment');
await verifyGlibBackport(root);
const options = { cwd: root, encoding: 'utf8', maxBuffer: 64000000, timeout: 600000, stdio: ['ignore', 'pipe', 'inherit'] };
const compiler = execFileSync('rustc', ['--version', '--verbose'], options);
const target = /^host: (.+)$/m.exec(compiler)?.[1];
assert(target, 'Cannot identify the Rust target');
const metadata = JSON.parse(execFileSync('cargo', ['metadata', '--locked', '--filter-platform', target, '--format-version', '1'], options));
const pkg = requireGlibBackport(metadata, root);
const messages = execFileSync('cargo', ['build', '--release', '--locked', '-p', 'glib@0.18.5', '--message-format=json'], options)
  .trim().split('\n').filter(Boolean).map(line => JSON.parse(line));
const artifacts = messages.filter(message => message.reason === 'compiler-artifact' && message.package_id === pkg.id && message.target.name === 'glib' && message.target.kind.includes('lib'));
assert.equal(artifacts.length, 1, 'Expected one exact patched GLib compiler artifact');
assert.equal(artifacts[0].profile.opt_level, '3', 'GLib must be compiled with release optimization');
const libraries = artifacts[0].filenames.filter(file => file.endsWith('.rlib'));
assert.equal(libraries.length, 1, 'Expected one exact GLib Rust library');
// Cargo emits a selected package's public rlib one level above its hashed
// dependency artifacts; use that deps directory without guessing a glob.
const libraryDirectory = path.dirname(libraries[0]);
const dependencies = path.basename(libraryDirectory) === 'deps' ? libraryDirectory : path.join(libraryDirectory, 'deps');
const nativeSearch = [...new Set(messages.filter(message => message.reason === 'build-script-executed').flatMap(message => message.linked_paths))].flatMap(directory => ['-L', directory]);
const directory = await mkdtemp(path.join(os.tmpdir(), 'shadowcode-glib-regression-'));
try {
  const binary = path.join(directory, 'variant-tests');
  console.log(`${compiler.trim()}\nOptimized iterator fixture: ${libraries[0]}`);
  execFileSync('rustc', ['--edition=2021', '--test', '-O', '--extern', `glib=${libraries[0]}`, '-L', `dependency=${dependencies}`, ...nativeSearch, path.join(root, 'scripts/fixtures/native-glib-variant.rs'), '-o', binary], { ...options, stdio: 'inherit' });
  execFileSync(binary, ['--test-threads=1'], { ...options, timeout: 30000, stdio: 'inherit' });
} finally {
  await rm(directory, { recursive: true, force: true });
}
