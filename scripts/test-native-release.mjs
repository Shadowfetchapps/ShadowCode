import { test } from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, writeFile, rm } from 'node:fs/promises';
import { readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { publish } from './publish-native-release.mjs';

async function fixture(run) {
  const root = await mkdtemp(path.join(tmpdir(), 'release-test-'));
  const files = ['app.AppImage', 'app.deb', 'sources.tar.gz', 'SHA256SUMS', 'RELEASE-MANIFEST.json'].map(name => path.join(root, name));
  for (const file of files) await writeFile(file, `fixture ${path.basename(file)}`);
  const state = { release: null, assets: new Map(), calls: [], failUpload: false, apiFailure: false };
  const gh = args => {
    state.calls.push(args);
    assert(!args.includes('--clobber'));
    if (args[0] === 'api') {
      if (state.apiFailure) throw new Error('HTTP 403');
      if (!state.release) return '[[]]';
      return JSON.stringify([[{ ...state.release, assets: [...state.assets.keys()].map(name => ({ name })) }]]);
    }
    switch (args[1]) {
      case 'create': assert(args.includes('--draft')); state.release = { draft: true, tag_name: 'v1.0.0' }; break;
      case 'upload':
        assert(state.release.draft);
        if (state.failUpload && state.assets.size === 1) throw new Error('upload interrupted');
        assert(!state.assets.has(path.basename(args[3])));
        state.assets.set(path.basename(args[3]), state.corruptUpload ? Buffer.from('corrupt upload') : readFileSync(args[3])); break;
      case 'download': writeFileSync(args[args.indexOf('--output') + 1], state.assets.get(args[args.indexOf('--pattern') + 1])); break;
      case 'edit': assert.equal(state.assets.size, files.length); state.release.draft = false; break;
      default: throw new Error(`Unexpected command: ${args}`);
    }
    return '';
  };
  const execute = () => publish({ repo: 'owner/repo', tag: 'v1.0.0', commit: 'a'.repeat(40), files, gh });
  try { await run({ state, execute, files }); } finally { await rm(root, { recursive: true, force: true }); }
}

test('uploads to a draft, verifies all bytes, then publishes', () => fixture(async ({ state, execute }) => {
  assert.equal((await execute()).status, 'published');
  assert.equal(state.release.draft, false);
  const publication = state.calls.findIndex(args => args[1] === 'edit');
  assert.equal(state.calls.slice(0, publication).filter(args => args[1] === 'download').length, 5);
}));
test('interrupted upload stays draft and retry resumes without overwrites', () => fixture(async ({ state, execute }) => {
  state.failUpload = true;
  await assert.rejects(execute, /upload interrupted/);
  assert(state.release.draft);
  assert.equal(state.assets.size, 1);
  state.failUpload = false;
  await execute();
  assert.equal(state.calls.filter(args => args[1] === 'upload' && args[3].endsWith('app.AppImage')).length, 1);
}));
test('published retry verifies bytes without mutations', () => fixture(async ({ state, execute }) => {
  await execute(); state.calls = [];
  assert.equal((await execute()).status, 'already-published');
  assert(state.calls.every(args => args[0] === 'api' || args[1] === 'download'));
}));
test('same version with changed local content is refused', () => fixture(async ({ state, execute, files }) => {
  await execute(); state.calls = [];
  await writeFile(files[0], 'different binary');
  await assert.rejects(execute, /Content mismatch/);
  assert(state.calls.every(args => args[0] === 'api' || args[1] === 'download'));
}));
test('mismatched draft content is never overwritten or published', () => fixture(async ({ state, execute }) => {
  state.release = { draft: true, tag_name: 'v1.0.0' };
  state.assets.set('app.AppImage', Buffer.from('wrong bytes'));
  await assert.rejects(execute, /Content mismatch/);
  assert(state.release.draft);
  assert(!state.calls.some(args => ['upload', 'edit'].includes(args[1])));
}));
test('authorization failure cannot create another release', () => fixture(async ({ state, execute }) => {
  state.apiFailure = true;
  await assert.rejects(execute, /HTTP 403/);
  assert.equal(state.calls.length, 1);
}));
test('incomplete published release is refused without repair writes', () => fixture(async ({ state, execute, files }) => {
  state.release = { draft: false, tag_name: 'v1.0.0' };
  state.assets.set('app.AppImage', readFileSync(files[0]));
  await assert.rejects(execute, /Incomplete/);
  assert(!state.calls.some(args => ['upload', 'edit'].includes(args[1])));
}));

test('uploaded bytes must verify before draft becomes public', () => fixture(async ({ state, execute }) => {
  state.corruptUpload = true;
  await assert.rejects(execute, /Content mismatch/);
  assert(state.release.draft);
  assert(!state.calls.some(args => args[1] === 'edit'));
}));
