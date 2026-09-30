import type { ReviewFile } from "../api";

/** Groups of changed files, riskiest first. */
export type TriageGroup =
  "config" | "dependencies" | "source" | "tests" | "generated" | "docs";

export const GROUP_LABELS: Record<TriageGroup, string> = {
  config: "Config and CI",
  dependencies: "Dependencies",
  source: "Source",
  tests: "Tests",
  generated: "Generated and build output",
  docs: "Docs",
};

const ORDER: TriageGroup[] = [
  "config",
  "dependencies",
  "source",
  "tests",
  "generated",
  "docs",
];

const LOCKFILES = new Set([
  "package-lock.json",
  "npm-shrinkwrap.json",
  "pnpm-lock.yaml",
  "yarn.lock",
  "Cargo.lock",
  "poetry.lock",
  "uv.lock",
  "go.sum",
  "Gemfile.lock",
  "composer.lock",
]);
const MANIFESTS = new Set([
  "package.json",
  "Cargo.toml",
  "pyproject.toml",
  "go.mod",
  "Gemfile",
  "composer.json",
  "setup.py",
  "setup.cfg",
  "Pipfile",
]);

/** Folders only build tools write into. */
const OUTPUT_FOLDERS = new Set(["dist", "out", "coverage"]);
/** Folders of hand-written code: a `build/` inside one is code too
 * (`src/commands/build/index.ts`). */
const SOURCE_FOLDERS = new Set(["src", "source", "lib", "app"]);
/** Files people write, which build tools do not produce. */
const HAND_WRITTEN =
  /\.(ts|tsx|jsx|go|rs|py|java|kt|swift|c|cc|cpp|h|hpp|cs|rb|php|vue|svelte|sh)$/;
/** JavaScript, which a `build/` folder holds as often as output: the scripts
 * that run the build (`build/webpack.base.conf.js`,
 * `scripts/build/release.mjs`, `commands/build/index.js`). */
const SCRIPT = /\.(js|mjs|cjs)$/;
/** A bundler's content-hashed name (`main.3f2a1b9c.js`,
 * `787.0e1d2c3b.chunk.js`): output wherever it is. */
const HASHED = /[.-](?=[0-9a-f]*\d)[0-9a-f]{8,}(\.chunk)?\.(js|mjs|cjs|css)$/;

/** Output of a build: in a build folder that is not inside a source folder,
 * and not a file people write (a `.d.ts` is generated). A `build/` folder
 * holds scripts too: JavaScript there is output only by a hashed name. */
function buildOutput(lower: string, lowerName: string): boolean {
  const folders = lower.split("/").slice(0, -1);
  const source = folders.findIndex((folder) => SOURCE_FOLDERS.has(folder));
  const outside = source < 0 ? folders : folders.slice(0, source);
  const output = outside.some((folder) => OUTPUT_FOLDERS.has(folder));
  if (!output && !outside.includes("build")) return false;
  if (lowerName.endsWith(".d.ts") || HASHED.test(lowerName)) return true;
  if (HAND_WRITTEN.test(lowerName)) return false;
  return output || !SCRIPT.test(lowerName);
}

/** Which group a changed file belongs to, by its path. */
export function groupOf(path: string): TriageGroup {
  const lower = path.toLowerCase();
  const name = path.split("/").pop() || path;
  const lowerName = name.toLowerCase();
  if (
    LOCKFILES.has(name) ||
    MANIFESTS.has(name) ||
    /^requirements.*\.txt$/.test(lowerName)
  )
    return "dependencies";
  if (
    /(^|\/)(generated|__generated__)\//.test(lower) ||
    /\.(min\.js|min\.css|map|pb\.go|g\.dart|snap)$/.test(lower) ||
    lower.includes("__snapshots__/")
  )
    return "generated";
  if (
    lower.startsWith(".github/") ||
    lower.startsWith(".gitlab") ||
    lower.startsWith(".circleci/") ||
    lower.startsWith(".husky/") ||
    /^(dockerfile|docker-compose.*|makefile|jenkinsfile|\.pre-commit-config\.yaml|\.editorconfig|\.gitignore|\.gitattributes)$/.test(
      lowerName,
    ) ||
    /^(tsconfig.*\.json|\.eslintrc.*|eslint\.config\..*|\.prettierrc.*|vite\.config\..*|vitest\.config\..*|webpack\.config\..*|babel\.config\..*|jest\.config\..*|playwright\.config\..*|tailwind\.config\..*|rustfmt\.toml|clippy\.toml|\.env\.example)$/.test(
      lowerName,
    )
  )
    return "config";
  // After config and CI: `.github/actions/build/action.yml` is CI.
  if (buildOutput(lower, lowerName)) return "generated";
  if (
    /(^|\/)(tests?|__tests__|spec|e2e)\//.test(lower) ||
    /(^test_|_test\.|\.test\.|\.spec\.)/.test(lowerName)
  )
    return "tests";
  if (/\.(md|mdx|rst|adoc)$/.test(lowerName) || lower.startsWith("docs/"))
    return "docs";
  return "source";
}

/** One line on what changed in a file, from its status and line counts. */
export function whyLine(file: ReviewFile): string {
  if (file.status === "added")
    return file.binary
      ? "new file"
      : `new file, ${file.added} line${file.added === 1 ? "" : "s"}`;
  if (file.status === "deleted")
    return file.binary
      ? "deleted"
      : `deleted, ${file.removed} line${file.removed === 1 ? "" : "s"}`;
  if (file.status === "unchanged") return "back to how it was";
  if (file.binary) return "binary file changed";
  if (file.added && !file.removed) return `${file.added} lines added`;
  if (file.removed && !file.added) return `${file.removed} lines removed`;
  return `${file.added} added, ${file.removed} removed`;
}

/** The files in groups, riskiest group first; within a group deletions
 * first, then the biggest changes. */
export function triage(
  files: ReviewFile[],
): { group: TriageGroup; label: string; files: ReviewFile[] }[] {
  const buckets = new Map<TriageGroup, ReviewFile[]>();
  for (const file of files) {
    const group = groupOf(file.path);
    buckets.set(group, [...(buckets.get(group) || []), file]);
  }
  return ORDER.filter((g) => buckets.has(g)).map((group) => ({
    group,
    label: GROUP_LABELS[group],
    files: [...(buckets.get(group) || [])].sort(
      (a, b) =>
        Number(b.status === "deleted") - Number(a.status === "deleted") ||
        b.added + b.removed - (a.added + a.removed) ||
        a.path.localeCompare(b.path),
    ),
  }));
}
