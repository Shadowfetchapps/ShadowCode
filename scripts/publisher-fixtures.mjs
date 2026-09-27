import assert from 'node:assert/strict';
import { generateKeyPairSync } from 'node:crypto';
import { mkdtemp, mkdir, writeFile, rm } from 'node:fs/promises';
import { readFileSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { assetNames, createEnvelope, signEnvelope, publicKeyIdentity, IDENTITY } from './native-release-auth.mjs';
import { digest, GATES, REQUIRED_GATES, scriptDigest, validateVerification } from './native-release-verification.mjs';

export const COMMIT = 'a'.repeat(40);
export const VERSION = '1.0.0'; // Fixture only; not a production release or cutover.
export async function fixture(run, publish) {
  const root = await mkdtemp(path.join(tmpdir(), 'shadowcode-publisher-fixture-'));
  const bundle = path.join(root, 'bundle'), trustDir = path.join(root, 'trust');
  await mkdir(bundle); await mkdir(trustDir);
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  const publicPem = publicKey.export({ format: 'pem', type: 'spki' });
  const keyId = publicKeyIdentity(publicPem);
  await writeFile(path.join(trustDir, `${keyId}.pem`), publicPem);
  await writeFile(path.join(trustDir, 'policy'), `ShadowCode-Release-Trust-v1\nrepository=${IDENTITY.repository}\nrepository-id=${IDENTITY.repositoryId}\nowner-id=${IDENTITY.ownerId}\ntarget=${IDENTITY.target}\nchannel=${IDENTITY.channel}\nminimum-epoch=1\nminimum-version=1.0.0\nkeys=ed25519-spki-sha256\nkey=1\t${keyId}\t1.0.0\t999.999.999\n`);
  const packages = assetNames(VERSION).map(([, name]) => path.join(bundle, name));
  await writeFile(packages[0], `#!/bin/sh\nprintf executed > '${root}/candidate-executed'\n`, { mode: 0o755 });
  for (const file of packages.slice(1)) await writeFile(file, `fixture ${path.basename(file)}`);
  const verification = { schema: 1, commit: COMMIT, run_id: '123', run_attempt: '2', artifacts: {}, gates: {} };
  const source = { cargo_lock_sha256: 'b'.repeat(64), ui_lock_sha256: 'c'.repeat(64), runtime_pin: 'commit=' + 'd'.repeat(40) };
  async function rebuild() {
    const artifacts = Object.fromEntries(await Promise.all(packages.map(async file => [path.basename(file), await digest(file)])));
    verification.artifacts = artifacts;
    verification.gates = Object.fromEntries(REQUIRED_GATES.map(gate => [gate, { schema: 1, gate, commit: COMMIT, run_id: verification.run_id, run_attempt: verification.run_attempt, script_sha256: scriptDigest(gate), status: 'passed', exit_code: 0, required_skips: [], optional_checks: [], ...(GATES[gate].artifacts ? { artifacts } : {}) }]));
    await writeFile(path.join(bundle, 'SHA256SUMS'), Object.entries(artifacts).map(([name, sha]) => `${sha}  ${name}\n`).join(''));
    const manifest = { schema: 1, tag: `v${VERSION}`, commit: COMMIT, target: IDENTITY.target, ...source, verification: validateVerification(verification, COMMIT, artifacts), assets: artifacts };
    await writeFile(path.join(bundle, 'RELEASE-MANIFEST.json'), `${JSON.stringify(manifest, null, 2)}\n`);
    await writeFile(path.join(bundle, 'VERIFICATION.json'), `${JSON.stringify(verification, null, 2)}\n`);
    const envelope = await createEnvelope({ bundleDir: bundle, version: VERSION, commit: COMMIT, keyId, keyEpoch: 1 });
    await writeFile(path.join(bundle, 'RELEASE-AUTH'), envelope);
    await writeFile(path.join(bundle, 'RELEASE-AUTH.sig'), signEnvelope(envelope, privateKey));
  }
  await rebuild();
  const files = [...packages, ...['SHA256SUMS', 'RELEASE-MANIFEST.json', 'RELEASE-AUTH', 'RELEASE-AUTH.sig'].map(name => path.join(bundle, name))];
  const state = { release: null, assets: new Map(), calls: [], failUpload: false, apiFailure: false };
  const gh = args => {
    state.calls.push(args); assert(!args.includes('--clobber'));
    if (args[0] === 'api') {
      if (state.apiFailure) throw new Error('HTTP 403');
      if (args[1] === `repos/${IDENTITY.repository}`) return JSON.stringify({ id: state.repositoryId ?? IDENTITY.repositoryId, owner: { id: IDENTITY.ownerId } });
      if (args[1].includes('/git/ref/tags/')) return JSON.stringify({ object: { type: 'commit', sha: state.tagCommit ?? COMMIT } });
      if (!state.release) return '[[]]';
      return JSON.stringify([[{ ...state.release, assets: [...state.assets.keys()].map(name => ({ name })) }]]);
    }
    switch (args[1]) {
      case 'create': assert(args.includes('--draft')); state.release = { draft: true, tag_name: `v${VERSION}` }; break;
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
  const execute = (overrides = {}) => publish({ repo: IDENTITY.repository, tag: `v${VERSION}`, commit: COMMIT, files, verification, trustDir, runId: '123', runAttempt: '2', gh, ...overrides });
  try { await run({ root, bundle, trustDir, packages, files, state, execute, verification, rebuild, privateKey, keyId, source, gh }); }
  finally { await rm(root, { recursive: true, force: true }); }
}
