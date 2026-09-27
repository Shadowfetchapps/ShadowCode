// Build-job handoff. No signing key and no publication capability.
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import { assetNames, IDENTITY, LIMITS } from './native-release-auth.mjs';
import { digest, packageHashes, readVerification, validateVerification } from './native-release-verification.mjs';
import { snapshotFile, validateContext, validateStagedRelease, retainBundle, releaseNames, jsonBytes } from './native-release-assets.mjs';

export async function prepareRelease({ files, checksums, source, verification, context, outputDir }) {
  const version = validateContext(context), names = assetNames(version).map(([, name]) => name);
  assert.deepEqual(files.map(file => path.basename(file)).sort(), [...names].sort(), 'Build package set mismatch');
  const directory = await mkdtemp(path.join(tmpdir(), 'shadowcode-build-handoff-'));
  try {
    const observed = new Map();
    for (const file of files) {
      const name = path.basename(file);
      observed.set(name, { file: path.join(directory, name), ...await snapshotFile(file, path.join(directory, name), LIMITS.artifact) });
    }
    await snapshotFile(checksums, path.join(directory, 'SHA256SUMS'), LIMITS.checksums);
    const assets = Object.fromEntries(names.map(name => [name, observed.get(name).hash]));
    const manifest = { schema: 1, tag: context.tag, commit: context.commit, target: IDENTITY.target, cargo_lock_sha256: source.cargo_lock_sha256, ui_lock_sha256: source.ui_lock_sha256, runtime_pin: source.runtime_pin, verification: validateVerification(verification, context.commit, assets), assets };
    await writeFile(path.join(directory, 'RELEASE-MANIFEST.json'), jsonBytes(manifest), { mode: 0o400 });
    await validateStagedRelease({ directory, observed, verification, context, signed: false });
    await retainBundle(directory, outputDir, releaseNames(version, false), verification);
  } finally { await rm(directory, { recursive: true, force: true }); }
}
async function main() {
  const [outputDir] = process.argv.slice(2); assert(outputDir && process.argv.length === 3, 'Output directory required');
  const context = { repo: process.env.GITHUB_REPOSITORY, tag: process.env.GITHUB_REF_NAME, commit: process.env.GITHUB_SHA, runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT };
  const version = validateContext(context);
  const git = args => execFileSync('git', args, { encoding: 'utf8' }).trim();
  assert.equal(git(['rev-parse', 'HEAD']), context.commit);
  assert.equal(git(['rev-parse', `${context.tag}^{commit}`]), context.commit);
  assert.equal(JSON.parse(await readFile('src-tauri/tauri.conf.json', 'utf8')).version, version);
  const base = 'target/release/bundle';
  const files = assetNames(version).map(([role, name]) => `${base}/${role === 'deb' ? 'deb' : 'appimage'}/${name}`);
  const verification = await readVerification('artifacts/release-verification', context.commit, context.runId, context.runAttempt, await packageHashes());
  const source = { cargo_lock_sha256: await digest('Cargo.lock'), ui_lock_sha256: await digest('ui/package-lock.json'), runtime_pin: (await readFile('tools/llama.cpp.pin', 'utf8')).trim() };
  await prepareRelease({ files, checksums: `${base}/SHA256SUMS`, source, verification, context, outputDir });
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await main();
