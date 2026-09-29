import { Users } from "lucide-react";
import {
  ROLE_LABELS,
  roleCost,
  stageStatus,
  verdictLabel,
  type RolesSummary,
} from "../lib/roles";

/** Who did what in a Plan → Implement → Review task, on its summary card:
 * each role's model, status, changes and cost. */
export function RoleSummary({ roles }: { roles: RolesSummary }) {
  if (!roles.stages.length) return null;
  return (
    <div className="task-summary-block role-summary">
      <h4>
        <Users size={13} aria-hidden="true" /> Roles
      </h4>
      <ul aria-label="Roles">
        {roles.stages.map((stage) => {
          const status = stageStatus(stage);
          const verdict = verdictLabel(stage.verdict);
          const bad =
            status !== "Done" && status !== "Skipped"
              ? true
              : stage.verdict === "needs_changes";
          return (
            <li key={stage.role} className={bad ? "bad" : ""}>
              <strong>{ROLE_LABELS[stage.role]}</strong>{" "}
              <span>{stage.name}</span>{" "}
              <span className="dim">
                {[
                  verdict || status,
                  stage.role === "implement" && stage.files
                    ? `${stage.files} file${stage.files === 1 ? "" : "s"} (+${stage.additions} −${stage.deletions})`
                    : "",
                  stage.skipped ? stage.skipped : "",
                  stage.skipped ? "" : roleCost(stage.cost, stage.usage),
                ]
                  .filter(Boolean)
                  .join(" · ")}
              </span>
              {stage.error && status !== "Done" && (
                <p className="dim role-error">{stage.error}</p>
              )}
            </li>
          );
        })}
      </ul>
      {roles.note && <p className="dim">{roles.note}</p>}
    </div>
  );
}
