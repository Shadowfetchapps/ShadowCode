import type { ComponentProps, RefObject } from "react";
import type { LimitsConfig, Project } from "../../api";
import { AllowancePanel, PlanLimitControl } from "../Allowance";
import { CompareDialog } from "../CompareDialog";
import { ConsentDialog } from "../ConsentDialog";
import {
  Help,
  Palette,
  ProjectPicker,
  TrustDialog,
  type PaletteItem,
} from "../overlays";
import { Settings, type AdvancedTab, type SettingsSection } from "../Settings";
import { readyLocalTargets } from "../../lib/allowance";
import { billingWarning, type PickerTarget } from "../../lib/picker";
import type { useAllowance } from "../../hooks/useCatalog";
import type { Consent } from "../../hooks/useTaskActions";

export type Overlay =
  "" | "settings" | "help" | "palette" | "project" | "allowance" | "compare";

type CompareProps = ComponentProps<typeof CompareDialog>;

/** Every modal the window can show: the open overlay (Settings, Compare,
 * Allowance, shortcuts, palette, project picker), the trust prompt and the
 * cloud consent dialog. */
export function AppDialogs({
  overlay,
  setOverlay,
  trust,
  onCancelTrust,
  onConfirmTrust,
  consent,
  targets,
  onCancelConsent,
  onSendConsent,
  settings,
  limits,
  allowance,
  onSaveLimits,
  onOpenSettings,
  onSetup,
  compare,
  allowanceReturn,
  version,
  palette,
  projects,
  workspace,
  onPickProject,
}: {
  overlay: Overlay;
  setOverlay: (overlay: Overlay) => void;
  trust: ComponentProps<typeof TrustDialog>["req"] | null;
  onCancelTrust: () => void;
  onConfirmTrust: () => void;
  consent: Consent | null;
  targets: PickerTarget[];
  onCancelConsent: () => void;
  onSendConsent: () => void;
  settings: Omit<ComponentProps<typeof Settings>, "planLimit">;
  limits: LimitsConfig;
  allowance: ReturnType<typeof useAllowance>;
  onSaveLimits: (limits: LimitsConfig) => Promise<void>;
  onOpenSettings: (
    section?: SettingsSection,
    extra?: { advanced?: AdvancedTab; vendor?: string },
  ) => void;
  onSetup: (target: PickerTarget) => void;
  compare: Omit<
    CompareProps,
    "targets" | "onOpenAllowance" | "onConnect" | "onSetup" | "onAddLocal"
  >;
  /** The Allowance panel was opened from Compare and returns to it. */
  allowanceReturn: RefObject<boolean>;
  version: string;
  palette: PaletteItem[];
  projects: Project[];
  workspace: string;
  onPickProject: (path: string) => void;
}) {
  const close = () => setOverlay("");
  const openLocal = () => onOpenSettings("local");
  const planLimit = (
    <PlanLimitControl
      limits={limits}
      localTargets={readyLocalTargets(targets)}
      automaticName={
        allowance.data?.rows.find((row) => row.id === "local")?.fallback?.name
      }
      onSave={onSaveLimits}
      onOpenLocal={openLocal}
    />
  );
  return (
    <>
      {trust && (
        <TrustDialog
          req={trust}
          onCancel={onCancelTrust}
          onConfirm={onConfirmTrust}
        />
      )}
      {consent && (
        <ConsentDialog
          request={consent.request}
          destination={targets.find((t) => t.id === consent.body.model)?.name}
          billingWarning={billingWarning(
            targets.find((t) => t.id === consent.body.model),
          )}
          attachments={consent.original.attachments.map((a) => a.name)}
          onCancel={onCancelConsent}
          onSend={onSendConsent}
        />
      )}
      {overlay === "settings" && (
        <Settings {...settings} planLimit={planLimit} />
      )}
      {overlay === "compare" && (
        <CompareDialog
          {...compare}
          targets={targets}
          onOpenAllowance={() => {
            allowanceReturn.current = true;
            setOverlay("allowance");
            void allowance.reload();
          }}
          onConnect={(vendor) => onOpenSettings("accounts", { vendor })}
          onSetup={onSetup}
          onAddLocal={openLocal}
        />
      )}
      {overlay === "allowance" && (
        <AllowancePanel
          data={allowance.data}
          loading={allowance.loading}
          error={allowance.error}
          planLimit={planLimit}
          onRefresh={() => void allowance.reload(true)}
          onClose={() => {
            // Opened from Compare: return to the lineup.
            setOverlay(allowanceReturn.current ? "compare" : "");
            allowanceReturn.current = false;
          }}
          onOpenAccounts={(vendor) => onOpenSettings("accounts", { vendor })}
          onOpenLocal={openLocal}
        />
      )}
      {overlay === "help" && <Help onClose={close} version={version} />}
      {overlay === "palette" && <Palette items={palette} onClose={close} />}
      {overlay === "project" && (
        <ProjectPicker
          projects={projects}
          current={workspace}
          onClose={close}
          onPick={onPickProject}
        />
      )}
    </>
  );
}
