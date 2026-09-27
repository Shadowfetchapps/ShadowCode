#!/usr/bin/env node
// Fails when a tracked file contains something that looks like a real
// credential. Test fixtures use short fake values ("sk-or-good") that don't
// match these patterns.
//
//   node scripts/check-secrets.mjs                  # scan tracked files
//   node scripts/check-secrets.mjs --staged         # scan changed index files too
//   node scripts/check-secrets.mjs --value-file F   # also look for the exact
//                                                   # value in F (never printed)
import { execFileSync } from "node:child_process";
import { readFileSync } from "node:fs";

const PATTERNS = [
  ["OpenRouter API key", /sk-or-v1-[0-9a-f]{48,}/],
  ["Anthropic API key", /sk-ant-(?:api|admin)\d{2}-[A-Za-z0-9_-]{60,}/],
  ["OpenAI API key", /sk-(?:proj-|svcacct-)?[A-Za-z0-9_-]{20}T3BlbkFJ[A-Za-z0-9_-]{20,}/],
  ["OpenAI project key", /sk-proj-[A-Za-z0-9_-]{80,}/],
  ["Google API key", /AIza[0-9A-Za-z_-]{35}/],
  ["xAI API key", /xai-[A-Za-z0-9]{60,}/],
  ["GitHub token", /\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b/],
  // Ed25519 PKCS#8 is only 64 base64 characters. Count body characters,
  // not whitespace, so short fake fixtures remain permitted.
  [
    "Private key block",
    /-----BEGIN (?:RSA |EC |DSA |OPENSSH |ENCRYPTED )?PRIVATE KEY-----([A-Za-z0-9+/=\s]{64,})-----END/,
    64,
  ],
];
const FORBIDDEN_FILES = [/(^|\/)secrets\.env$/, /(^|\/)\.env(\.(?!example$)[^/]+)?$/];

const args = process.argv.slice(2);
const valueFile = args.includes("--value-file")
  ? args[args.indexOf("--value-file") + 1]
  : null;
const exact = valueFile ? readFileSync(valueFile, "utf8").trim() : "";

const git = (...a) =>
  execFileSync("git", a, { encoding: "utf8", maxBuffer: 1 << 30 });
const files = git("ls-files", "-z").split("\0").filter(Boolean);
const problems = [];
const MAX_FILE_BYTES = 20_000_000;

function scanContent(label, buf) {
  if (buf.length > MAX_FILE_BYTES || buf.includes(0)) return; // binary
  const text = buf.toString("utf8");
  for (const [name, re, minimumBodyCharacters] of PATTERNS) {
    for (const match of text.matchAll(new RegExp(re.source, "g"))) {
      if (minimumBodyCharacters && match[1].replace(/\s/g, "").length < minimumBodyCharacters) continue;
      const line = text.slice(0, match.index).split("\n").length;
      problems.push(`${label}:${line}: looks like a ${name}`);
      break;
    }
  }
  if (exact && exact.length >= 16 && text.includes(exact))
    problems.push(`${label}: contains the value from ${valueFile}`);
}

for (const file of files) {
  if (FORBIDDEN_FILES.some((re) => re.test(file)))
    problems.push(`${file}: secrets file is tracked`);
  let buf;
  try {
    buf = readFileSync(file);
  } catch {
    continue; // deleted in the working tree
  }
  scanContent(file, buf);
}

if (args.includes("--staged")) {
  // A body-only PEM edit has no delimiters in a zero-context diff. Read each
  // changed index blob instead, even if its working copy was sanitized/deleted.
  const staged = git("diff", "--cached", "--name-only", "--diff-filter=ACMRT", "--no-ext-diff", "-z")
    .split("\0").filter(Boolean);
  for (const file of staged) {
    try {
      const object = `:${file}`;
      const size = Number(git("cat-file", "-s", object).trim());
      if (!Number.isSafeInteger(size) || size < 0) throw new Error("Invalid index size");
      if (size > MAX_FILE_BYTES) continue;
      const buf = execFileSync("git", ["cat-file", "blob", object], {
        maxBuffer: MAX_FILE_BYTES + 1,
        stdio: ["ignore", "pipe", "pipe"],
      });
      scanContent(`staged ${file}`, buf);
    } catch {
      // Never print subprocess errors: they may retain the secret blob bytes.
      problems.push(`${file}: could not inspect staged content (values not shown)`);
    }
  }
}

if (problems.length) {
  console.error(`Possible secrets found (values not shown):\n  ${problems.join("\n  ")}`);
  process.exit(1);
}
console.log(`No secrets found in ${files.length} tracked files.`);
