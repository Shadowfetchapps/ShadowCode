import { test } from 'node:test';
import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { readFileSync } from 'node:fs';
import { publish } from './publish-native-release.mjs';

import { fixture as signedFixture } from './publisher-fixtures.mjs';
const fixture = run => signedFixture(run, publish);

test('uploads to a draft, verifies all bytes, then publishes', () => fixture(async ({ state, execute }) => {
  assert.equal((await execute()).status, 'published');
  assert.equal(state.release.draft, false);
  const publication = state.calls.findIndex(args => args[1] === 'edit');
  assert.equal(state.calls.slice(0, publication).filter(args => args[1] === 'download').length, 7);
}));
test('interrupted upload stays draft and retry resumes without overwrites', () => fixture(async ({ state, execute }) => {
  state.failUpload = true;
  await assert.rejects(execute, /upload interrupted/);
  assert(state.release.draft);
  assert.equal(state.assets.size, 1);
  state.failUpload = false;
  await execute();
  assert.equal(state.calls.filter(args => args[1] === 'upload' && args[3].endsWith('_amd64.AppImage')).length, 1);
}));
test('published retry verifies bytes without mutations', () => fixture(async ({ state, execute }) => {
  await execute(); state.calls = [];
  assert.equal((await execute()).status, 'already-published');
  assert(state.calls.every(args => args[0] === 'api' || args[1] === 'download'));
}));
test('same version with changed local content is refused', () => fixture(async ({ state, execute, files, rebuild }) => {
  await execute(); state.calls = [];
  await writeFile(files[0], 'different binary');
  await rebuild();
  await assert.rejects(execute, /Content mismatch/);
  assert(state.calls.every(args => args[0] === 'api' || args[1] === 'download'));
}));
test('mismatched draft content is never overwritten or published', () => fixture(async ({ state, execute }) => {
  state.release = { draft: true, tag_name: 'v1.0.0' };
  state.assets.set('ShadowCode_1.0.0_amd64.AppImage', Buffer.from('wrong bytes'));
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
  state.assets.set('ShadowCode_1.0.0_amd64.AppImage', readFileSync(files[0]));
  await assert.rejects(execute, /Incomplete/);
  assert(!state.calls.some(args => ['upload', 'edit'].includes(args[1])));
}));

test('uploaded bytes must verify before draft becomes public', () => fixture(async ({ state, execute }) => {
  state.corruptUpload = true;
  await assert.rejects(execute, /Content mismatch/);
  assert(state.release.draft);
  assert(!state.calls.some(args => args[1] === 'edit'));
}));

test('missing required verification cannot create or publish a release', () => fixture(async ({ state, execute }) => {
  await assert.rejects(execute({ verification: null }), /verification/i);
  assert.equal(state.calls.length, 0);
}));

for (const status of ['failed', 'skipped', 'running']) {
  test(`required ${status} verification cannot create or publish a release`, () => fixture(async ({ state, execute, verification }) => {
    verification.gates['native-behavior'].status = status;
    await assert.rejects(execute, /Required verification gate did not pass/);
    assert.equal(state.calls.length, 0);
  }));
}
