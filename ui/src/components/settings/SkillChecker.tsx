import { useCallback, useEffect, useRef, useState } from "react";
import { api, type SkillCheck } from "../../api";
import { LoadError } from "../cards";
import "./RulesPage.css";

const SEVERITY: Record<string, string> = {
  error: "Error",
  warning: "Warning",
  info: "Note",
};

/** Health › Skill checker: problems in profile and project rules, skills,
 * commands and agent files. It only reports; nothing is changed. */
export function SkillChecker() {
  const [report, setReport] = useState<SkillCheck | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const request = useRef(0);
  const load = useCallback(async () => {
    const number = ++request.current;
    setLoading(true);
    setError("");
    try {
      const next = await api.skillCheck();
      if (request.current === number) setReport(next);
    } catch (e) {
      if (request.current === number) setError(String(e));
    } finally {
      if (request.current === number) setLoading(false);
    }
  }, []);
  useEffect(() => {
    void load();
    return () => {
      request.current += 1;
    };
  }, [load]);
  return (
    <section aria-labelledby="skill-checker-title">
      <h4 id="skill-checker-title">Skill checker</h4>
      <p className="hint">
        Checks your profile and this project&apos;s rules, skills, commands and
        agents. It only reports; nothing is changed. The same check runs with{" "}
        <code>shadowcode rules check</code>.
      </p>
      {error && <LoadError message={error} onRetry={load} />}
      {loading && !report && <p role="status">Checking skills…</p>}
      {report && (
        <>
          <p role="status">
            {report.checked.toLocaleString()} files checked ·{" "}
            {report.errors.toLocaleString()} errors ·{" "}
            {report.warnings.toLocaleString()} warnings ·{" "}
            {report.infos.toLocaleString()} notes
          </p>
          {report.findings.length === 0 ? (
            <p className="health-ok">No problems found.</p>
          ) : (
            <ul className="skill-findings" aria-label="Skill checker findings">
              {report.findings.map((finding, index) => (
                <li key={`${finding.code}:${finding.path}:${index}`}>
                  <strong
                    className={
                      finding.severity === "error" ? "health-bad" : undefined
                    }
                  >
                    {SEVERITY[finding.severity] ?? finding.severity}
                  </strong>{" "}
                  {finding.path && <code>{finding.path}</code>}
                  <p className="hint">{finding.message}</p>
                  {finding.fix && <p className="hint">{finding.fix}</p>}
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      <div className="row">
        <button
          type="button"
          className="mini"
          disabled={loading}
          onClick={() => void load()}
        >
          {loading ? "Checking…" : "Check again"}
        </button>
      </div>
    </section>
  );
}
