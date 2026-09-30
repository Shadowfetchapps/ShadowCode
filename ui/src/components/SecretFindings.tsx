import type { SecretFinding } from "../api";

/** Findings of the secret check before a commit or push: where each one is
 * and what it looks like, never the value. `actions` renders the per-file
 * buttons (commit only); `unchecked` says what the check could not read. */
export function SecretFindings({
  findings,
  truncated,
  unchecked,
  actions,
}: {
  findings: SecretFinding[];
  truncated?: boolean;
  unchecked?: string | null;
  actions?: (path: string) => React.ReactNode;
}) {
  const paths = [...new Set(findings.map((f) => f.path))];
  return (
    <div className="secret-findings">
      <ul aria-label="Possible secrets">
        {paths.map((path) => (
          <li key={path}>
            <div className="secret-file">
              <code>{path}</code>
              {actions && <span className="row">{actions(path)}</span>}
            </div>
            <ul>
              {findings
                .filter((f) => f.path === path)
                .map((f, i) => (
                  <li key={i}>
                    {f.line ? `Line ${f.line}: ` : ""}
                    looks like {f.kind} ({f.preview})
                    {f.commit ? ` in commit ${f.commit.slice(0, 8)}` : ""}
                  </li>
                ))}
            </ul>
          </li>
        ))}
      </ul>
      {truncated && <p className="hint">Only the first findings are listed.</p>}
      {unchecked && <p className="hint">{unchecked}</p>}
      <p className="hint">
        If a value is a harmless example, add{" "}
        <code>shadowcode:allow-secret</code> on its line, or list the file in{" "}
        <code>.shadowcode/secret-scan-ignore</code>.
      </p>
    </div>
  );
}
