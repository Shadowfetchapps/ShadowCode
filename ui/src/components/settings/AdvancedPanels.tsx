import { useCallback, useEffect, useRef, useState } from "react";
import {
  api,
  type BackgroundTask,
  type DoctorReport,
  type Goal,
  type Health,
  type ProjectSkill,
} from "../../api";
import { exportDiagnostics, isNative } from "../../lib/transport";
import { Empty, LoadError } from "../cards";
import { ConfirmDialog } from "../ConfirmDialog";
import { goalStatusLabel, processStatusLabel } from "../../lib/statusLabels";
import { SkillChecker } from "./SkillChecker";

/* Project tools: skills and health sit under Settings › Advanced; goals and
 * background processes are in the drawer's Tools tab (used while working). */

type Toast = (text: string, kind?: "ok" | "err" | "info") => void;

export { SkillsTab, GoalsTab, HealthTab, BackgroundTab };

// --- Skills / instructions ------------------------------------------------------

function SkillsTab({
  toast,
  onChanged,
  onUse,
  busy,
}: {
  toast: Toast;
  onChanged: () => Promise<void>;
  onUse: (name: string) => void;
  busy: boolean;
}) {
  const [instructions, setInstructions] = useState("");
  const [skills, setSkills] = useState<ProjectSkill[]>([]);
  const [issues, setIssues] = useState<string[]>([]);
  const [name, setName] = useState("workflow");
  const [body, setBody] = useState("");
  const [hash, setHash] = useState("missing");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");
  const load = useCallback(async () => {
    const [inst, sk] = await Promise.all([api.instructions(), api.skills()]);
    setInstructions(inst.content);
    setSkills(sk.skills);
    setIssues(sk.issues || []);
  }, []);
  useEffect(() => {
    void load().catch((error) => setError(String(error)));
  }, [load]);
  const save = async (skill: boolean) => {
    setSaving(true);
    setError("");
    try {
      if (skill) await api.saveSkill(name, body, isNative() ? hash : undefined);
      else await api.saveInstructions(instructions);
      await load();
      await onChanged();
      if (skill) {
        const saved = (await api.skills()).skills.find(
          (item) => item.path === `.shadow/skills/${name}.md`,
        );
        setHash(saved?.hash || "missing");
      }
      toast(skill ? `Saved skill ${name}` : "Instructions saved", "ok");
    } catch (error) {
      setError(String(error));
    } finally {
      setSaving(false);
    }
  };
  return (
    <>
      <p className="hint">
        Project instructions guide every task.{" "}
        {isNative()
          ? "Skills run when you select them or use /skill name in the composer."
          : "Save reusable project workflow files here."}
      </p>
      {error && (
        <p className="health-bad" role="alert">
          {error}
        </p>
      )}
      <div className="field">
        <label htmlFor="project-instructions">Project instructions</label>
        <p className="hint">
          <code>.shadow/instructions.md</code>
        </p>
        <textarea
          id="project-instructions"
          rows={7}
          value={instructions}
          onChange={(e) => setInstructions(e.target.value)}
          placeholder="How this project likes to be worked on…"
        />
        <button
          type="button"
          className="mini"
          disabled={saving || busy}
          onClick={() => void save(false)}
        >
          Save instructions
        </button>
      </div>
      <div className="field">
        <label htmlFor="skill-name">Skill name</label>
        <input
          id="skill-name"
          value={name}
          maxLength={80}
          onChange={(e) => {
            setName(e.target.value);
            setHash("missing");
          }}
          placeholder="skill name"
        />
        <label htmlFor="skill-body">Skill instructions</label>
        <textarea
          id="skill-body"
          rows={5}
          value={body}
          onChange={(e) => setBody(e.target.value)}
          placeholder="Describe the workflow. Use $ARGUMENTS for additional context."
        />
        <p className="hint">
          Saved to <code>.shadow/skills/{name || "name"}.md</code>.
        </p>
        <button
          type="button"
          className="mini"
          disabled={saving || busy || !name.trim() || !body.trim()}
          onClick={() => void save(true)}
        >
          Save skill
        </button>
      </div>
      {issues.map((issue, index) => (
        <p key={index} className="health-bad">
          {issue}
        </p>
      ))}
      <div className="list">
        {skills.map((skill) => (
          <details key={skill.path || skill.name}>
            <summary>
              {skill.name}
              {skill.mode ? ` · ${skill.mode}` : ""}
            </summary>
            {skill.description && <p className="hint">{skill.description}</p>}
            {skill.path && (
              <p className="hint">
                <code>{skill.path}</code>
              </p>
            )}
            <pre className="op-full">{skill.content}</pre>
            {isNative() && (
              <button
                type="button"
                className="mini"
                disabled={busy}
                onClick={() => onUse(skill.name)}
              >
                Use /{skill.name}
              </button>
            )}
            {(!skill.path ||
              /^\.shadow\/skills\/[^/]+\.md$/.test(skill.path)) && (
              <button
                type="button"
                className="mini"
                disabled={saving}
                onClick={() => {
                  setName(
                    skill.path?.split("/").at(-1)?.replace(/\.md$/, "") ||
                      skill.name,
                  );
                  setBody(skill.raw_content || skill.content);
                  setHash(skill.hash || "missing");
                }}
              >
                Edit {skill.name}
              </button>
            )}
          </details>
        ))}
      </div>
    </>
  );
}

// --- Goals: milestone checklist · resume · progress ----------------------------

function GoalsTab({
  sessionId,
  onOpen,
  toast,
}: {
  sessionId: string;
  onOpen: (id: string) => void;
  toast: Toast;
}) {
  const [goals, setGoals] = useState<Goal[]>([]);
  const [text, setText] = useState("");
  const [error, setError] = useState("");
  const [pending, setPending] = useState("");
  const [deleting, setDeleting] = useState<Goal | null>(null);
  const load = useCallback(async () => {
    try {
      setGoals((await api.goals()).goals);
      setError("");
    } catch (err) {
      setError(String(err));
    }
  }, []);
  useEffect(() => {
    void load();
    const id = setInterval(() => void load(), 3000);
    return () => clearInterval(id);
  }, [load]);

  async function create(run: boolean) {
    if (!text.trim() || pending) return;
    setPending("create");
    try {
      const goal = await api.createGoal(
        text.trim(),
        run,
        sessionId || undefined,
      );
      setText("");
      await load();
      if (run && goal.session_id) onOpen(goal.session_id);
      toast(run ? "Goal started" : "Goal created", "ok");
    } catch (err) {
      toast(String(err), "err");
    } finally {
      setPending("");
    }
  }
  async function action(id: string, run: () => Promise<unknown>) {
    if (pending) return;
    setPending(id);
    try {
      await run();
      await load();
    } catch (err) {
      toast(String(err), "err");
    } finally {
      setPending("");
    }
  }

  return (
    <>
      <div className="field">
        <textarea
          aria-label="Goal instruction"
          rows={2}
          value={text}
          onChange={(e) => setText(e.target.value)}
          placeholder="One line: what should be true when this goal is done?"
        />
        <div className="row">
          <button
            type="button"
            className="mini"
            disabled={Boolean(pending) || !text.trim()}
            onClick={() => void create(false)}
          >
            Plan
          </button>
          <button
            type="button"
            className="mini primary-mini"
            disabled={Boolean(pending) || !text.trim()}
            onClick={() => void create(true)}
          >
            Plan & run
          </button>
        </div>
      </div>
      {error && (
        <p className="notice bad" role="alert">
          {error}
        </p>
      )}
      {!error && goals.length === 0 && (
        <Empty
          title="No goals yet"
          body="Create a checklist, then run its milestones in order. Saved task results show what was done and checked."
        />
      )}
      {goals.map((g) => (
        <div key={g.id} className={`goal ${g.status}`}>
          <header>
            <strong>{g.title || g.instruction}</strong>
            <span className="goal-pct">{g.progress_pct}%</span>
          </header>
          <div
            className="bar"
            role="progressbar"
            aria-label={`Goal progress: ${g.title || g.instruction}`}
            aria-valuemin={0}
            aria-valuemax={100}
            aria-valuenow={g.progress_pct}
          >
            <i style={{ width: `${g.progress_pct}%` }} />
          </div>
          <ul className="milestones">
            {g.milestones.map((m) => (
              <li key={m.id} className={m.status}>
                <button
                  type="button"
                  className="tick"
                  title={
                    m.status === "done"
                      ? "Mark pending"
                      : "Mark complete manually"
                  }
                  aria-label={`${m.status === "done" ? "Mark pending" : "Mark complete manually"}: ${m.title}`}
                  disabled={g.running || Boolean(pending)}
                  onClick={() =>
                    void action(g.id, () =>
                      api.setMilestone(
                        g.id,
                        m.id,
                        m.status === "done" ? "pending" : "done",
                      ),
                    )
                  }
                >
                  {m.status === "done"
                    ? "✓"
                    : m.status === "in_progress"
                      ? "▸"
                      : m.status === "failed"
                        ? "✗"
                        : "○"}
                </button>
                <div>
                  <span>{m.title}</span>
                  {m.detail && (
                    <details>
                      <summary>Result</summary>
                      <p className="milestone-detail">{m.detail}</p>
                    </details>
                  )}
                </div>
              </li>
            ))}
          </ul>
          <div className="row goal-actions">
            <span className="dim goal-status">{goalStatusLabel(g)}</span>
            {!g.running && g.status !== "completed" && (
              <button
                type="button"
                className="mini"
                disabled={Boolean(pending)}
                onClick={() =>
                  void action(g.id, async () => {
                    const next = await api.runGoal(
                      g.id,
                      isNative() ? undefined : sessionId || undefined,
                    );
                    if (next.session_id) onOpen(next.session_id);
                  })
                }
              >
                {g.progress > 0 ? "Resume" : "Run"}
              </button>
            )}
            {g.running && isNative() && (
              <button
                type="button"
                className="mini"
                disabled={Boolean(pending)}
                onClick={() => void action(g.id, () => api.pauseGoal(g.id))}
              >
                Pause
              </button>
            )}
            {g.session_id && (
              <button
                type="button"
                className="mini"
                onClick={() => onOpen(g.session_id!)}
              >
                Open task
              </button>
            )}
            {!g.running && !["completed", "abandoned"].includes(g.status) && (
              <button
                type="button"
                className="mini"
                disabled={Boolean(pending)}
                onClick={() => void action(g.id, () => api.abandonGoal(g.id))}
              >
                Abandon
              </button>
            )}
            {!g.running && (
              <button
                type="button"
                className="mini danger-text"
                disabled={Boolean(pending)}
                onClick={() => setDeleting(g)}
              >
                Delete
              </button>
            )}
          </div>
          {g.run_detail && (
            <p className="dim milestone-detail">{g.run_detail}</p>
          )}
        </div>
      ))}
      {deleting && (
        <ConfirmDialog
          title="Delete this goal?"
          confirmLabel="Delete"
          danger
          onCancel={() => setDeleting(null)}
          onConfirm={async () => {
            const goal = deleting;
            await action(goal.id, () => api.deleteGoal(goal.id));
            setDeleting(null);
          }}
        >
          <p>
            “{deleting.title || deleting.instruction}” and its checklist are
            removed. Tasks it already ran stay in the task list.
          </p>
        </ConfirmDialog>
      )}
    </>
  );
}

// --- Health: installed tools and native diagnostics -------------------------

function HealthTab({ health }: { health: Health | null }) {
  const [report, setReport] = useState<DoctorReport | null>(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(false);
  const [previewOpen, setPreviewOpen] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState("");
  const [saved, setSaved] = useState(false);
  const requestNumber = useRef(0);
  const load = useCallback(async () => {
    const number = ++requestNumber.current;
    setLoading(true);
    setError("");
    setSaveError("");
    setSaved(false);
    setSaving(false);
    setPreviewOpen(false);
    setReport(null);
    try {
      const next = await api.doctor();
      if (requestNumber.current === number) setReport(next);
    } catch (e) {
      if (requestNumber.current === number) setError(String(e));
    } finally {
      if (requestNumber.current === number) setLoading(false);
    }
  }, []);
  useEffect(() => {
    void load();
    return () => {
      requestNumber.current += 1;
    };
  }, [load]);
  const snapshot = report?.diagnostic_export;
  const save = async () => {
    if (!snapshot || saving) return;
    const number = requestNumber.current;
    setSaving(true);
    setSaveError("");
    setSaved(false);
    try {
      const result = await exportDiagnostics(snapshot.id, snapshot.content);
      if (requestNumber.current === number && result) setSaved(true);
    } catch (e) {
      if (requestNumber.current === number) setSaveError(String(e));
    } finally {
      if (requestNumber.current === number) setSaving(false);
    }
  };
  return (
    <>
      <div className="kv">
        <div>
          <span>ShadowCode</span>
          <code>{health?.version || "…"}</code>
        </div>
        {Object.entries(health?.tools || {}).map(([name, info]) => (
          <div key={name}>
            <span>{name}</span>
            <code className={info.ok ? "health-ok" : "health-bad"}>
              {info.ok ? info.detail || "Found" : "Not found"}
            </code>
          </div>
        ))}
      </div>
      <h4>Diagnostics</h4>
      {error && <LoadError message={error} onRetry={load} />}
      {loading && !report && <p role="status">Running diagnostics…</p>}
      {snapshot && (
        <div className="diagnostic-export">
          <button
            type="button"
            className="mini"
            onClick={() => setPreviewOpen((open) => !open)}
          >
            {previewOpen ? "Hide export preview" : "Preview diagnostics export"}
          </button>
          {previewOpen && (
            <div>
              <p>
                Review these local Doctor check statuses before saving. The
                export excludes paths, project content, credentials, prompts and
                raw logs. It is not a full system or model qualification.
              </p>
              <pre aria-label="Diagnostics export preview">
                {snapshot.content}
              </pre>
              {saveError && (
                <p className="health-bad" role="alert">
                  {saveError}
                </p>
              )}
              {saved && <p role="status">Diagnostics saved.</p>}
              <button
                type="button"
                className="mini"
                disabled={saving || loading}
                onClick={() => void save()}
              >
                {saving ? "Saving…" : "Save diagnostics…"}
              </button>
            </div>
          )}
        </div>
      )}
      {report && report.checks.length === 0 && (
        <p className="dim">No checks reported for this project.</p>
      )}
      {report && report.checks.length > 0 && (
        <div className="diagnostic-list">
          {report.checks.map((c) => {
            const status = c.status || (c.ok ? "pass" : "fail");
            const label = {
              pass: "Passed",
              warn: "Warning",
              fail: "Failed",
              info: "Information",
              not_checked: "Not checked",
            }[status];
            return (
              <div className="diagnostic-check" key={c.id}>
                <strong>{c.label}</strong>
                <span
                  className={
                    status === "pass"
                      ? "health-ok"
                      : status === "fail"
                        ? "health-bad"
                        : "dim"
                  }
                >
                  {label}
                </span>
                <p>{c.detail}</p>
                {c.fix && <p className="dim">{c.fix}</p>}
              </div>
            );
          })}
        </div>
      )}
      <div className="row">
        <button
          type="button"
          className="mini"
          disabled={loading}
          onClick={() => void load()}
        >
          {loading ? "Checking…" : "Run again"}
        </button>
      </div>
      <SkillChecker />
    </>
  );
}

// --- Background processes -----------------------------------------------------

function BackgroundTab({ toast }: { toast: Toast }) {
  const [tasks, setTasks] = useState<BackgroundTask[]>([]);
  const [name, setName] = useState("dev");
  const [command, setCommand] = useState("");
  const [starting, setStarting] = useState(false);
  const [stopping, setStopping] = useState<string[]>([]);
  const [error, setError] = useState("");
  const [retained, setRetained] = useState<BackgroundTask | null>(null);
  const load = useCallback(async () => {
    try {
      setTasks((await api.background()).tasks);
      setError("");
    } catch (error) {
      setError(String(error));
    }
  }, []);
  useEffect(() => {
    void load();
    const id = setInterval(() => void load(), 1000);
    return () => clearInterval(id);
  }, [load]);
  const start = async () => {
    if (starting || !name.trim() || !command.trim()) return;
    setStarting(true);
    try {
      await api.startBackground(name.trim(), command);
      setCommand("");
      await load();
    } catch (error) {
      toast(String(error), "err");
    } finally {
      setStarting(false);
    }
  };
  const stop = async (id: string) => {
    setStopping((current) => [...current, id]);
    try {
      await api.stopBackground(id);
      await load();
    } catch (error) {
      toast(String(error), "err");
    } finally {
      setStopping((current) => current.filter((value) => value !== id));
    }
  };
  return (
    <>
      <p className="hint">
        Development servers and watchers continue between tasks.
        {isNative() &&
          " This panel shows the current project's processes. They stop when ShadowCode closes."}
      </p>
      <form
        className="background-form"
        onSubmit={(event) => {
          event.preventDefault();
          void start();
        }}
      >
        <label htmlFor="background-name">Process name</label>
        <input
          id="background-name"
          value={name}
          onChange={(e) => setName(e.target.value)}
          maxLength={80}
          disabled={starting}
        />
        <label htmlFor="background-command">Background command</label>
        <input
          id="background-command"
          value={command}
          onChange={(e) => setCommand(e.target.value)}
          placeholder="npm run dev"
          maxLength={64000}
          disabled={starting}
        />
        <button
          type="submit"
          className="mini"
          disabled={starting || !name.trim() || !command.trim()}
        >
          {starting ? "Starting…" : "Start process"}
        </button>
      </form>
      {error && (
        <p className="health-bad" role="alert">
          {error}
        </p>
      )}
      {!error && tasks.length === 0 && (
        <Empty
          title="No processes yet"
          body="Start a dev server or watcher above, or ask the agent to run one."
        />
      )}
      {tasks.map((t) => (
        <div key={t.id} className="bg-task">
          <header>
            <strong>{t.name}</strong>
            <span className={`dim st-${t.status.toLowerCase()}`}>
              {processStatusLabel(t.status)}
              {t.exit_code !== null ? ` · exit ${t.exit_code}` : ""}
            </span>
            {["STARTING", "RUNNING", "STOPPING"].includes(
              t.status.toUpperCase(),
            ) && (
              <button
                type="button"
                className="mini danger-text"
                aria-label={`Stop ${t.name}`}
                disabled={
                  stopping.includes(t.id) ||
                  t.status.toUpperCase() === "STOPPING"
                }
                onClick={() => void stop(t.id)}
              >
                {stopping.includes(t.id) ||
                t.status.toUpperCase() === "STOPPING"
                  ? "Stopping…"
                  : "Stop"}
              </button>
            )}
          </header>
          <code className="dim">
            {retained?.id === t.id ? retained.command : t.command}
          </code>
          {t.pid > 0 && (
            <p className="hint">
              PID {t.pid}
              {t.started_at
                ? ` · started ${new Date(t.started_at * 1000).toLocaleTimeString()}`
                : ""}
            </p>
          )}
          {t.error && <p className="health-bad">{t.error}</p>}
          {t.output && (
            <details
              className="background-output"
              open={
                ["STARTING", "RUNNING", "STOPPING"].includes(
                  t.status.toUpperCase(),
                ) || retained?.id === t.id
              }
            >
              <summary>
                {retained?.id === t.id
                  ? "Retained output snapshot"
                  : t.truncated || t.output_preview_truncated
                    ? "Recent output · older output omitted"
                    : "Process output"}
              </summary>
              <pre
                className="plan log"
                tabIndex={0}
                aria-label={`${t.name} process output`}
              >
                {retained?.id === t.id ? retained.output : t.output}
              </pre>
              {isNative() && (
                <div className="row">
                  <button
                    type="button"
                    className="mini"
                    onClick={() =>
                      void api
                        .backgroundTask(t.id)
                        .then(setRetained)
                        .catch((error) => toast(String(error), "err"))
                    }
                  >
                    {retained?.id === t.id
                      ? "Refresh retained output"
                      : "Read retained output"}
                  </button>
                  {retained?.id === t.id && (
                    <button
                      type="button"
                      className="mini"
                      onClick={() => setRetained(null)}
                    >
                      Return to live output
                    </button>
                  )}
                </div>
              )}
            </details>
          )}
        </div>
      ))}
    </>
  );
}
