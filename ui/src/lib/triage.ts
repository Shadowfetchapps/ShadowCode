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
    /(^|\/)(dist|build|out|generated|__generated__|coverage)\//.test(lower) ||
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
