import test from 'node:test';
import assert from 'node:assert/strict';
import { fixture } from './publisher-fixtures.mjs';
const { publish } = await import(process.env.PUBLISHER_BASELINE ? './legacy-publish-native-release.mjs' : './publish-native-release.mjs');
test('unsigned metadata cannot create or publish a release even with all passed gate receipts', () => fixture(async ({ execute, files, state, gh }) => {
  // A vulnerable baseline accepts this five-file unsigned release. Relax only
  // the mock remote's final-count assertion so the baseline exercises publication.
  const legacyGh = args => {
    if (args[0] === 'release' && args[1] === 'edit') { state.calls.push(args); state.release.draft = false; return ''; }
    return gh(args);
  };
  await assert.rejects(execute({ files: files.slice(0, 5), gh: legacyGh }), /authentication|asset set|signed|release assets/i);
  assert.equal(state.calls.length, 0);
}, publish));
test('signed metadata is classified separately from the three qualified package artifacts', () => fixture(async ({ execute, state }) => {
  assert.equal((await execute()).status, 'published');
  assert.equal(state.assets.size, 7);
}, publish));
