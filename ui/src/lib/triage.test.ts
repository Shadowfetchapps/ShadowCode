import { expect, it } from "vitest";
import { groupOf, triage, whyLine } from "./triage";
import type { ReviewFile } from "../api";

const file = (path: string, over: Partial<ReviewFile> = {}): ReviewFile => ({
  path,
  status: "modified",
  source: "checkpoint",
  added: 1,
  removed: 1,
  binary: false,
  ...over,
});

it("groups files by what they are", () => {
  expect(groupOf("src/app.ts")).toBe("source");
  expect(groupOf("src/app.test.ts")).toBe("tests");
  expect(groupOf("tests/test_api.py")).toBe("tests");
  expect(groupOf(".github/workflows/ci.yml")).toBe("config");
  expect(groupOf("tsconfig.json")).toBe("config");
  expect(groupOf("package-lock.json")).toBe("dependencies");
  expect(groupOf("web/package.json")).toBe("dependencies");
  expect(groupOf("requirements-dev.txt")).toBe("dependencies");
  expect(groupOf("dist/app.min.js")).toBe("generated");
  expect(groupOf("src/__snapshots__/a.snap")).toBe("generated");
  expect(groupOf("README.md")).toBe("docs");
  expect(groupOf("docs/guide/setup.txt")).toBe("docs");
});

it("files only build output as generated, not code in a folder named build", () => {
  expect(groupOf("packages/web/dist/index.js")).toBe("generated");
  expect(groupOf("dist/index.mjs")).toBe("generated");
  expect(groupOf("out/extension.js")).toBe("generated");
  expect(groupOf("build/index.html")).toBe("generated");
  expect(groupOf("build/static/js/main.3f2a1b9c.js")).toBe("generated");
  expect(groupOf("build/static/js/787.0e1d2c3b.chunk.js")).toBe("generated");
  expect(groupOf("build/static/css/main.5b6c7d8e.css")).toBe("generated");
  // The scripts that run a build, in JavaScript projects without src/.
  expect(groupOf("build/webpack.base.conf.js")).toBe("source");
  expect(groupOf("build/check-versions.js")).toBe("source");
  expect(groupOf("scripts/build/release.mjs")).toBe("source");
  expect(groupOf("packages/cli/commands/build/index.js")).toBe("source");
  expect(groupOf("tools/build/rollup.cjs")).toBe("source");
  expect(groupOf("dist/index.d.ts")).toBe("generated");
  expect(groupOf("coverage/lcov-report/index.html")).toBe("generated");
  expect(groupOf("src/generated/client.ts")).toBe("generated");
  expect(groupOf("src/build/sign.ts")).toBe("source");
  expect(groupOf("src/build/helpers.js")).toBe("source");
  expect(groupOf("packages/cli/src/commands/build/index.ts")).toBe("source");
  expect(groupOf("tools/out/x.go")).toBe("source");
  expect(groupOf(".github/actions/build/action.yml")).toBe("config");
});

it("orders groups by risk and files by deletions then size", () => {
  const groups = triage([
    file("README.md"),
    file("src/small.ts", { added: 1, removed: 0 }),
    file("src/big.ts", { added: 80, removed: 20 }),
    file("src/gone.ts", { status: "deleted", added: 0, removed: 5 }),
    file(".github/workflows/ci.yml"),
    file("Cargo.lock"),
  ]);
  expect(groups.map((g) => g.label)).toEqual([
    "Config and CI",
    "Dependencies",
    "Source",
    "Docs",
  ]);
  expect(groups[2].files.map((f) => f.path)).toEqual([
    "src/gone.ts",
    "src/big.ts",
    "src/small.ts",
  ]);
});

it("says what changed in a few words", () => {
  expect(whyLine(file("a", { status: "added", added: 120, removed: 0 }))).toBe(
    "new file, 120 lines",
  );
  expect(whyLine(file("a", { status: "deleted", added: 0, removed: 1 }))).toBe(
    "deleted, 1 line",
  );
  expect(whyLine(file("a", { added: 3, removed: 0 }))).toBe("3 lines added");
  expect(whyLine(file("a", { added: 3, removed: 2 }))).toBe(
    "3 added, 2 removed",
  );
});
