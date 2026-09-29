import { useState } from "react";
import { forgeApi } from "../lib/forge";

/** Where a new worktree task starts: the project's current files (with
 * uncommitted work) or another branch. Branches load on first use. */
export function WorktreeBaseSelect({
  value,
  onChange,
}: {
  value: string;
  onChange: (branch: string) => void;
}) {
  const [branches, setBranches] = useState<string[] | null>(null);
  const load = () => {
    if (branches !== null) return;
    setBranches([]);
    void forgeApi
      .overview()
      .then((overview) =>
        setBranches(
          (overview.branches || [])
            .filter((b) => !b.current)
            .map((b) => b.name),
        ),
      )
      .catch(() => setBranches([]));
  };
  return (
    <label className="worktree-base">
      <span>New worktree starts from</span>
      <select
        value={value}
        onFocus={load}
        onPointerDown={load}
        onChange={(e) => onChange(e.target.value)}
      >
        <option value="">The current files, as they are now</option>
        {value && !(branches || []).includes(value) && (
          <option value={value}>{value}</option>
        )}
        {(branches || []).map((name) => (
          <option key={name} value={name}>
            The branch {name}
          </option>
        ))}
      </select>
    </label>
  );
}
