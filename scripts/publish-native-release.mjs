// Draft staging with byte verification; never replace a versioned asset.
import { createReadStream } from 'node:fs';
import { readFile, writeFile, mkdtemp, rm } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';

export async function digest(file) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}
const command = (args) => execFileSync('gh', args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });

export async function publish({ repo, tag, commit, files, gh = command }) {
  assert.match(repo, /^[\w.-]+\/[\w.-]+$/);
  assert.match(tag, /^v\d+\.\d+\.\d+(?:[-+][\w.-]+)?$/);
  assert.match(commit, /^[a-f0-9]{40}$/);
  const expected = new Map();
  for (const file of files) {
    const name = path.basename(file);
    assert.match(name, /^[\w.-]+$/);
    assert(!expected.has(name), `Duplicate asset ${name}`);
    expected.set(name, { file, hash: await digest(file) });
  }
  assert(expected.size >= 4, 'Required release assets missing');
  // Listing includes authenticated drafts; the tag endpoint describes
  // published releases and must not be used to infer draft absence.
  const inspect = () => {
    const releases = JSON.parse(gh(['api', `repos/${repo}/releases?per_page=100`, '--paginate', '--slurp'])).flat();
    const matches = releases.filter(release => release.tag_name === tag);
    assert(matches.length <= 1, 'Multiple releases use this tag; review before publication');
    return matches[0];
  };
  let release = inspect();
  if (!release) {
    gh(['release', 'create', tag, '--repo', repo, '--draft', '--verify-tag', '--target', commit,
      '--title', `ShadowCode ${tag}`, '--notes-file', 'docs/RELEASE_NOTES.md']);
    release = inspect();
  }
  assert(release, 'Draft release is missing after creation');
  assert.equal(release.tag_name, tag);
  const verify = async (record, allowMissing) => {
    const names = new Set();
    const scratch = await mkdtemp(path.join(tmpdir(), 'shadowcode-release-'));
    try {
      for (const asset of record.assets) {
        assert(expected.has(asset.name), `Unexpected remote asset ${asset.name}; review the draft`);
        assert(!names.has(asset.name), `Duplicate remote asset ${asset.name}`);
        names.add(asset.name);
        const target = path.join(scratch, asset.name);
        gh(['release', 'download', tag, '--repo', repo, '--pattern', asset.name, '--output', target]);
        assert.equal(await digest(target), expected.get(asset.name).hash, `Content mismatch for ${asset.name}; never overwrite this version`);
      }
      if (!allowMissing) assert.equal(names.size, expected.size, 'Incomplete remote asset set');
      return names;
    } finally { await rm(scratch, { recursive: true, force: true }); }
  };
  const present = await verify(release, release.draft);
  if (!release.draft) return { status: 'already-published' }; // Read-only retry.
  for (const [name, asset] of expected) {
    if (!present.has(name)) gh(['release', 'upload', tag, asset.file, '--repo', repo]);
  }
  release = inspect();
  assert(release.draft, 'Release changed during staging');
  await verify(release, false);
  gh(['release', 'edit', tag, '--repo', repo, '--draft=false', '--verify-tag']);
  return { status: 'published' };
}

async function main() {
  const tag = process.env.GITHUB_REF_NAME;
  const commit = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  assert.equal(execFileSync('git', ['rev-parse', `${tag}^{commit}`], { encoding: 'utf8' }).trim(), commit);
  assert.equal(commit, process.env.GITHUB_SHA);
  const version = JSON.parse(await readFile('src-tauri/tauri.conf.json', 'utf8')).version;
  assert.equal(tag, `v${version}`);
  const bundle = 'target/release/bundle';
  const files = [
    `${bundle}/appimage/ShadowCode_${version}_amd64.AppImage`,
    `${bundle}/appimage/ShadowCode_${version}_appimage-runtime-sources.tar.gz`,
    `${bundle}/deb/ShadowCode_${version}_amd64.deb`,
  ];
  const sums = await readFile(`${bundle}/SHA256SUMS`, 'utf8');
  const listed = new Map(sums.trim().split('\n').map(line => {
    const match = /^([a-f0-9]{64})  ([\w.-]+)$/.exec(line);
    assert(match, 'Malformed package checksums');
    return [match[2], match[1]];
  }));
  assert.equal(listed.size, files.length);
  for (const file of files) assert.equal(await digest(file), listed.get(path.basename(file)), `Package checksum mismatch: ${file}`);
  const manifest = {
    schema: 1, tag, commit, target: 'x86_64-unknown-linux-gnu',
    cargo_lock_sha256: await digest('Cargo.lock'),
    ui_lock_sha256: await digest('ui/package-lock.json'),
    runtime_pin: (await readFile('tools/llama.cpp.pin', 'utf8')).trim(),
    verification: 'Required release workflow checks completed before this publication step',
    assets: Object.fromEntries(listed),
  };
  const manifestPath = `${bundle}/RELEASE-MANIFEST.json`;
  await writeFile(manifestPath, `${JSON.stringify(manifest, null, 2)}\n`);
  console.log(await publish({ repo: process.env.GITHUB_REPOSITORY, tag, commit,
    files: [...files, `${bundle}/SHA256SUMS`, manifestPath] }));
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await main();
