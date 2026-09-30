import { useEffect, useRef, useState } from "react";
import { forgeApi } from "../lib/forge";

/** Where a new worktree task starts: the project's current files (with
 * uncommitted work) or another branch. Branches load each time the menu
 * around it opens, and on focus. */
export function WorktreeBaseSelect({
  value,
  onChange,
}: {
  value: string;
  onChange: (branch: string) => void;
}) {
  const [branches, setBranches] = useState<string[]>([]);
  const root = useRef<HTMLLabelElement>(null);
  // The last list stays while it refreshes.
  const load = () =>
    void forgeApi
      .overview()
      .then((overview) =>
        setBranches(
          (overview.branches || [])
            .filter((b) => !b.current)
            .map((b) => b.name),
        ),
      )
      .catch(() => undefined);
  useEffect(() => {
    const menu = root.current?.closest("details");
    if (!menu) {
      load();
      return;
    }
    const opened = () => {
      if (menu.open) load();
    };
    opened();
    menu.addEventListener("toggle", opened);
    return () => menu.removeEventListener("toggle", opened);
  }, []);
  return (
    <label className="worktree-base" ref={root}>
      <span>New worktree starts from</span>
      <select
        value={value}
        onFocus={load}
        onChange={(e) => onChange(e.target.value)}
      >
        <option value="">The current files, as they are now</option>
        {value && !branches.includes(value) && (
          <option value={value}>{value}</option>
        )}
        {branches.map((name) => (
          <option key={name} value={name}>
            The branch {name}
          </option>
        ))}
      </select>
    </label>
  );
}
