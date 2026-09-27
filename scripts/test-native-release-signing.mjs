import test from 'node:test';
import assert from 'node:assert/strict';
import { writeFile, readFile, readdir, rm, stat, symlink, chmod } from 'node:fs/promises';
import { writeFileSync } from 'node:fs';
import { generateKeyPairSync } from 'node:crypto';
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import { fixture, COMMIT, VERSION } from './publisher-fixtures.mjs';
import { IDENTITY, createEnvelope, signEnvelope } from './native-release-auth.mjs';
import { prepareRelease } from './prepare-native-release.mjs';
import { signRelease } from './sign-native-release.mjs';
import { publish } from './publish-native-release.mjs';
import { jsonBytes, releaseNames, readReceipts, requireTransportLayout } from './native-release-assets.mjs';
const context = { repo: IDENTITY.repository, tag: `v${VERSION}`, commit: COMMIT, runId: '123', runAttempt: '2' };
const staged = async f => {
  const outputDir = path.join(f.root, 'unsigned');
  await prepareRelease({ files: f.packages, checksums: path.join(f.bundle, 'SHA256SUMS'), source: f.source, verification: f.verification, context, outputDir });
  return outputDir;
};
const signing = (f, bundleDir, extra = {}) => signRelease({ bundleDir, trustDir: f.trustDir, privateKey: f.privateKey, context, outputDir: path.join(f.root, 'signed'), ...extra });
const testFixture = run => fixture(run, publish);

test('build handoff signs data without execution and publisher consumes all seven verified snapshots', () => testFixture(async f => {
  const input = await staged(f); await signing(f, input);
  const signed = path.join(f.root, 'signed');
  assert.deepEqual((await readdir(signed)).sort(), [...releaseNames(VERSION), 'VERIFICATION.json'].sort());
  const files = await requireTransportLayout(signed, VERSION, true);
  assert.equal((await f.execute({ files, verification: await readReceipts(signed) })).status, 'published');
  await assert.rejects(stat(path.join(f.root, 'candidate-executed')), { code: 'ENOENT' });
  for (const args of f.state.calls.filter(args => args[1] === 'upload')) assert(args[3].includes('shadowcode-publisher-snapshot-'));
}));
test('same input, current successful receipts and key create byte-identical release metadata across runs', () => testFixture(async f => {
  await signing(f, await staged(f));
  const first = path.join(f.root, 'signed');
  f.verification.run_id = '124'; f.verification.run_attempt = '1'; await f.rebuild();
  const secondContext = { ...context, runId: '124', runAttempt: '1' }, secondInput = path.join(f.root, 'unsigned-two');
  await prepareRelease({ files: f.packages, checksums: path.join(f.bundle, 'SHA256SUMS'), source: f.source, verification: f.verification, context: secondContext, outputDir: secondInput });
  await signing(f, secondInput, { context: secondContext, outputDir: path.join(f.root, 'signed-two') });
  for (const name of releaseNames(VERSION)) assert.deepEqual(await readFile(path.join(first, name)), await readFile(path.join(f.root, 'signed-two', name)), name);
}));
for (const field of ['runId', 'runAttempt', 'commit', 'tag']) test(`signing refuses mismatched expected ${field}`, () => testFixture(async f => {
  const input = await staged(f);
  const wrong = { runId: '999', runAttempt: '99', commit: 'f'.repeat(40), tag: 'v1.0.1' };
  await assert.rejects(signing(f, input, { context: { ...context, [field]: wrong[field] } }));
  await assert.rejects(stat(path.join(f.root, 'signed')), { code: 'ENOENT' });
}));
for (const status of ['missing', 'failed', 'skipped']) test(`signing and publishing refuse ${status} required gate receipts`, () => testFixture(async f => {
  const input = await staged(f), receipts = JSON.parse(await readFile(path.join(input, 'VERIFICATION.json')));
  if (status === 'missing') delete receipts.gates['native-window']; else receipts.gates['native-window'].status = status;
  await chmod(path.join(input, 'VERIFICATION.json'), 0o600);
  await writeFile(path.join(input, 'VERIFICATION.json'), jsonBytes(receipts));
  await assert.rejects(signing(f, input), /gate|verification/i);
  await assert.rejects(f.execute({ verification: receipts }), /gate|verification/i); assert.equal(f.state.calls.length, 0);
}));
test('signing rejects unknown key, missing trust, edited checksums, extra transport code, and existing output', () => testFixture(async f => {
  const input = await staged(f);
  await assert.rejects(signing(f, input, { privateKey: generateKeyPairSync('ed25519').privateKey }), /not in.*trust/);
  await assert.rejects(signing(f, input, { trustDir: null }), /Provisioned/);
  await writeFile(path.join(input, 'payload.mjs'), 'throw new Error("must never execute")');
  await assert.rejects(signing(f, input), /transport file set/); await rm(path.join(input, 'payload.mjs'));
  const sums = await readFile(path.join(input, 'SHA256SUMS'));
  await chmod(path.join(input, 'SHA256SUMS'), 0o600);
  await writeFile(path.join(input, 'SHA256SUMS'), Buffer.concat([sums, sums]));
  await assert.rejects(signing(f, input), /canonical package/); await writeFile(path.join(input, 'SHA256SUMS'), sums);
  await signing(f, input); const sig = await readFile(path.join(f.root, 'signed/RELEASE-AUTH.sig'));
  await assert.rejects(signing(f, input), /EEXIST/); assert.deepEqual(await readFile(path.join(f.root, 'signed/RELEASE-AUTH.sig')), sig);
}));
test('signing current trust floors cannot be bypassed by a retained old key', () => testFixture(async f => {
  const input = await staged(f), policy = path.join(f.trustDir, 'policy');
  await writeFile(policy, (await readFile(policy, 'utf8')).replace('minimum-epoch=1', 'minimum-epoch=2'));
  await assert.rejects(signing(f, input), /Retired/); await assert.rejects(stat(path.join(f.root, 'signed')), { code: 'ENOENT' });
}));
test('publisher refuses invalid signature, changed signed manifest claims and duplicate or absent signed metadata before gh', () => testFixture(async f => {
  const sig = path.join(f.bundle, 'RELEASE-AUTH.sig'), signature = await readFile(sig); await writeFile(sig, Buffer.alloc(64));
  await assert.rejects(f.execute(), /signature/); await writeFile(sig, signature);
  await assert.rejects(f.execute({ files: [...f.files, sig] }), /Duplicate/);
  await assert.rejects(f.execute({ files: f.files.filter(file => !file.endsWith('.sig')) }), /asset set/);
  const file = path.join(f.bundle, 'RELEASE-MANIFEST.json'), manifest = JSON.parse(await readFile(file));
  manifest.verification.gates.interface.scope = 'Everything verified'; await writeFile(file, jsonBytes(manifest));
  const bytes = await createEnvelope({ bundleDir: f.bundle, version: VERSION, commit: COMMIT, keyId: f.keyId, keyEpoch: 1 });
  await writeFile(path.join(f.bundle, 'RELEASE-AUTH'), bytes); await writeFile(sig, signEnvelope(bytes, f.privateKey));
  await assert.rejects(f.execute(), /gate claims/); assert.equal(f.state.calls.length, 0);
}));
test('local input mutation after authentication does not change uploaded private snapshot bytes', () => testFixture(async f => {
  const original = await readFile(f.files[0]); let mutated = false;
  const gh = args => { if (!mutated) { mutated = true; writeFileSync(f.files[0], 'changed after snapshot'); } return f.gh(args); };
  await f.execute({ gh }); assert.deepEqual(f.state.assets.get(path.basename(f.files[0])), original);
}));
test('symlink and FIFO inputs are refused without blocking or executing', () => testFixture(async f => {
  const source = f.files[0], original = await readFile(source); await rm(source); await symlink(path.join(f.bundle, 'SHA256SUMS'), source);
  await assert.rejects(f.execute(), /ELOOP/); await rm(source);
  assert.equal(spawnSync('mkfifo', [source]).status, 0);
  await assert.rejects(f.execute(), /Invalid snapshot/); await rm(source); await writeFile(source, original);
  assert.equal(f.state.calls.length, 0);
}));
test('repository identity or remote tag drift refuses mutations even with valid signatures', () => testFixture(async f => {
  f.state.repositoryId = '1'; await assert.rejects(f.execute(), /repository identity/);
  delete f.state.repositoryId; f.state.tagCommit = 'f'.repeat(40); await assert.rejects(f.execute(), /tag changed/);
  assert(f.state.calls.every(args => args[0] === 'api'));
}));

test('signing CLI keeps ephemeral private key material out of arguments, output and retained public bundle', () => testFixture(async f => {
  const input = await staged(f), outputDir = path.join(f.root, 'signed-cli');
  const privatePem = f.privateKey.export({ format: 'pem', type: 'pkcs8' });
  const command = new URL('./sign-native-release.mjs', import.meta.url).pathname;
  const env = { ...process.env, GITHUB_REPOSITORY: context.repo, GITHUB_REF_NAME: context.tag, GITHUB_SHA: context.commit, GITHUB_RUN_ID: context.runId, GITHUB_RUN_ATTEMPT: context.runAttempt, NATIVE_RELEASE_SIGNING_KEY_PEM: privatePem };
  let result = spawnSync(process.execPath, [command, input, f.trustDir, outputDir], { env, encoding: 'utf8', timeout: 20000 });
  assert.equal(result.status, 0, result.stderr);
  assert(!(result.stdout + result.stderr).includes(privatePem.trim()));
  for (const name of await readdir(outputDir)) assert(!(await readFile(path.join(outputDir, name))).includes(Buffer.from('PRIVATE KEY')));
  result = spawnSync(process.execPath, [command, input, f.trustDir, outputDir], { env: { ...env, NATIVE_RELEASE_SIGNING_KEY_PEM: 'invalid secret must never be printed' }, encoding: 'utf8', timeout: 20000 });
  assert.equal(result.status, 1); assert(!result.stderr.includes('invalid secret')); assert.match(result.stderr, /Release signing refused/);
}));
test('publisher rejects oversized metadata and canonical-JSON duplicate fields before gh', () => testFixture(async f => {
  const file = path.join(f.bundle, 'RELEASE-MANIFEST.json'), original = await readFile(file);
  await writeFile(file, Buffer.alloc(1024 * 1024 + 1)); await assert.rejects(f.execute(), /Invalid snapshot/);
  await writeFile(file, Buffer.from(original.toString().replace('"schema": 1,', '"schema": 1,\n  "schema": 1,')));
  await assert.rejects(f.execute(), /canonical JSON/); assert.equal(f.state.calls.length, 0);
}));
test('release workflow separates untrusted build from protected signing and write-authorized publication', async () => {
  const workflow = await readFile(new URL('../.github/workflows/release.yml', import.meta.url), 'utf8');
  const build = workflow.split('\n  build:')[1].split('\n  sign:')[0];
  const sign = workflow.split('\n  sign:')[1].split('\n  publish:')[0];
  const publication = workflow.split('\n  publish:')[1];
  assert(workflow.includes('permissions:\n  contents: read'));
  assert(!/secrets\.|contents: write|publish-native-release/.test(build));
  assert.match(sign, /needs: \[configuration, build\]/); assert.match(sign, /environment: \$\{\{ needs.configuration.outputs.environment \}\}/);
  assert(!/contents: write|npm |cargo |cache:/.test(sign));
  assert.match(sign, /ref: \$\{\{ needs.configuration.outputs.tooling \}\}/);
  assert.match(sign, /artifact-ids: \$\{\{ needs.build.outputs.artifact-id \}\}/);
  assert.match(publication, /artifact-ids: \$\{\{ needs.sign.outputs.artifact-id \}\}/);
  assert.match(publication, /needs: \[configuration, build, sign\]/); assert.match(publication, /contents: write/);
  assert(!/SIGNING_KEY|secrets\./.test(publication));
  assert.equal([...workflow.matchAll(/secrets\.NATIVE_RELEASE_SIGNING_KEY_PEM/g)].length, 1);
  for (const [, ref] of workflow.matchAll(/uses: ([^\s#]+)/g)) assert.match(ref, /@[a-f0-9]{40}$/);
});
test('workflow configuration fails closed without real provisioning; fixture-only inputs exercise its parser', () => testFixture(async f => {
  const workflow = await readFile(new URL('../.github/workflows/release.yml', import.meta.url), 'utf8');
  const body = /        run: \|\n([\s\S]*?)\n  build:/.exec(workflow)[1].replace(/^          /gm, '');
  const env = { ...process.env, GITHUB_REPOSITORY: context.repo, REPOSITORY_ID: IDENTITY.repositoryId, OWNER_ID: IDENTITY.ownerId, GITHUB_EVENT_NAME: 'push', GITHUB_REF_TYPE: 'tag', GITHUB_REF_NAME: context.tag, TOOLING_COMMIT: '', SIGNING_ENVIRONMENT: '', GITHUB_OUTPUT: path.join(f.root, 'configuration-output') };
  const run = extra => spawnSync('bash', ['-euo', 'pipefail', '-c', body], { env: { ...env, ...extra }, encoding: 'utf8', timeout: 20000 });
  assert.notEqual(run({}).status, 0);
  assert.equal(run({ TOOLING_COMMIT: 'e'.repeat(40), SIGNING_ENVIRONMENT: 'fixture-only' }).status, 0);
  assert.equal(await readFile(env.GITHUB_OUTPUT, 'utf8'), `tooling=${'e'.repeat(40)}\nenvironment=fixture-only\n`);
}));

for (const status of ['missing', 'failed', 'skipped']) test(`signing and publishing refuse ${status} built-project cleanup qualification`, () => testFixture(async f => {
  const input = await staged(f), receipts = JSON.parse(await readFile(path.join(input, 'VERIFICATION.json')));
  if (status === 'missing') delete receipts.gates['built-project-cleanup'];
  else { assert(receipts.gates['built-project-cleanup']); receipts.gates['built-project-cleanup'].status = status; }
  await chmod(path.join(input, 'VERIFICATION.json'), 0o600);
  await writeFile(path.join(input, 'VERIFICATION.json'), jsonBytes(receipts));
  await assert.rejects(signing(f, input), /gate|verification/i);
  await assert.rejects(f.execute({ verification: receipts }), /gate|verification/i);
  assert.equal(f.state.calls.length, 0);
}));
