import { expect, it, vi } from "vitest";
import { paletteItems, type PaletteActions } from "./palette";
import { DRAWER_TABS } from "../Drawer";
import { TOOL_VIEWS } from "../ToolsTab";

const SETTINGS_SECTIONS = [
  "accounts",
  "local",
  "code",
  "voice",
  "permissions",
  "appearance",
  "remote",
  "advanced",
  "about",
];

it("the command palette reaches every drawer panel and Settings page", () => {
  const panels: string[] = [];
  const sections: string[] = [];
  const actions: PaletteActions = {
    newTask: vi.fn(),
    chooseModel: vi.fn(),
    openProject: vi.fn(),
    panel: (tab) => panels.push(tab),
    compare: vi.fn(),
    comparisons: vi.fn(),
    settings: (section) => sections.push(section || "accounts"),
    exportTask: vi.fn(),
    stop: vi.fn(),
    toggleTheme: vi.fn(),
    help: vi.fn(),
  };
  const items = paletteItems(actions);
  items.forEach((item) => item.run());
  for (const tab of DRAWER_TABS.map((t) => t.id).filter((t) => t !== "tools"))
    expect(panels).toContain(tab);
  for (const view of TOOL_VIEWS.map((t) => t.id))
    expect(panels).toContain(view);
  for (const section of SETTINGS_SECTIONS) expect(sections).toContain(section);
  // Every entry is unique and reads as a label, not an identifier.
  expect(new Set(items.map((i) => i.id)).size).toBe(items.length);
  for (const item of items) expect(item.label[0]).toMatch(/[A-Z]/);
});
