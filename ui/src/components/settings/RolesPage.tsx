import { useId } from "react";
import type { RoleId, RolesChange, RoleTarget } from "../../api";
import { useRoles } from "../../hooks/useRoles";
import type { ToastKind } from "../../hooks/useToasts";
import { isApiKey, isLocal, type PickerTarget } from "../../lib/picker";
import {
  ROLE_HINTS,
  ROLE_LABELS,
  ROLE_ORDER,
  SKIP,
  roleCost,
  roleModel,
} from "../../lib/roles";

/** What Settings › Roles needs from the window. */
export type RolesPageProps = {
  workspace: string;
  sessionId: string;
  /** The composer's model, which roles set to "the conversation's model"
   * use. */
  model: string;
  /** The model picker's rows, offered for each role. */
  targets: PickerTarget[];
  /** The roles changed: the composer reloads its control. */
  onChanged: () => void;
};

const GROUPS: { label: string; test: (t: PickerTarget) => boolean }[] = [
  {
    label: "Subscriptions",
    test: (t) => !isLocal(t) && !isApiKey(t) && t.id.startsWith("cli:"),
  },
  { label: "API keys", test: (t) => isApiKey(t) },
  { label: "This computer", test: (t) => isLocal(t) },
];

/** Settings › Roles: a model or vendor CLI per role for this project, and
 * whether Code tasks run as Plan → Implement → Review. */
export function RolesPage({
  workspace,
  sessionId,
  model,
  targets,
  onChanged,
  onToast,
}: RolesPageProps & {
  onToast: (text: string, kind: ToastKind) => void;
}) {
  const roles = useRoles({
    workspace,
    sessionId,
    model,
    toast: (text, kind = "err") => onToast(text, kind),
  });
  const toggle = useId();
  const view = roles.view;
  const save = async (change: RolesChange) => {
    const next = await roles.save(change);
    if (next) onChanged();
  };
  if (!workspace)
    return (
      <section className="settings-page roles-page">
        <h3>Roles</h3>
        <p className="hint">Open a project to choose its roles.</p>
      </section>
    );
  if (!view)
    return (
      <section className="settings-page roles-page">
        <h3>Roles</h3>
        <p className="hint" role="status">
          Loading this project&apos;s roles…
        </p>
      </section>
    );
  const project = workspace.split("/").filter(Boolean).pop() || workspace;
  return (
    <section className="settings-page roles-page">
      <h3>Roles</h3>
      <p className="hint">
        Choose which model plans, implements, reviews and explores in{" "}
        <strong>{project}</strong>. A role can be a subscription such as Claude
        Code or Codex, an API model, or a model on this computer. Subagents the
        agent starts use their role&apos;s model too.
      </p>
      <label className="roles-toggle roles-toggle-large" htmlFor={toggle}>
        <span>
          <strong>Run Code tasks as Plan → Implement → Review</strong>
          <small>
            The plan role writes a plan, the implement role makes the changes in
            its own copy of the project, the review role checks them, and then
            you approve the changes as usual. Plan tasks run the plan role only.
          </small>
        </span>
        <input
          id={toggle}
          type="checkbox"
          role="switch"
          checked={view.setup.pipeline}
          disabled={roles.saving}
          onChange={(event) => void save({ pipeline: event.target.checked })}
        />
      </label>
      <h4>Presets</h4>
      <div className="roles-presets">
        {view.presets.map((preset) => (
          <button
            type="button"
            key={preset.id}
            className={`roles-preset-option ${view.setup.preset === preset.id ? "on" : ""}`}
            aria-pressed={view.setup.preset === preset.id}
            disabled={roles.saving}
            onClick={() => void save({ preset: preset.id })}
          >
            <strong>{preset.label}</strong>
            <small>{preset.description}</small>
          </button>
        ))}
      </div>
      <h4>Each role</h4>
      <div className="roles-table">
        {ROLE_ORDER.map((role) => (
          <RoleRow
            key={role}
            role={role}
            target={view.roles[role]}
            conversation={view.conversation.name}
            targets={targets}
            disabled={roles.saving}
            onChange={(value) => void save({ [role]: value })}
          />
        ))}
      </div>
      <p className="hint">
        Roles run one after another, so each can use its own model on this
        computer. When this conversation runs on this computer, ShadowCode asks
        before a cloud role receives its work; offline, cloud roles do not run.
        Every role&apos;s approvals appear in the conversation with the
        role&apos;s name, and its tokens and cost count toward the task.
      </p>
    </section>
  );
}

function RoleRow({
  role,
  target,
  conversation,
  targets,
  disabled,
  onChange,
}: {
  role: RoleId;
  target: RoleTarget | undefined;
  conversation: string;
  targets: PickerTarget[];
  disabled: boolean;
  onChange: (value: string) => void;
}) {
  const id = useId();
  const hint = `${id}-hint`;
  const setting = target?.skipped ? SKIP : target?.setting || "";
  const known =
    setting === "" || setting === SKIP || targets.some((t) => t.id === setting);
  const skippable = role === "plan" || role === "review";
  return (
    <div className="roles-row">
      <label htmlFor={id}>
        <strong>{ROLE_LABELS[role]}</strong>
        <small id={hint}>{ROLE_HINTS[role]}</small>
      </label>
      <div className="roles-row-choice">
        <select
          id={id}
          value={setting}
          disabled={disabled}
          aria-describedby={hint}
          onChange={(event) => onChange(event.target.value)}
        >
          <option value="">Conversation model ({conversation})</option>
          {skippable && <option value={SKIP}>Skip this step</option>}
          {!known && <option value={setting}>{target?.name || setting}</option>}
          {GROUPS.map((group) => {
            const rows = targets.filter(group.test);
            if (!rows.length) return null;
            return (
              <optgroup key={group.label} label={group.label}>
                {rows.map((row) => (
                  <option key={row.id} value={row.id}>
                    {row.availability === "ready"
                      ? row.name
                      : `${row.name} (${row.availability_label || "not ready"})`}
                  </option>
                ))}
              </optgroup>
            );
          })}
        </select>
        {target && !target.skipped && (
          <p className="roles-row-meta">
            {[
              roleModel(target),
              target.runner === "vendor" ? "vendor CLI" : "",
              target.local ? "this computer" : "cloud",
              roleCost(target.cost),
            ]
              .filter(Boolean)
              .join(" · ")}
          </p>
        )}
        {target?.blocked && (
          <p className="roles-row-meta warn-text">{target.blocked}</p>
        )}
        {!target?.blocked && target?.needs_consent && (
          <p className="roles-row-meta">
            Runs in the cloud: ShadowCode asks before this conversation&apos;s
            work goes to it.
          </p>
        )}
      </div>
    </div>
  );
}
