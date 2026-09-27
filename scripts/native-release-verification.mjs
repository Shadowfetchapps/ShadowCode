// Explicit release qualification receipts. Local evidence, not a signature.
import assert from 'node:assert/strict';
import { createHash } from 'node:crypto';
import { createReadStream } from 'node:fs';
import { mkdir, readFile, writeFile, rename, rm } from 'node:fs/promises';
import { spawn, execFileSync } from 'node:child_process';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const version = String.raw`VERSION=$(node -p "require('./src-tauri/tauri.conf.json').version")`;
const packaged = `${version}\nBINARY="$GITHUB_WORKSPACE/target/release/bundle/appimage/ShadowCode_${'${VERSION}'}_amd64.AppImage"`;
export const GATES = {
  'release-tests': { script: 'node --test --test-reporter=tap scripts/test-native-release.mjs scripts/test-native-release-verification.mjs scripts/test-publisher-auth.mjs scripts/test-native-release-signing.mjs' },
  'release-auth': { script: 'node scripts/check-secrets.mjs\nnode --test --test-reporter=tap scripts/test-native-release-auth.mjs scripts/test-check-secrets.mjs', scope: 'Publisher authentication parser, crypto, replay and private-snapshot fixtures with ephemeral keys; production signing and installer integration are separate.' },
  'release-tag': { script: `${version}\n` + String.raw`test "$GITHUB_REF_NAME" = "v$VERSION"
test "$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)" = "$VERSION"
test "$(node -p "require('./ui/package.json').version")" = "$VERSION"
grep -Fxq "X-ShadowCode-Version=$VERSION" packaging/shadow-agent.desktop` },
  interface: { script: 'npm --prefix ui ci\nnpm --prefix ui run build\nnpm --prefix ui test' },
  'glib-backport': { script: 'rustfmt +1.95.0 --check --edition 2021 scripts/fixtures/native-glib-variant.rs\nnode --test --test-reporter=tap scripts/test-native-glib-backport.mjs\nnode scripts/test-native-glib-variant.mjs', scope: 'Exact vendored GLib source/provenance, packaged-notice fixtures and optimized iterator regression; native package/window qualification is separate.' },
  'native-source': { script: 'cargo +1.95.0 fmt --all --check\ncargo +1.95.0 clippy --workspace --all-targets --locked -- -D warnings\ncargo +1.95.0 build -p shadowcode-desktop --locked\ncargo +1.95.0 test --workspace --locked -- --skip worktrees::cleanup::tests::actual_built_project_cleanup_is_bounded_and_completes', scope: 'Default non-ignored Rust suite; the exact built-project cleanup fixture is transferred to the required built-project-cleanup gate. Other explicitly optional ignored tests are excluded and recorded.' },
  'built-project-cleanup': { script: String.raw`mkdir -p artifacts/built-project-cleanup
CARGO_TERM_COLOR=never SHADOWCODE_CLEANUP_BUILT_FIXTURE="$GITHUB_WORKSPACE" SHADOWCODE_CLEANUP_BUILT_SCOPE=ui cargo +1.95.0 test -p shadowcode-core --release --lib worktrees::cleanup::tests::actual_built_project_cleanup_is_bounded_and_completes --locked -- --ignored --exact --nocapture 2>&1 | tee artifacts/built-project-cleanup/test.log
grep -Eq '^test result: ok\. 1 passed; 0 failed; 0 ignored;' artifacts/built-project-cleanup/test.log`, scope: 'Release-profile cleanup over the actual npm-installed ui/node_modules and built ui/dist on the CI host, with entries/bytes/time reported. Full multi-GiB Cargo-cache qualification is separate.' },
  'native-behavior': { script: 'node scripts/test-native-cli.mjs\nnode scripts/test-native-stress.mjs\nnode scripts/test-native-tui.mjs\nnode scripts/test-native-mcp-server.mjs\nSHADOW_MCP_TRANSPORT=http node scripts/test-native-mcp-server.mjs' },
  'native-window': { script: "cargo +1.95.0 install tauri-driver --version 2.0.6 --locked\nxvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- node scripts/test-native-desktop.mjs\nxvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- node scripts/test-native-markdown.mjs", scope: 'Native fixture window and embedded Markdown worker under the real security policy; authenticated cloud-consent check is optional and reported separately. This is X11 source-binary evidence, not Wayland or standalone package qualification.' },
  'managed-runtime': { script: 'bash scripts/build-llama.cpp.sh --no-user-install\nnode --test --test-reporter=tap scripts/test-llama-runtime.mjs' },
  packages: { artifacts: true, script: `${version}\n` + String.raw`node scripts/build-native.mjs
node --test --test-reporter=tap scripts/test-native-packaging-env.mjs
node scripts/check-native-package.mjs "target/release/bundle/appimage/ShadowCode_${'${VERSION}'}_amd64.AppImage" "target/release/bundle/deb/ShadowCode_${'${VERSION}'}_amd64.deb"
node scripts/test-native-runtime-write-errors.mjs
node scripts/test-native-runtime-sources.mjs
node scripts/test-native-runtime.mjs` },
  'clean-host-packages': { artifacts: true, unchanged: true, scope: 'Both packages installed or extracted in a network-disabled Debian 13 runtime container; offline CLI status, Debian launcher/icon metadata and a visible first GUI window under Xvfb. GUI interaction, physical Wayland, local inference, model downloads and installer rollback remain separate.', script: `${version}\n` + String.raw`docker build -f scripts/clean-host-runtime.Dockerfile -t shadowcode-clean-runtime:ci scripts
node scripts/test-clean-host-packages.mjs "target/release/bundle/appimage/ShadowCode_${'${VERSION}'}_amd64.AppImage" "target/release/bundle/deb/ShadowCode_${'${VERSION}'}_amd64.deb" artifacts/native-package/SHA256SUMS` },
  'packaged-behavior': { artifacts: true, unchanged: true, scope: 'Packaged fixture behavior on the CI build host; authenticated cloud-consent check is optional and reported separately.', script: `${packaged}\n` + String.raw`SHADOW_DESKTOP_BINARY="$BINARY" SHADOW_CLI_ARGS='["--appimage-extract-and-run"]' node scripts/test-native-cli.mjs
SHADOW_DESKTOP_BINARY="$BINARY" SHADOW_CLI_ARGS='["--appimage-extract-and-run"]' node scripts/test-native-tui.mjs
SHADOW_DESKTOP_BINARY="$BINARY" SHADOW_DESKTOP_ARGS='["--appimage-extract-and-run","ui"]' SHADOW_NATIVE_DEFAULT_PROFILE=1 xvfb-run -a -s '-screen 0 1440x1100x24' dbus-run-session -- node scripts/test-native-desktop.mjs
SHADOW_DESKTOP_BINARY="$BINARY" SHADOW_CLI_ARGS='["--appimage-extract-and-run"]' SHADOW_MCP_TRANSPORT=http node scripts/test-native-mcp-server.mjs
npm --prefix scripts/native-mcp-peer ci --ignore-scripts --no-audit --no-fund
SHADOW_DESKTOP_BINARY="$BINARY" SHADOW_CLI_ARGS='["--appimage-extract-and-run"]' node scripts/test-native-mcp-peer.mjs` },
  installer: { artifacts: true, unchanged: true, script: 'node --test --test-reporter=tap scripts/test-install-auth.mjs\nbash scripts/test-install-appimage.sh' },
};
export const REQUIRED_GATES = Object.keys(GATES);
const OPTIONAL_RUST_TESTS = new Set([
  'live_qwen3_agent_and_gemma4_vision_from_the_ollama_store',
  'live_local_acceptance_from_explicit_models',
  'live_three_model_compare_cancels_queued_models',
  'live_install_and_status_of_the_real_server',
  'live_example_com_and_search', 'saved_results_page_parses',
  'live_managed_install_and_semantic_search', 'real_servers_smoke',
  'voice::whisper::tests::transcribes_with_a_local_model',
  'voice::capture::tests::records_from_the_default_microphone',
]);
export const scriptDigest = gate => createHash('sha256').update(GATES[gate].script).digest('hex');
export async function digest(file) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(file)) hash.update(chunk);
  return hash.digest('hex');
}
export async function packageHashes() {
  const { version: releaseVersion } = JSON.parse(await readFile('src-tauri/tauri.conf.json', 'utf8'));
  const base = 'target/release/bundle';
  const files = [`${base}/appimage/ShadowCode_${releaseVersion}_amd64.AppImage`, `${base}/appimage/ShadowCode_${releaseVersion}_appimage-runtime-sources.tar.gz`, `${base}/deb/ShadowCode_${releaseVersion}_amd64.deb`];
  return Object.fromEntries(await Promise.all(files.map(async file => [path.basename(file), await digest(file)])));
}

// Recognize test-runner skips separately from ordinary prose. Existing optional
// checks have narrow, explicit scopes; all other reported skips fail the gate.
export function inspectLine(gate, raw, requiredSkips, optional) {
  const line = raw.replace(/\x1b\[[0-?]*[ -/]*[@-~]/g, '').trim();
  const ignored = /^test (.+) \.\.\. ignored(?:,.*)?$/.exec(line);
  if (ignored) {
    if (gate === 'native-source' && OPTIONAL_RUST_TESTS.has(ignored[1])) optional.add(line);
    else requiredSkips.add(line);
    return;
  }
  if (['native-window', 'packaged-behavior'].includes(gate) && line === 'ok  no Ready cloud row on this machine: consent step skipped') {
    optional.add('Authenticated cloud-consent check skipped: no Ready cloud row.'); return;
  }
  if (/^ok .*# SKIP\b/i.test(line) || /^# (?:skipped|todo) [1-9]\d*$/.test(line)
      || /^(?:Test Files|Tests)\s+.*\b[1-9]\d* (?:skipped|todo)\b/.test(line)
      || /^SKIP\b/.test(line)) requiredSkips.add(line);
}

export function validateVerification(verification, commit, assets) {
  assert(verification?.schema === 1, 'Required release verification receipts are missing');
  assert.equal(verification.commit, commit, 'Release verification commit mismatch');
  assert.match(verification.run_id, /^\d+$/, 'Release verification run identity missing');
  assert.match(verification.run_attempt, /^\d+$/, 'Release verification attempt missing');
  assert.deepEqual(Object.keys(verification.gates).sort(), [...REQUIRED_GATES].sort(), 'Required verification gate set mismatch');
  assert(Object.keys(verification.artifacts).length > 0, 'Verification package identities missing');
  assert.deepEqual(verification.artifacts, assets, 'Verification package set or bytes changed');
  for (const [name, sha] of Object.entries(verification.artifacts)) {
    assert.match(sha, /^[a-f0-9]{64}$/);
    assert.equal(assets[name], sha, `Verified package changed: ${name}`);
  }
  const gates = {};
  for (const gate of REQUIRED_GATES) {
    const receipt = verification.gates[gate];
    assert.equal(receipt.schema, 1, `Invalid verification receipt: ${gate}`);
    assert.equal(receipt.gate, gate);
    assert.equal(receipt.commit, commit, `Stale verification receipt: ${gate}`);
    assert.equal(receipt.run_id, verification.run_id, `Wrong workflow run: ${gate}`);
    assert.equal(receipt.run_attempt, verification.run_attempt, `Wrong workflow attempt: ${gate}`);
    assert.equal(receipt.script_sha256, scriptDigest(gate), `Verification command changed: ${gate}`);
    assert.equal(receipt.status, 'passed', `Required verification gate did not pass: ${gate}`);
    assert.equal(receipt.exit_code, 0, `Required verification command failed: ${gate}`);
    assert.deepEqual(receipt.required_skips, [], `Required verification checks skipped: ${gate}`);
    assert(Array.isArray(receipt.optional_checks), `Optional verification scope missing: ${gate}`);
    if (GATES[gate].artifacts) assert.deepEqual(receipt.artifacts, verification.artifacts, `Verification artifact mismatch: ${gate}`);
    gates[gate] = { status: 'passed', script_sha256: receipt.script_sha256, scope: GATES[gate].scope || 'All checks in this required gate.', optional_checks: receipt.optional_checks };
  }
  return { schema: 1, status: 'required_gates_passed', scope: 'Declared Linux CI gates only; excluded checks are not verified. This does not establish publisher authentication, first GUI launch, local-model operation or complete clean-host usability.', gates };
}

export async function readVerification(directory, commit, runId, attempt, artifacts) {
  const gates = {};
  for (const gate of REQUIRED_GATES) {
    try { gates[gate] = JSON.parse(await readFile(path.join(directory, `${gate}.json`), 'utf8')); }
    catch (error) { throw new Error(`Required verification receipt missing or invalid: ${gate}`, { cause: error }); }
  }
  const result = { schema: 1, commit, run_id: runId, run_attempt: attempt, artifacts, gates };
  validateVerification(result, commit, artifacts);
  return result;
}

export async function runGate({ gate, directory, commit, runId, attempt, execute, hashes = packageHashes }) {
  assert(GATES[gate], `Unknown release gate: ${gate}`);
  assert.match(commit, /^[a-f0-9]{40}$/);
  assert.match(runId, /^\d+$/);
  assert.match(attempt, /^\d+$/);
  await mkdir(directory, { recursive: true });
  const file = path.join(directory, `${gate}.json`);
  // A crashed rerun must not leave its previous successful receipt usable.
  await rm(file, { force: true });
  const skips = new Set(), optional = new Set();
  const receipt = { schema: 1, gate, commit, run_id: runId, run_attempt: attempt, script_sha256: scriptDigest(gate), status: 'failed', exit_code: null, required_skips: [], optional_checks: [] };
  try {
    const before = GATES[gate].unchanged ? await hashes() : null;
    const code = await execute(GATES[gate].script, line => inspectLine(gate, line, skips, optional));
    receipt.exit_code = code;
    receipt.required_skips = [...skips].sort();
    receipt.optional_checks = [...optional].sort();
    receipt.status = code === 0 ? (skips.size ? 'skipped' : 'passed') : 'failed';
    if (receipt.status === 'passed' && GATES[gate].artifacts) {
      receipt.artifacts = await hashes();
      if (before) assert.deepEqual(receipt.artifacts, before, 'Packages changed during required verification');
    }
  } catch (error) { receipt.status = 'failed'; receipt.error = String(error.message || error); }
  await writeFile(`${file}.pending`, `${JSON.stringify(receipt, null, 2)}\n`);
  await rename(`${file}.pending`, file);
  assert.equal(receipt.status, 'passed', `Required release gate ${gate} ${receipt.status}: ${receipt.error || receipt.required_skips.join('; ') || receipt.exit_code}`);
  return receipt;
}

export function executeScript(script, line) {
  return new Promise((resolve, reject) => {
    const child = spawn('bash', ['-euo', 'pipefail', '-c', script], { stdio: ['ignore', 'pipe', 'pipe'] });
    for (const [stream, output] of [[child.stdout, process.stdout], [child.stderr, process.stderr]]) {
      let pending = '';
      stream.setEncoding('utf8');
      stream.on('data', text => {
        output.write(text);
        pending += text;
        let boundary;
        while ((boundary = pending.indexOf('\n')) >= 0) { line(pending.slice(0, boundary)); pending = pending.slice(boundary + 1); }
        if (pending.length > 65536) { line(pending); pending = ''; }
      });
      stream.on('end', () => { if (pending) line(pending); });
    }
    child.once('error', reject);
    child.once('close', (code, signal) => signal ? reject(new Error(`Gate terminated by ${signal}`)) : resolve(code));
  });
}
async function main() {
  const [mode, gate] = process.argv.slice(2);
  assert.equal(mode, 'run', 'Usage: node scripts/native-release-verification.mjs run GATE');
  const commit = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
  assert.equal(commit, process.env.GITHUB_SHA);
  await runGate({ gate, directory: 'artifacts/release-verification', commit, runId: process.env.GITHUB_RUN_ID, attempt: process.env.GITHUB_RUN_ATTEMPT, execute: executeScript });
}
if (process.argv[1] && import.meta.url === pathToFileURL(path.resolve(process.argv[1])).href) await main();
