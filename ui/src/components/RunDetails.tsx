import type { RunRecord } from "../api";
import { runDetailRows } from "../lib/spending";
import "./TaskTimingDetails.css";

/** "Run details": exactly what ran the task, for bug reports and for
 * comparing runs. Identifiers and short hashes only. */
export function RunDetails({ run }: { run?: RunRecord }) {
  if (!run) return null;
  return (
    <details className="run-details">
      <summary>Run details</summary>
      <dl>
        {runDetailRows(run).map(([label, value]) => (
          <div key={label}>
            <dt>{label}</dt>
            <dd>
              <code>{value}</code>
            </dd>
          </div>
        ))}
      </dl>
      <p className="dim">
        The settings and rules values are short fingerprints: two runs with the
        same fingerprint used the same settings or the same rules.
      </p>
    </details>
  );
}
