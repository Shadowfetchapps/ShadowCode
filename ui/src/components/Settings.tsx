import { useState, type ReactNode } from "react";
import { Dialog } from "./Dialog";
import type { DownloadModel, Health } from "../api";
import { AccountsPage } from "./settings/AccountsPage";
import { CodeIntelPage } from "./settings/CodeIntelPage";
import { VoicePage } from "./settings/VoicePage";
import { LocalModelsPage } from "./settings/LocalModelsPage";
import { AppearancePage, PermissionsPage } from "./settings/PreferencePages";
import { AdvancedPage, type AdvancedTab } from "./settings/AdvancedPage";
import { RemotePage } from "./settings/RemotePage";
import { AboutPage } from "./settings/AboutPage";
import { RolesPage, type RolesPageProps } from "./settings/RolesPage";
import { RulesPage } from "./settings/RulesPage";

export type SettingsSection =
  | "accounts"
  | "local"
  | "roles"
  | "code"
  | "rules"
  | "voice"
  | "permissions"
  | "appearance"
  | "remote"
  | "advanced"
  | "about";
export type { AdvancedTab };

const SECTIONS: { id: SettingsSection; label: string }[] = [
  { id: "accounts", label: "Accounts" },
  { id: "local", label: "Local models" },
  { id: "roles", label: "Roles" },
  { id: "code", label: "Code intelligence" },
  { id: "rules", label: "Rules & skills" },
  { id: "voice", label: "Voice" },
  { id: "permissions", label: "Permissions & network" },
  { id: "appearance", label: "Appearance" },
  { id: "remote", label: "Remote access" },
  { id: "advanced", label: "Advanced" },
  { id: "about", label: "About" },
];

/** Settings. Each page saves only its own configuration group. */
export type SettingsProps = {
  cfg: Record<string, unknown>;
  initialSection?: SettingsSection;
  initialAdvanced?: AdvancedTab;
  /** Accounts › this vendor gets focus (from a picker "Sign in" row). */
  focusVendor?: string;
  health: Health | null;
  sessionId: string;
  busy: boolean;
  onClose: () => void;
  onSave: (values: Record<string, unknown>) => Promise<void>;
  /** Accounts or local models changed: the picker reloads its rows. */
  onCatalogChanged: () => void;
  /** A free model finished downloading on the Local models page. */
  onModelDownloaded?: (model: DownloadModel) => void;
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
  onOpenProject?: (path: string) => void;
  onOpenSession: (id: string) => void;
  onSkillsChanged: () => Promise<void>;
  onUseSkill: (name: string) => void;
  /** Accounts › "When a plan runs out" (the same control as Allowance). */
  planLimit?: ReactNode;
  /** Roles › the open project and conversation. */
  roles?: RolesPageProps;
};

export function Settings({
  cfg,
  initialSection = "accounts",
  initialAdvanced = "skills",
  focusVendor,
  health,
  sessionId,
  busy,
  onClose,
  onSave,
  onCatalogChanged,
  onModelDownloaded,
  onToast,
  onOpenProject,
  onOpenSession,
  onSkillsChanged,
  onUseSkill,
  planLimit,
  roles,
}: SettingsProps) {
  const [section, setSection] = useState<SettingsSection>(initialSection);
  const [advanced, setAdvanced] = useState<AdvancedTab>(initialAdvanced);
  return (
    <Dialog label="Settings" className="modal settings" onClose={onClose}>
      <nav className="settings-nav" aria-label="Settings sections">
        <h2>Settings</h2>
        {SECTIONS.map((s) => (
          <button
            type="button"
            key={s.id}
            className={section === s.id ? "on" : ""}
            aria-current={section === s.id ? "page" : undefined}
            onClick={() => setSection(s.id)}
          >
            {s.label}
          </button>
        ))}
      </nav>
      <div className="settings-body">
        {section === "accounts" && (
          <AccountsPage
            focusVendor={focusVendor}
            offline={
              ((cfg.network || {}) as { mode?: string }).mode === "offline"
            }
            onChanged={onCatalogChanged}
            onToast={onToast}
            planLimit={planLimit}
          />
        )}
        {section === "local" && (
          <LocalModelsPage
            onChanged={onCatalogChanged}
            onDownloaded={onModelDownloaded}
            onToast={onToast}
          />
        )}
        {section === "roles" &&
          (roles ? (
            <RolesPage {...roles} onToast={onToast} />
          ) : (
            <section className="settings-page">
              <h3>Roles</h3>
              <p className="hint">Open a project to choose its roles.</p>
            </section>
          ))}
        {section === "code" && <CodeIntelPage onToast={onToast} />}
        {section === "rules" && <RulesPage onToast={onToast} />}
        {section === "voice" && <VoicePage onToast={onToast} />}
        {section === "permissions" && (
          <PermissionsPage cfg={cfg} onSave={onSave} />
        )}
        {section === "appearance" && (
          <AppearancePage cfg={cfg} onSave={onSave} />
        )}
        {section === "remote" && <RemotePage onToast={onToast} />}
        {section === "advanced" && (
          <AdvancedPage
            cfg={cfg}
            tab={advanced}
            onTab={setAdvanced}
            health={health}
            sessionId={sessionId}
            busy={busy}
            onSave={onSave}
            onToast={onToast}
            onOpenProject={onOpenProject}
            onOpenSession={(id) => {
              onClose();
              onOpenSession(id);
            }}
            onSkillsChanged={onSkillsChanged}
            onUseSkill={(name) => {
              onClose();
              onUseSkill(name);
            }}
          />
        )}
        {section === "about" && <AboutPage onSave={onSave} onToast={onToast} />}
        <div className="row end settings-close">
          <button type="button" className="ghost" onClick={onClose}>
            Close
          </button>
        </div>
      </div>
    </Dialog>
  );
}
