// Test-only helper. Keys exist only in temporary fixture directories.
import { generateKeyPairSync } from 'node:crypto';
import { mkdir, writeFile, readFile } from 'node:fs/promises';
import path from 'node:path';
import { assetNames, createEnvelope, signEnvelope, publicKeyIdentity, digestBytes, IDENTITY } from './native-release-auth.mjs';

export const COMMIT = 'c'.repeat(40);
export async function provision(root, privateFile) {
  const pair = generateKeyPairSync('ed25519');
  const publicPem = pair.publicKey.export({ format: 'pem', type: 'spki' });
  const privatePem = pair.privateKey.export({ format: 'pem', type: 'pkcs8' });
  const id = publicKeyIdentity(publicPem), trust = path.join(root, 'release/trust');
  await mkdir(trust, { recursive: true });
  await writeFile(privateFile, privatePem, { mode: 0o600 });
  await writeFile(path.join(trust, `${id}.pem`), publicPem);
  await writeFile(path.join(trust, 'policy'), `ShadowCode-Release-Trust-v1\nrepository=${IDENTITY.repository}\nrepository-id=${IDENTITY.repositoryId}\nowner-id=${IDENTITY.ownerId}\ntarget=${IDENTITY.target}\nchannel=stable\nminimum-epoch=1\nminimum-version=0.28.0\nkeys=ed25519-spki-sha256\nkey=1\t${id}\t0.28.0\t999.999.999\n`);
  await writeFile(path.join(root, 'release/install-policy'), 'ShadowCode-Install-Policy-v1\nfirst-authenticated-version=0.28.0\n');
}
export async function signFixture(file, privateFile) {
  const version = /^ShadowCode_(\d+\.\d+\.\d+)_amd64\.AppImage$/.exec(path.basename(file))?.[1];
  if (!version) throw new Error('Invalid fixture filename');
  const directory = path.dirname(file), assets = {};
  for (const [role, name] of assetNames(version)) {
    const target = path.join(directory, name);
    if (role !== 'appimage') await writeFile(target, `Fixture ${role} ${version}\n`);
    assets[name] = digestBytes(await readFile(target));
  }
  await writeFile(path.join(directory, 'SHA256SUMS'), Object.entries(assets).map(([name, hash]) => `${hash}  ${name}\n`).join(''));
  await writeFile(path.join(directory, 'RELEASE-MANIFEST.json'), `${JSON.stringify({ schema: 1, tag: `v${version}`, commit: COMMIT, target: IDENTITY.target, assets, fixture_only: true }, null, 2)}\n`);
  const privatePem = await readFile(privateFile);
  const envelope = await createEnvelope({ bundleDir: directory, version, commit: COMMIT, keyId: publicKeyIdentity(privatePem), keyEpoch: 1 });
  await writeFile(path.join(directory, 'RELEASE-AUTH'), envelope);
  await writeFile(path.join(directory, 'RELEASE-AUTH.sig'), signEnvelope(envelope, privatePem));
}
if (process.argv[2] === 'provision') await provision(process.argv[3], process.argv[4]);
if (process.argv[2] === 'sign') await signFixture(process.argv[3], process.argv[4]);
