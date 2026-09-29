#!/usr/bin/env node
// Regenerate the upgrade fixtures in native/core/tests/fixtures/upgrade/.
//
// For each released version, export that tag's source, add the generator
// (native/core/tests/fixtures/upgrade/generator/) as an integration test and
// run it with that release's own engine. The generator writes a profile the
// way the release does (conversations, jobs, goals, automations, compare
// records, usage, config and secrets layout) and dumps it as text.
//
//   node scripts/generate-upgrade-fixtures.mjs [--workdir DIR] [v0.28.0 v0.34.2 ...]
//
// Needs git, tar and the Rust 1.95.0 toolchain. Each release is built once in
// a shared target directory under --workdir (default: a folder in the system
// temporary directory), so the first run takes a while. When a new version is
// released, add its tag to RELEASES and run this for that tag only.
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const repo = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const generator = path.join(repo, 'native/core/tests/fixtures/upgrade/generator');
const fixtures = path.join(repo, 'native/core/tests/fixtures/upgrade');

// Published releases that shipped the native profile (schema 25 and later).
// 0.33.0, 0.34.0 and 0.34.1 were failed, unpublished release attempts.
export const RELEASES = [
  'v0.28.0', 'v0.28.1', 'v0.29.0', 'v0.30.0', 'v0.30.1', 'v0.30.2', 'v0.31.0', 'v0.31.1',
  'v0.32.0', 'v0.33.1', 'v0.34.2',
];

// Generator snippets and the first release that has the feature.
const SNIPPETS = [
  ['compare', '0.31.0'],
  ['automations', '0.32.0'],
  ['editor_drafts', '0.33.0'],
];

const newer = (a, b) => {
  const [x, y] = [a, b].map(v => v.replace(/^v/, '').split('.').map(Number));
  for (let i = 0; i < 3; i += 1) if (x[i] !== y[i]) return x[i] > y[i];
  return true;
};

export function generatorSource(tag) {
  const included = SNIPPETS.filter(([, since]) => newer(tag, since)).map(([name]) => name);
  const parts = [readFileSync(path.join(generator, 'base.rs'), 'utf8')];
  for (const name of included) parts.push(readFileSync(path.join(generator, `${name}.rs`), 'utf8'));
  parts.push(`async fn version_specific(ctx: &mut Ctx) {\n${included.map(n => `    ${n}(ctx).await;\n`).join('')}    let _ = ctx;\n}\n`);
  return parts.join('\n');
}

function main() {
  const args = process.argv.slice(2);
  let workdir = path.join(tmpdir(), 'shadowcode-upgrade-fixtures');
  const index = args.indexOf('--workdir');
  if (index >= 0) {
    workdir = path.resolve(args[index + 1]);
    args.splice(index, 2);
  }
  const tags = args.length ? args : RELEASES;
  const home = path.join(workdir, 'home');
  mkdirSync(home, { recursive: true });
  const env = {
    ...process.env,
    CARGO_TARGET_DIR: path.join(workdir, 'target'),
    CARGO_HOME: process.env.CARGO_HOME || path.join(process.env.HOME, '.cargo'),
    RUSTUP_HOME: process.env.RUSTUP_HOME || path.join(process.env.HOME, '.rustup'),
    RUSTUP_TOOLCHAIN: '1.95.0',
    // The generated profile is isolated; HOME only keeps stray tools away
    // from the real one.
    HOME: home,
  };
  for (const tag of tags) {
    const version = tag.replace(/^v/, '');
    const source = path.join(workdir, 'src', tag);
    rmSync(source, { recursive: true, force: true });
    mkdirSync(source, { recursive: true });
    const archive = execFileSync('git', ['-C', repo, 'archive', '--format=tar', tag], { maxBuffer: 1 << 30 });
    execFileSync('tar', ['-x', '-C', source, '--exclude=ui', '--exclude=docs/images'], { input: archive });
    writeFileSync(path.join(source, 'native/core/tests/zz_upgrade_fixture.rs'), generatorSource(tag));
    const out = path.join(fixtures, version);
    console.log(`\n== ${tag}: generating ${path.relative(repo, out)}`);
    const run = spawnSync('cargo', ['test', '-p', 'shadowcode-core', '--test', 'zz_upgrade_fixture', '--locked', '--',
      '--ignored', '--exact', 'generate_upgrade_fixture', '--nocapture'], {
      cwd: source, env: { ...env, SHADOWCODE_FIXTURE_OUT: out }, stdio: 'inherit',
    });
    if (run.status !== 0) {
      console.error(`${tag}: generation failed (exit ${run.status})`);
      process.exitCode = 1;
      continue;
    }
    const manifest = JSON.parse(readFileSync(path.join(out, 'manifest.json'), 'utf8'));
    console.log(`${tag}: schema ${manifest.schema_version}, ${JSON.stringify(manifest.row_counts)}`);
    if (manifest.skipped?.length) console.log(`${tag}: skipped ${manifest.skipped.join('; ')}`);
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) main();
