import type { PaletteItem } from "../overlays";
import type { AdvancedTab, SettingsSection } from "../Settings";
import type { DrawerTab } from "../Drawer";

export type PaletteActions = {
  newTask: () => void;
  chooseModel: () => void;
  openProject: () => void;
  panel: (tab: DrawerTab) => void;
  compare: () => void;
  comparisons: () => void;
  settings: (section?: SettingsSection, advanced?: AdvancedTab) => void;
  exportTask: (format: "md" | "json") => void;
  stop: () => void;
  toggleTheme: () => void;
  help: () => void;
};

/** The command palette (Ctrl+K) entries, in display order. */
export function paletteItems(a: PaletteActions): PaletteItem[] {
  return [
    { id: "new", label: "New task", hint: "Ctrl+N", run: a.newTask },
    {
      id: "model",
      label: "Choose a model",
      hint: "Ctrl+M",
      run: a.chooseModel,
    },
    {
      id: "project",
      label: "Open project",
      hint: "Ctrl+P",
      run: a.openProject,
    },
    { id: "changes", label: "Review changes", run: () => a.panel("changes") },
    {
      id: "git",
      label: "Commit, push and open a pull request",
      run: () => a.panel("git"),
    },
    { id: "compare", label: "Compare models on this task", run: a.compare },
    {
      id: "comparisons",
      label: "Comparisons in this project",
      run: a.comparisons,
    },
    { id: "files", label: "Browse files", run: () => a.panel("files") },
    {
      id: "terminal",
      label: "Open a terminal",
      hint: "Ctrl+`",
      run: () => a.panel("terminal"),
    },
    {
      id: "preview",
      label: "Preview the running app",
      run: () => a.panel("preview"),
    },
    {
      id: "sessions",
      label: "Manage tasks · rename, branch, export, delete",
      run: () => a.panel("sessions"),
    },
    { id: "accounts", label: "Accounts", run: () => a.settings("accounts") },
    { id: "local", label: "Local models", run: () => a.settings("local") },
    {
      id: "code",
      label: "Code intelligence · language servers and search",
      run: () => a.settings("code"),
    },
    { id: "voice", label: "Voice input", run: () => a.settings("voice") },
    {
      id: "permissions",
      label: "Permissions & network",
      run: () => a.settings("permissions"),
    },
    {
      id: "appearance",
      label: "Appearance and notifications",
      run: () => a.settings("appearance"),
    },
    {
      id: "remote",
      label: "Remote access · follow tasks from your phone",
      run: () => a.settings("remote"),
    },
    {
      id: "about",
      label: "About ShadowCode · version and updates",
      run: () => a.settings("about"),
    },
    {
      id: "goals",
      label: "Goals and milestones",
      run: () => a.panel("goals"),
    },
    {
      id: "automations",
      label: "Automations · run a prompt on a schedule",
      run: () => a.panel("automations"),
    },
    {
      id: "issues",
      label: "Start from an issue",
      run: () => a.panel("issues"),
    },
    {
      id: "background",
      label: "Background processes · dev servers and watchers",
      run: () => a.panel("background"),
    },
    {
      id: "worktrees",
      label: "Worktrees",
      run: () => a.panel("worktrees"),
    },
    {
      id: "skills",
      label: "Skills and instructions",
      run: () => a.settings("advanced", "skills"),
    },
    {
      id: "health",
      label: "Workspace health",
      run: () => a.settings("advanced", "health"),
    },
    {
      id: "export",
      label: "Export this task as Markdown",
      hint: "Ctrl+Shift+E",
      run: () => a.exportTask("md"),
    },
    {
      id: "export-json",
      label: "Export this task as JSON",
      hint: "Complete event records",
      run: () => a.exportTask("json"),
    },
    { id: "stop", label: "Stop the agent", hint: "Ctrl+.", run: a.stop },
    {
      id: "settings",
      label: "Settings",
      hint: "Ctrl+,",
      run: () => a.settings(),
    },
    {
      id: "theme",
      label: "Toggle light / dark appearance",
      run: a.toggleTheme,
    },
    { id: "help", label: "Keyboard shortcuts", hint: "?", run: a.help },
  ];
}
