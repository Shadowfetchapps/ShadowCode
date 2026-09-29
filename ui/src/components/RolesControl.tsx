import { useId } from "react";
import { Users } from "lucide-react";
import type { RolesView } from "../api";
import type { TaskMode } from "../lib/effort";
import { pipelineLine, rolesAsking, rolesBlocked } from "../lib/roles";

/** The composer's compact roles control (in More): turn Plan → Implement →
 * Review on or off for this project, use a preset, or open Settings ›
 * Roles. */
export function RolesControl({
  view,
  saving,
  mode,
  onToggle,
  onPreset,
  onOpenSettings,
}: {
  view: RolesView | null;
  saving: boolean;
  mode: TaskMode;
  onToggle: (on: boolean) => void;
  /** Use a preset (and turn Plan → Implement → Review on). */
  onPreset: (id: string) => void;
  onOpenSettings: () => void;
}) {
  const id = useId();
  if (!view) return null;
  const on = view.setup.pipeline;
  const blocked = rolesBlocked(view);
  const asking = rolesAsking(view);
  return (
    <div className="roles-control" role="group" aria-label="Roles">
      <label className="roles-toggle" htmlFor={id}>
        <Users size={14} aria-hidden="true" />
        <span>Plan → Implement → Review</span>
        <input
          id={id}
          type="checkbox"
          role="switch"
          checked={on}
          disabled={saving}
          onChange={(event) => onToggle(event.target.checked)}
        />
      </label>
      <p className="roles-note">
        {on
          ? pipelineLine(view)
          : "Off: each message runs on the model you picked."}
      </p>
      {on && mode === "ask" && (
        <p className="roles-note">
          Roles run Code and Plan messages. Ask answers on the model you picked.
        </p>
      )}
      {on && blocked && <p className="roles-note warn-text">{blocked}</p>}
      {on && !blocked && asking.length > 0 && (
        <p className="roles-note">
          {asking.join(" and ")} {asking.length === 1 ? "runs" : "run"} in the
          cloud. ShadowCode asks before this conversation&apos;s work leaves
          this computer.
        </p>
      )}
      <div className="roles-presets-compact" role="group" aria-label="Presets">
        {view.presets.map((preset) => {
          const current = on && view.setup.preset === preset.id;
          return (
            <button
              type="button"
              key={preset.id}
              className={`roles-chip ${current ? "on" : ""}`}
              aria-pressed={current}
              disabled={saving}
              title={preset.description}
              onClick={() => onPreset(preset.id)}
            >
              {preset.label}
            </button>
          );
        })}
      </div>
      <button type="button" className="roles-edit" onClick={onOpenSettings}>
        Choose a model for each role…
      </button>
    </div>
  );
}
