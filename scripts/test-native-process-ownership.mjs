import { test } from 'node:test';
import assert from 'node:assert/strict';
import { matchesPackagedExecutable } from './native-process-ownership.mjs';

const digest = 'a'.repeat(64);

test('native builds require the exact launched executable path', () => {
  assert.equal(matchesPackagedExecutable({
    runningPath: '/build/shadowcode', launchedPath: '/build/shadowcode',
  }), true);
  assert.equal(matchesPackagedExecutable({
    runningPath: '/other/shadowcode', launchedPath: '/build/shadowcode',
  }), false);
});

test('AppImage extracted executable is accepted only by packaged name and exact bytes', () => {
  assert.equal(matchesPackagedExecutable({
    runningPath: '/tmp/appimage_extracted_123/usr/bin/shadowcode',
    launchedPath: '/downloads/ShadowCode.AppImage',
    runningSha256: digest,
    packagedSha256: digest,
  }), true);
  assert.equal(matchesPackagedExecutable({
    runningPath: '/tmp/appimage_extracted_123/usr/bin/shadowcode (deleted)',
    launchedPath: '/downloads/ShadowCode.AppImage',
    runningSha256: digest,
    packagedSha256: digest,
  }), true);
  assert.equal(matchesPackagedExecutable({
    runningPath: '/tmp/appimage_extracted_123/usr/bin/shadowcode',
    launchedPath: '/downloads/ShadowCode.AppImage',
    runningSha256: 'b'.repeat(64),
    packagedSha256: digest,
  }), false);
  assert.equal(matchesPackagedExecutable({
    runningPath: '/tmp/unrelated/other',
    launchedPath: '/downloads/ShadowCode.AppImage',
    runningSha256: digest,
    packagedSha256: digest,
  }), false);
  assert.equal(matchesPackagedExecutable({
    runningPath: '/tmp/appimage_extracted_123/usr/bin/shadowcode',
    launchedPath: '/downloads/ShadowCode.AppImage',
    runningSha256: digest,
  }), false);
});
