import { describe, expect, it } from "vitest";
import { branchNameProblem, signedInText, syncText } from "./forge";

it("explains bad branch names the way the engine refuses them", () => {
  for (const good of ["feature/login", "fix-12", "user/a.b_c"])
    expect(branchNameProblem(good)).toBe("");
  expect(branchNameProblem("")).toMatch(/Enter/);
  expect(branchNameProblem("-rf")).toMatch(/dash/);
  expect(branchNameProblem("a b")).toMatch(/spaces/);
  for (const bad of [
    "a..b",
    "x.lock",
    "/x",
    "x/",
    ".x",
    "a/.b",
    "HEAD",
    "a@{1}",
  ])
    expect(branchNameProblem(bad)).toBe("That is not a valid branch name");
});

it("describes where a branch stands against its remote", () => {
  expect(
    syncText({ repo: true, branch: "main", upstream: "origin/main" }),
  ).toBe("Up to date with origin/main");
  expect(
    syncText({
      repo: true,
      branch: "main",
      upstream: "origin/main",
      ahead: 2,
      behind: 1,
    }),
  ).toBe("2 to push · 1 to pull · origin/main");
  expect(
    syncText({ repo: true, branch: "feat", remote: "origin", ahead: 1 }),
  ).toBe("Not on origin yet · 1 commit to publish");
  expect(syncText({ repo: true, branch: "feat" })).toBe("No remote configured");
  expect(syncText({ repo: true, branch: null, detached: true })).toMatch(
    /Detached/,
  );
});

describe("signed-in line", () => {
  it("names the account instead of repeating the CLI's status line", () => {
    expect(signedInText("Logged in to github.com account octo (keyring)")).toBe(
      "Signed in as octo",
    );
    expect(
      signedInText(
        "Logged in to gitlab.com as octo (/home/me/.config/glab-cli/config.yml)",
      ),
    ).toBe("Signed in as octo");
    expect(signedInText("Token valid (oauth_token)")).toBe("Token valid");
  });
});
