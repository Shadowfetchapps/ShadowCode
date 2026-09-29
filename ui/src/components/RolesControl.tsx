import { useId } from "react";
import { Users } from "lucide-react";
import type { RolesView } from "../api";
import type { TaskMode } from "../lib/effort";
import { pipelineLine, rolesAsking, rolesBlocked } from "../lib/roles";

/** The composer's compact roles control (in More): turn Plan → Implement →
 * Review on or off for this project, pick a preset, or open Settings ›
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
  onPreset: (id: string) => void;
  onOpenSettings: () => void;
}) {
  const id = useId();
  if (!view) return null;
  const on = view.setup.pipeline;
  const blocked = rolesBlocked(view);
  const asking = rolesAsking(view);
  const preset = view.presets.find((p) => p.id === view.setup.preset);
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
      <p className="composer-more-reason">
        {on
          ? pipelineLine(view)
          : "Off: each message runs on the model you picked."}
      </p>
      {on && mode === "ask" && (
        <p className="composer-more-reason">
          Roles run Code and Plan messages. Ask answers on the model you picked.
        </p>
      )}
      {on && blocked && (
        <p className="composer-more-reason warn-text">{blocked}</p>
      )}
      {on && !blocked && asking.length > 0 && (
        <p className="composer-more-reason">
          {asking.join(" and ")} {asking.length === 1 ? "runs" : "run"} in the
          cloud. ShadowCode asks before this conversation&apos;s work leaves
          this computer.
        </p>
      )}
      <label className="composer-chip roles-preset">
        <span className="sr-only">Roles preset</span>
        <select
          value={preset ? preset.id : ""}
          disabled={saving}
          onChange={(event) => {
            if (event.target.value) onPreset(event.target.value);
          }}
        >
          <option value="">
            {preset ? "Choose a preset…" : "Your own roles"}
          </option>
          {view.presets.map((p) => (
            <option key={p.id} value={p.id} title={p.description}>
              {p.label}
            </option>
          ))}
        </select>
      </label>
      <button type="button" className="roles-edit" onClick={onOpenSettings}>
        Choose a model for each role…
      </button>
    </div>
  );
}
