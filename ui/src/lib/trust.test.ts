import { expect, it } from "vitest";
import {
  isProjectTrustError,
  sameWorkspacePath,
  trustErrorHint,
  trustPromptFor,
  trustRequestFor,
} from "./trust";

it("detects the job-gate trust error so the dialog can open", () => {
  expect(
    isProjectTrustError(
      "Error: Trust this project before starting an agent task",
    ),
  ).toBe(true);
  expect(isProjectTrustError("Permission denied")).toBe(false);
});

it("adds a banner hint only for the job-gate trust error", () => {
  expect(
    trustErrorHint("Error: Trust this project before starting an agent task"),
  ).toBe(
    "This folder is not trusted. Click Trust this folder, then Trust and open, and send the task again.",
  );
  expect(trustErrorHint("Permission denied")).toBeUndefined();
});

it("builds a trust dialog request from the open workspace path", () => {
  expect(trustRequestFor("/home/user/app/", { level: "workspace" })).toEqual({
    path: "/home/user/app/",
    name: "app",
    permissions: { level: "workspace" },
  });
});

it("treats trailing slashes as the same workspace", () => {
  expect(sameWorkspacePath("/home/user/app", "/home/user/app/")).toBe(true);
  expect(sameWorkspacePath("/home/user/app", "/home/user/other")).toBe(false);
  expect(sameWorkspacePath("", "/home/user/app")).toBe(false);
});

it("opens the trust dialog for a restored untrusted folder instead of sending a job", () => {
  expect(
    trustPromptFor("/home/user/app/", false, { level: "workspace" }),
  ).toEqual({
    path: "/home/user/app/",
    name: "app",
    permissions: { level: "workspace" },
  });
  expect(trustPromptFor("/home/user/app", true)).toBeNull();
  expect(trustPromptFor("/home/user/app", undefined)).toBeNull();
  expect(trustPromptFor("", false)).toBeNull();
});
