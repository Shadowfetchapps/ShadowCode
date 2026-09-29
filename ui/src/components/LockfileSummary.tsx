import type { LockfileChange } from "../api";

/** What a lockfile change does, in names instead of thousands of lines. */
export function LockfileSummary({
  change,
  showDiff,
  onToggleDiff,
}: {
  change: LockfileChange;
  showDiff: boolean;
  onToggleDiff: () => void;
}) {
  const groups: [string, { name: string; from?: string; to?: string }[]][] = [
    ["Added", change.added],
    ["Updated", change.updated],
    ["Removed", change.removed],
  ];
  return (
    <section className="lockfile-summary" aria-label="Dependency changes">
      <p>
        <strong>Dependencies:</strong> {change.summary}
      </p>
      {groups
        .filter(([, list]) => list.length > 0)
        .map(([label, list]) => (
          <details key={label} open={list.length <= 8}>
            <summary>
              {label} ({list.length})
            </summary>
            <ul>
              {list.slice(0, 200).map((item) => (
                <li key={item.name}>
                  <code>{item.name}</code>{" "}
                  {item.from && item.to
                    ? `${item.from} → ${item.to}`
                    : item.to || item.from}
                </li>
              ))}
            </ul>
            {list.length > 200 && (
              <p className="hint">and {list.length - 200} more</p>
            )}
          </details>
        ))}
      <button
        type="button"
        className="link"
        aria-expanded={showDiff}
        onClick={onToggleDiff}
      >
        {showDiff ? "Hide the full diff" : "Show the full diff"}
      </button>
    </section>
  );
}
