// Protected-job data consumer. Never imports or executes anything from a build artifact.
import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
import { createEnvelope, signEnvelope, publicKeyIdentity, parseTrustPolicy, LIMITS } from './native-release-auth.mjs';
import { withReleaseSnapshot, validateContext, validateStagedRelease, readReceipts, requireTransportLayout, retainBundle, releaseNames, readBytes } from './native-release-assets.mjs';

export async function signRelease({ bundleDir, trustDir, privateKey, context, outputDir }) {
  assert(trustDir && privateKey, 'Provisioned publisher trust and private signing key required');
  const version = validateContext(context), files = await requireTransportLayout(bundleDir, version, false);
  const verification = await readReceipts(bundleDir);
  await withReleaseSnapshot({ files, version, signed: false }, async (directory, observed) => {
    await validateStagedRelease({ directory, observed, verification, context, signed: false });
    const keyId = publicKeyIdentity(privateKey);
    // The independently reviewed tooling checkout supplies this bounded public policy.
    const policy = await readBytes(path.join(trustDir, 'policy'), LIMITS.trust);
    const entry = parseTrustPolicy(policy).keys.find(key => key.id === keyId);
    assert(entry, 'Private signing key is not in the provisioned trust policy');
    const bytes = await createEnvelope({ bundleDir: directory, version, commit: context.commit, keyId, keyEpoch: entry.epoch });
    await writeFile(path.join(directory, 'RELEASE-AUTH'), bytes, { flag: 'wx', mode: 0o400 });
    await writeFile(path.join(directory, 'RELEASE-AUTH.sig'), signEnvelope(bytes, privateKey), { flag: 'wx', mode: 0o400 });
    // Applies current floors/key ranges and verifies all three exact package snapshots.
    await validateStagedRelease({ directory, observed, verification, context, trustDir, signed: true });
    await retainBundle(directory, outputDir, releaseNames(version), verification);
  });
}
async function main() {
  const [bundleDir, trustDir, outputDir] = process.argv.slice(2);
  const privateKey = process.env.NATIVE_RELEASE_SIGNING_KEY_PEM;
  delete process.env.NATIVE_RELEASE_SIGNING_KEY_PEM;
  try {
    assert(bundleDir && trustDir && outputDir && process.argv.length === 5);
    const context = { repo: process.env.GITHUB_REPOSITORY, tag: process.env.GITHUB_REF_NAME, commit: process.env.GITHUB_SHA, runId: process.env.GITHUB_RUN_ID, runAttempt: process.env.GITHUB_RUN_ATTEMPT };
    await signRelease({ bundleDir, trustDir, privateKey, context, outputDir });
    console.log('Publisher signature created and verified for the required release asset set.');
  } catch {
    // Do not serialize crypto exceptions/arguments from a secret-bearing process.
    console.error('Release signing refused; no completed authenticated bundle was retained.');
    process.exitCode = 1;
  }
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await main();
