import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import { afterEach, expect, it, vi } from "vitest";
import {
  api,
  type RulesOverview,
  type RulesPreview,
  type SkillCheck,
} from "../../api";
import { RulesPage, sourceLabel } from "./RulesPage";
import { SkillChecker } from "./SkillChecker";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

function overview(overrides: Partial<RulesOverview> = {}): RulesOverview {
  return {
    profile: {
      path: "/home/me/.config/shadowcode/profile",
      exists: true,
      agents_md: {
        content: "Answer in British English.\n",
        hash: "h1",
        path: "/home/me/.config/shadowcode/profile/AGENTS.md",
      },
    },
    workspace: "/work/demo",
    share_with_cli_agents: true,
    items: [
      {
        id: "profile:AGENTS.md",
        scope: "profile",
        source: "profile",
        kind: "rules",
        name: "AGENTS.md",
        path: "/home/me/.config/shadowcode/profile/AGENTS.md",
        description: "",
        enabled: true,
        bytes: 27,
        hash: "h1",
        overridden_by: null,
      },
      {
        id: "profile:skills/careful-review/SKILL.md",
        scope: "profile",
        source: "profile",
        kind: "skill",
        name: "careful-review",
        path: "/home/me/.config/shadowcode/profile/skills/careful-review/SKILL.md",
        description: "Review a change for real defects",
        enabled: true,
        bytes: 900,
        hash: "s1",
        overridden_by: ".shadow/skills/careful-review.md",
      },
      {
        id: "profile:imports/team/skills/ship/SKILL.md",
        scope: "profile",
        source: "import:team",
        kind: "skill",
        name: "ship",
        path: "/home/me/.config/shadowcode/profile/imports/team/skills/ship/SKILL.md",
        description: "Ship a release",
        enabled: false,
        bytes: 300,
        hash: "s2",
        overridden_by: null,
      },
      {
        id: "project:CLAUDE.md",
        scope: "project",
        source: "project",
        kind: "rules",
        name: "CLAUDE.md",
        path: "CLAUDE.md",
        description: "",
        enabled: true,
        bytes: 40,
        hash: "p1",
        overridden_by: null,
      },
    ],
    imports: [
      {
        name: "team",
        url: "https://github.com/team/rules.git",
        path: "/home/me/.config/shadowcode/profile/imports/team",
        commit: {
          commit: "abcdef1234567890",
          short: "abcdef1234",
          subject: "Add ship skill",
          date: "2026-09-20T10:00:00Z",
        },
      },
    ],
    issues: ["Your profile skill /careful-review is not used here"],
    starters: [
      {
        name: "careful-review",
        title: "Careful review",
        summary: "Review a change.",
        installed: true,
        path: "/p/skills/careful-review/SKILL.md",
      },
      {
        name: "cli-design",
        title: "CLI design",
        summary: "Design a CLI.",
        installed: false,
        path: "/p/skills/cli-design/SKILL.md",
      },
    ],
    limits: {
      profile_file_bytes: 16000,
      profile_total_bytes: 24000,
      total_bytes: 48000,
      skill_index_entries: 48,
      skill_index_bytes: 6000,
    },
    ...overrides,
  };
}

const preview: RulesPreview = {
  workspace: "/work/demo",
  runners: [
    {
      id: "shadowcode",
      label: "ShadowCode's own agent",
      mechanism: "System prompt; skills load with load_skill.",
      delivered: true,
      sharing_off: false,
      native_files: [],
      native_skill_folders: [],
      preview: {
        items: [
          {
            path: "/home/me/.config/shadowcode/profile/AGENTS.md",
            kind: "profile-rules",
            included: true,
            reason: "Included",
            bytes: 27,
            total_bytes: 27,
            from_line: null,
            to_line: null,
            entries: [],
            truncated: false,
          },
        ],
        included_bytes: 300,
        estimated_tokens: 100,
        truncated: false,
      },
    },
    {
      id: "claude",
      label: "Claude Code",
      mechanism:
        "--append-system-prompt-file, and --plugin-dir for profile skills.",
      delivered: true,
      sharing_off: false,
      native_files: ["CLAUDE.md"],
      native_skill_folders: [".claude/skills/"],
      preview: {
        items: [
          {
            path: "CLAUDE.md",
            kind: "project-rules",
            included: false,
            reason: "Claude Code reads this file itself",
            bytes: 0,
            total_bytes: null,
            from_line: null,
            to_line: null,
            entries: [],
            truncated: false,
          },
        ],
        included_bytes: 600,
        estimated_tokens: 200,
        truncated: false,
      },
    },
  ],
};

function mockApi(data = overview()) {
  vi.spyOn(api, "rules").mockResolvedValue(data);
  vi.spyOn(api, "rulesPreview").mockResolvedValue(preview);
  vi.spyOn(api, "rulesExport").mockResolvedValue({
    targets: [
      {
        id: "claude",
        label: "Claude Code",
        home: "/home/me/.claude",
        enabled: false,
        links: [
          {
            link: "/home/me/.claude/rules/shadowcode-profile.md",
            target: "/home/me/.config/shadowcode/profile/AGENTS.md",
            state: "available",
          },
        ],
        created: [],
      },
      {
        id: "codex",
        label: "Codex",
        home: "/home/me/.codex",
        enabled: false,
        links: [
          {
            link: "/home/me/.codex/AGENTS.md",
            target: "/home/me/.config/shadowcode/profile/AGENTS.md",
            state: "blocked",
          },
        ],
        created: [],
      },
    ],
  });
}

it("names where each item comes from", () => {
  expect(sourceLabel({ scope: "profile", source: "profile" })).toBe(
    "Your profile",
  );
  expect(sourceLabel({ scope: "profile", source: "import:team" })).toBe(
    "Imported: team",
  );
  expect(sourceLabel({ scope: "project", source: "project" })).toBe(
    "This project",
  );
});

it("lists profile and project items with their sources and conflicts", async () => {
  mockApi();
  render(<RulesPage onToast={vi.fn()} />);
  const profile = await screen.findByRole("list", {
    name: "From your profile",
  });
  expect(within(profile).getByText("Skill: careful-review")).toBeTruthy();
  expect(
    within(profile).getByText(
      "Not used here: .shadow/skills/careful-review.md has the same name.",
    ),
  ).toBeTruthy();
  expect(within(profile).getByText(/Imported: team/)).toBeTruthy();
  const project = screen.getByRole("list", { name: "From this project" });
  expect(within(project).getByText("Rules: CLAUDE.md")).toBeTruthy();
  expect(
    screen.getByText("Your profile skill /careful-review is not used here"),
  ).toBeTruthy();
  expect(screen.getByText(/Commit abcdef1234 · Add ship skill/)).toBeTruthy();
});

it("switches a project item off for the project on screen", async () => {
  mockApi();
  const toggle = vi
    .spyOn(api, "setRuleEnabled")
    .mockResolvedValue({ ok: true });
  render(<RulesPage onToast={vi.fn()} />);
  const box = await screen.findByRole("checkbox", {
    name: "Use Rules: CLAUDE.md",
  });
  fireEvent.click(box);
  await waitFor(() =>
    expect(toggle).toHaveBeenCalledWith(
      "project:CLAUDE.md",
      false,
      "/work/demo",
    ),
  );
  const off = screen.getByRole("checkbox", { name: "Use Skill: ship" });
  expect((off as HTMLInputElement).checked).toBe(false);
  fireEvent.click(off);
  await waitFor(() =>
    expect(toggle).toHaveBeenCalledWith(
      "profile:imports/team/skills/ship/SKILL.md",
      true,
    ),
  );
});

it("saves the profile AGENTS.md with the hash it loaded", async () => {
  mockApi();
  const save = vi
    .spyOn(api, "saveProfileRules")
    .mockResolvedValue({ ok: true, hash: "h2" });
  const toast = vi.fn();
  render(<RulesPage onToast={toast} />);
  const editor = await screen.findByLabelText(
    "AGENTS.md in your profile, sent to every agent",
  );
  const button = screen.getByRole("button", { name: "Save rules" });
  expect((button as HTMLButtonElement).disabled).toBe(true);
  fireEvent.change(editor, { target: { value: "Prefer small commits.\n" } });
  fireEvent.click(screen.getByRole("button", { name: "Save rules" }));
  await waitFor(() =>
    expect(save).toHaveBeenCalledWith("Prefer small commits.\n", "h1"),
  );
  await waitFor(() => expect(toast).toHaveBeenCalledWith("Rules saved", "ok"));
});

it("warns when the rules are longer than agents read", async () => {
  mockApi();
  render(<RulesPage onToast={vi.fn()} />);
  const editor = await screen.findByLabelText(
    "AGENTS.md in your profile, sent to every agent",
  );
  fireEvent.change(editor, { target: { value: "x".repeat(16_001) } });
  const note = screen.getByText(/16,001 bytes/);
  expect(note.className).toBe("health-bad");
});

it("shows what each agent reads, like the context inventory", async () => {
  mockApi();
  render(<RulesPage onToast={vi.fn()} />);
  expect(
    await screen.findByRole("list", {
      name: "What ShadowCode's own agent reads",
    }),
  ).toBeTruthy();
  expect(
    screen.getByText("About 100 tokens estimated · 300 bytes"),
  ).toBeTruthy();
  fireEvent.change(screen.getByLabelText("Agent"), {
    target: { value: "claude" },
  });
  const claude = screen.getByRole("list", { name: "What Claude Code reads" });
  expect(
    within(claude).getByText("Claude Code reads this file itself"),
  ).toBeTruthy();
  expect(
    screen.getByText(/--append-system-prompt-file, and --plugin-dir/),
  ).toBeTruthy();
});

it("installs only the starter skills the user picks", async () => {
  mockApi();
  const install = vi
    .spyOn(api, "installStarters")
    .mockResolvedValue({ installed: ["cli-design"], skipped: [] });
  render(<RulesPage onToast={vi.fn()} />);
  const installed = await screen.findByRole("checkbox", {
    name: /Careful review/,
  });
  expect((installed as HTMLInputElement).disabled).toBe(true);
  const button = screen.getByRole("button", { name: "Install selected" });
  expect((button as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByRole("checkbox", { name: /CLI design/ }));
  fireEvent.click(screen.getByRole("button", { name: "Install selected" }));
  await waitFor(() => expect(install).toHaveBeenCalledWith(["cli-design"]));
});

it("imports from Git and removes an import only after confirming", async () => {
  mockApi();
  const add = vi.spyOn(api, "importRules").mockResolvedValue({
    name: "rules",
    url: "https://github.com/me/rules.git",
    commit: { commit: "1", short: "1234567890", subject: "x", date: null },
  });
  const remove = vi
    .spyOn(api, "removeRulesImport")
    .mockResolvedValue({ ok: true });
  const toast = vi.fn();
  render(<RulesPage onToast={toast} />);
  const input = await screen.findByLabelText("Repository address");
  fireEvent.change(input, {
    target: { value: " https://github.com/me/rules.git " },
  });
  fireEvent.click(screen.getByRole("button", { name: "Import" }));
  await waitFor(() =>
    expect(add).toHaveBeenCalledWith("https://github.com/me/rules.git"),
  );
  await waitFor(() =>
    expect(toast).toHaveBeenCalledWith("Imported rules at 1234567890", "ok"),
  );
  fireEvent.click(screen.getByRole("button", { name: "Remove" }));
  expect(remove).not.toHaveBeenCalled();
  const dialog = screen.getByRole("dialog", { name: "Remove team?" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Remove" }));
  await waitFor(() => expect(remove).toHaveBeenCalledWith("team"));
});

it("exports only when asked and says which files are left alone", async () => {
  mockApi();
  const exporter = vi.spyOn(api, "setRulesExport").mockResolvedValue({});
  render(<RulesPage onToast={vi.fn()} />);
  const use = await screen.findByRole("button", { name: "Use in Claude Code" });
  expect(exporter).not.toHaveBeenCalled();
  expect(screen.getByText(/a file already exists here/)).toBeTruthy();
  fireEvent.click(use);
  await waitFor(() => expect(exporter).toHaveBeenCalledWith("claude", true));
});

it("turns sending to vendor CLIs off", async () => {
  mockApi();
  const sharing = vi
    .spyOn(api, "setRulesSharing")
    .mockResolvedValue({ ok: true });
  render(<RulesPage onToast={vi.fn()} />);
  const box = await screen.findByRole("checkbox", {
    name: /Send rules and skills to Claude Code/,
  });
  fireEvent.click(box);
  await waitFor(() => expect(sharing).toHaveBeenCalledWith(false));
});

it("shows a load error with a retry", async () => {
  const rules = vi
    .spyOn(api, "rules")
    .mockRejectedValueOnce(new Error("rulebook.json is not valid"))
    .mockResolvedValue(overview());
  vi.spyOn(api, "rulesPreview").mockResolvedValue(preview);
  vi.spyOn(api, "rulesExport").mockResolvedValue({ targets: [] });
  render(<RulesPage onToast={vi.fn()} />);
  expect(await screen.findByText("rulebook.json is not valid")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Try again|Retry/ }));
  await waitFor(() => expect(rules).toHaveBeenCalledTimes(2));
  expect(
    await screen.findByRole("list", { name: "From your profile" }),
  ).toBeTruthy();
});

it("the skill checker lists findings and never offers to edit", async () => {
  const report: SkillCheck = {
    ok: false,
    checked: 7,
    errors: 1,
    warnings: 1,
    infos: 0,
    note: "The checker only reports. It never changes a file.",
    findings: [
      {
        severity: "error",
        code: "front-matter",
        scope: "profile",
        path: "/p/skills/broken/SKILL.md",
        name: "",
        message: "Invalid workflow front matter",
        fix: "Fix the front matter between the --- lines.",
      },
      {
        severity: "warning",
        code: "unsafe-content",
        scope: "project",
        path: ".shadow/skills/yolo.md",
        name: "yolo",
        message: 'This text turns off approvals: "approval_policy = never".',
        fix: "Remove or reword it unless you are sure it is safe.",
      },
    ],
  };
  const check = vi.spyOn(api, "skillCheck").mockResolvedValue(report);
  render(<SkillChecker />);
  const list = await screen.findByRole("list", {
    name: "Skill checker findings",
  });
  expect(within(list).getByText("Error").className).toBe("health-bad");
  expect(within(list).getByText(".shadow/skills/yolo.md")).toBeTruthy();
  expect(
    screen.getByText("7 files checked · 1 errors · 1 warnings · 0 notes"),
  ).toBeTruthy();
  expect(screen.queryByRole("button", { name: /Fix|Edit/ })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Check again" }));
  await waitFor(() => expect(check).toHaveBeenCalledTimes(2));
});
