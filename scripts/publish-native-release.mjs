// Draft staging with byte verification; never replace a versioned asset.
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { execFileSync } from 'node:child_process';
import assert from 'node:assert/strict';
import { digest } from './native-release-verification.mjs';
import { IDENTITY } from './native-release-auth.mjs';
import { withReleaseSnapshot, validateContext, validateStagedRelease, readReceipts, requireTransportLayout } from './native-release-assets.mjs';

export { digest } from './native-release-verification.mjs';
const command = (args) => execFileSync('gh', args, { encoding: 'utf8', maxBuffer: 16 * 1024 * 1024, stdio: ['ignore', 'pipe', 'pipe'] });

export async function publish({ repo, tag, commit, files, verification, trustDir, runId, runAttempt, gh = command }) {
  const context = { repo, tag, commit, runId, runAttempt };
  const version = validateContext(context);
  return withReleaseSnapshot({ files, version }, async (directory, expected) => {
    await validateStagedRelease({ directory, observed: expected, verification, context, trustDir, signed: true });
    return publishVerified({ repo, tag, commit, expected, gh });
  });
}
async function publishVerified({ repo, tag, commit, expected, gh }) {
  const identity = JSON.parse(gh(['api', `repos/${repo}`]));
  assert.equal(String(identity.id), IDENTITY.repositoryId, 'Remote repository identity changed');
  assert.equal(String(identity.owner?.id), IDENTITY.ownerId, 'Remote repository owner changed');
  const verifyTag = () => {
    let object = JSON.parse(gh(['api', `repos/${repo}/git/ref/tags/${tag}`])).object;
    let depth = 0;
    while (object?.type === 'tag') {
      assert(++depth <= 4 && /^[a-f0-9]{40}$/.test(object.sha), 'Invalid annotated tag chain');
      object = JSON.parse(gh(['api', `repos/${repo}/git/tags/${object.sha}`])).object;
    }
    assert.equal(object?.type, 'commit', 'Release tag must resolve to a commit');
    assert.equal(object.sha, commit, 'Remote release tag changed');
  };
  verifyTag();
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
    verifyTag();
    gh(['release', 'create', tag, '--repo', repo, '--draft', '--verify-tag', '--target', commit,
      '--title', `ShadowCode ${tag}`, '--notes-file', 'docs/RELEASE_NOTES.md']);
    release = inspect();
  }
  assert(release, 'Draft release is missing after creation');
  assert.equal(release.tag_name, tag);
  assert.equal(typeof release.draft, 'boolean');
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
  verifyTag();
  gh(['release', 'edit', tag, '--repo', repo, '--draft=false', '--verify-tag']);
  return { status: 'published' };
}

async function main() {
  const [bundle, trustDir] = process.argv.slice(2);
  assert(bundle && trustDir && process.argv.length === 4, 'Usage: node publish-native-release.mjs SIGNED_BUNDLE TRUST_DIR');
  const context = { repo: process.env.GITHUB_REPOSITORY, tag: process.env.GITHUB_REF_NAME, commit: process.env.GITHUB_SHA, runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT };
  const version = validateContext(context);
  const files = await requireTransportLayout(bundle, version, true);
  const verification = await readReceipts(bundle);
  console.log(await publish({ ...context, files, verification, trustDir }));
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await main();
