import test from "node:test";
import assert from "node:assert/strict";
import { generateKeyPairSync } from "node:crypto";
import { mkdtemp, writeFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const scanner = fileURLToPath(new URL("./check-secrets.mjs", import.meta.url));
const fakePrivate = "-----BEGIN PRIVATE KEY-----\nshortfake\n-----END PRIVATE KEY-----\n";

async function fixture(t) {
  const directory = await mkdtemp(path.join(tmpdir(), "shadowcode-secret-scan-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const env = {
    ...process.env,
    GIT_CONFIG_NOSYSTEM: "1",
    GIT_CONFIG_GLOBAL: "/dev/null",
    GIT_AUTHOR_NAME: "Secret scanner fixture",
    GIT_AUTHOR_EMAIL: "fixture@example.invalid",
    GIT_COMMITTER_NAME: "Secret scanner fixture",
    GIT_COMMITTER_EMAIL: "fixture@example.invalid",
  };
  const git = (...args) => {
    const result = spawnSync("git", ["-c", "core.hooksPath=/dev/null", ...args], {
      cwd: directory, env, encoding: "utf8", timeout: 10000,
    });
    // Never include command stdout/stderr: a fixture index can contain keys.
    assert.equal(result.error, undefined, "Fixture Git command failed to execute");
    assert.equal(result.status, 0, "Fixture Git command failed");
    return result.stdout;
  };
  git("init", "--quiet");
  const scan = (...args) => spawnSync(process.execPath, [scanner, ...args], {
    cwd: directory, env, encoding: "utf8", timeout: 10000,
  });
  const { privateKey, publicKey } = generateKeyPairSync("ed25519");
  const privatePem = privateKey.export({ type: "pkcs8", format: "pem" });
  const publicPem = publicKey.export({ type: "spki", format: "pem" });
  const body = privatePem.split("\n").filter(line => line && !line.startsWith("-----")).join("");
  assert.equal(body.length, 64, "Fixture must exercise short Ed25519 PKCS#8");
  return { directory, git, scan, privatePem, publicPem, body };
}

function assertResult(f, result, refused) {
  assert.equal(result.error, undefined, "Scanner did not complete normally");
  const output = `${result.stdout}${result.stderr}`;
  assert(!output.includes(f.privatePem) && !output.includes(f.body), "Scanner disclosed private key bytes");
  assert.equal(result.status, refused ? 1 : 0, refused ? "Scanner accepted a real Ed25519 private key" : "Scanner rejected a safe fixture");
  if (refused) assert.match(output, /Private key block/);
  return output;
}

test("tracked Ed25519 PKCS#8 private key is refused without exposing its value", async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.directory, "signing.pem"), f.privatePem);
  f.git("add", "--", "signing.pem");
  assertResult(f, f.scan(), true);
});

test("staged added Ed25519 key is refused after its working copy is sanitized", async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.directory, "signing.pem"), f.privatePem);
  f.git("add", "--", "signing.pem");
  await writeFile(path.join(f.directory, "signing.pem"), f.publicPem);
  assertResult(f, f.scan(), false);
  assertResult(f, f.scan("--staged"), true);
});

test("staged body-only private-key replacement is detected without PEM delimiters in the diff", async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.directory, "signing.pem"), fakePrivate);
  f.git("add", "--", "signing.pem"); f.git("commit", "--quiet", "-m", "fake fixture");
  await writeFile(path.join(f.directory, "signing.pem"), f.privatePem);
  f.git("add", "--", "signing.pem");
  assert(!f.git("diff", "--cached", "-U0", "--no-color").includes("BEGIN PRIVATE KEY"), "Fixture must omit unchanged PEM delimiters");
  await writeFile(path.join(f.directory, "signing.pem"), fakePrivate);
  assertResult(f, f.scan(), false);
  assertResult(f, f.scan("--staged"), true);
});

test("a staged key remains detectable when its working file was deleted", async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.directory, "signing.pem"), f.privatePem);
  f.git("add", "--", "signing.pem");
  await rm(path.join(f.directory, "signing.pem"));
  assertResult(f, f.scan("--staged"), true);
});

test("public SPKI and short fake private fixtures are allowed in tracked and staged content", async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.directory, "public.pem"), f.publicPem);
  await writeFile(path.join(f.directory, "fake.pem"), fakePrivate);
  f.git("add", "--", "public.pem", "fake.pem");
  assertResult(f, f.scan(), false);
  assertResult(f, f.scan("--staged"), false);
});

test("whitespace does not turn a short fake into a key or hide a later real private block", async t => {
  const f = await fixture(t), file = path.join(f.directory, "fixture.pem");
  const paddedFake = fakePrivate.replace("shortfake", `shortfake${"\n".repeat(100)}`);
  await writeFile(file, paddedFake); f.git("add", "--", "fixture.pem");
  assertResult(f, f.scan("--staged"), false);
  await writeFile(file, `${paddedFake}${f.privatePem}`); f.git("add", "--", "fixture.pem");
  assertResult(f, f.scan(), true);
  await writeFile(file, paddedFake);
  assertResult(f, f.scan("--staged"), true);
});

test("removing an existing private key is not blocked by deleted diff text", async t => {
  const f = await fixture(t);
  await writeFile(path.join(f.directory, "signing.pem"), f.privatePem);
  f.git("add", "--", "signing.pem"); f.git("commit", "--quiet", "-m", "temporary sensitive fixture");
  f.git("rm", "--quiet", "--", "signing.pem");
  assertResult(f, f.scan("--staged"), false);
});

test("existing API-key detection also inspects the complete changed index blob without logging values", async t => {
  const f = await fixture(t), token = `sk-or-v1-${"b".repeat(64)}`;
  await writeFile(path.join(f.directory, "config.txt"), token);
  f.git("add", "--", "config.txt");
  await writeFile(path.join(f.directory, "config.txt"), "safe working copy");
  const result = f.scan("--staged"), output = `${result.stdout}${result.stderr}`;
  assert.equal(result.status, 1);
  assert.match(output, /OpenRouter API key/);
  assert(!output.includes(token), "Scanner disclosed a token value");
});
