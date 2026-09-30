/** Desktop notification choices, saved in the `ui` settings group
 * (`notify`, `notify_approval`, `notify_failed`, `notify_limit`,
 * `notify_finished`, `notify_sound`). */
export type NotifyPrefs = {
  notify: boolean;
  notify_approval: boolean;
  notify_failed: boolean;
  notify_limit: boolean;
  notify_finished: boolean;
  notify_sound: boolean;
};

const KINDS: [keyof NotifyPrefs, string][] = [
  [
    "notify_approval",
    "A task needs your approval (and 2 minutes before an unanswered approval is denied)",
  ],
  ["notify_failed", "A task failed"],
  [
    "notify_limit",
    "A plan or spending limit was reached (and whether the task continued on a local model)",
  ],
  ["notify_finished", "A task finished"],
];

export function notifyPrefs(ui: Record<string, unknown>): NotifyPrefs {
  const on = (key: string, fallback = true) =>
    typeof ui[key] === "boolean" ? (ui[key] as boolean) : fallback;
  return {
    notify: on("notify"),
    notify_approval: on("notify_approval"),
    notify_failed: on("notify_failed"),
    notify_limit: on("notify_limit"),
    notify_finished: on("notify_finished"),
    notify_sound: on("notify_sound", false),
  };
}

export function NotificationFields({
  value,
  onChange,
}: {
  value: NotifyPrefs;
  onChange: (next: NotifyPrefs) => void;
}) {
  const set = (key: keyof NotifyPrefs, on: boolean) =>
    onChange({ ...value, [key]: on });
  return (
    <fieldset className="notify-fields">
      <legend>Desktop notifications</legend>
      <label className="check">
        <input
          type="checkbox"
          checked={value.notify}
          onChange={(e) => set("notify", e.target.checked)}
        />{" "}
        Notify me when the window is in the background or the task is in another
        conversation
      </label>
      <div className="notify-kinds" aria-disabled={!value.notify}>
        {KINDS.map(([key, label]) => (
          <label className="check" key={key}>
            <input
              type="checkbox"
              checked={value[key]}
              disabled={!value.notify}
              onChange={(e) => set(key, e.target.checked)}
            />{" "}
            {label}
          </label>
        ))}
        <label className="check">
          <input
            type="checkbox"
            checked={value.notify_sound}
            disabled={!value.notify}
            onChange={(e) => set("notify_sound", e.target.checked)}
          />{" "}
          Play a sound
        </label>
      </div>
      <p className="hint">Clicking a notification opens its conversation.</p>
    </fieldset>
  );
}
