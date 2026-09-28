import path from 'node:path';

// The WebDriver starts AppImages through their runtime, so /proc/<pid>/exe
// points at the extracted ELF rather than the AppImage wrapper. Accept that
// different path only when its basename and bytes match the executable
// extracted from the exact AppImage passed to the test.
export function matchesPackagedExecutable({
  runningPath,
  launchedPath,
  runningSha256,
  packagedSha256,
  packagedExecutable = 'shadowcode',
}) {
  if (runningPath === launchedPath) return true;
  return typeof packagedSha256 === 'string'
    && /^[a-f0-9]{64}$/.test(packagedSha256)
    && runningSha256 === packagedSha256
    && path.basename(runningPath.replace(/ \(deleted\)$/, '')) === packagedExecutable;
}
