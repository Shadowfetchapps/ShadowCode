import { useCallback, useEffect, useMemo, useState } from "react";
import {
  api,
  type RuleItem,
  type RulesExportTarget,
  type RulesOverview,
  type RulesPreview,
} from "../../api";
import { LoadError } from "../cards";
import { ConfirmDialog } from "../ConfirmDialog";
import { InventoryItems } from "../ContextInventory";
import {
  canPickFiles,
  openRulesFolder,
  transportKind,
} from "../../lib/transport";
import "./RulesPage.css";

type Toast = (text: string, kind: "ok" | "err" | "info") => void;

const KIND_LABEL: Record<string, string> = {
  rules: "Rules",
  skill: "Skill",
  command: "Command",
  agent: "Agent",
};

/** Where an item came from, in words. */
export function sourceLabel(item: Pick<RuleItem, "scope" | "source">) {
  if (item.source.startsWith("import:"))
    return `Imported: ${item.source.slice("import:".length)}`;
  return item.scope === "project" ? "This project" : "Your profile";
}

/** Settings › Rules & skills: one rulebook for every agent. The user's
 * profile (rules, skills, commands, agents) and the project's own files,
 * with switches, the profile AGENTS.md editor, what each agent reads, Git
 * imports, starter skills and the optional export to other CLIs. */
export function RulesPage({ onToast }: { onToast: Toast }) {
  const [overview, setOverview] = useState<RulesOverview | null>(null);
  const [error, setError] = useState("");
  const [draft, setDraft] = useState("");
  const [baseHash, setBaseHash] = useState("missing");
  const [pending, setPending] = useState("");
  const [url, setUrl] = useState("");
  const [removing, setRemoving] = useState<string | null>(null);
  const [chosen, setChosen] = useState<string[]>([]);
  // Bumped after every change so "What each agent reads" reloads.
  const [version, setVersion] = useState(0);

  const load = useCallback(async (resetDraft = false) => {
    try {
      const next = await api.rules();
      setOverview(next);
      setError("");
      if (resetDraft) {
        setDraft(next.profile.agents_md.content);
        setBaseHash(next.profile.agents_md.hash);
      }
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void load(true);
  }, [load]);

  async function run(key: string, action: () => Promise<unknown>, done = "") {
    setPending(key);
    try {
      await action();
      if (done) onToast(done, "ok");
      return true;
    } catch (e) {
      onToast(String(e), "err");
      return false;
    } finally {
      setPending("");
      await load();
      setVersion((v) => v + 1);
    }
  }

  if (!overview) {
    return (
      <section className="settings-page">
        <h3>Rules &amp; skills</h3>
        {error ? (
          <LoadError message={error} onRetry={() => load(true)} />
        ) : (
          <p role="status">Reading your rules and skills…</p>
        )}
      </section>
    );
  }

  const saved = overview.profile.agents_md.content;
  const dirty = draft !== saved;
  const limit = overview.limits.profile_file_bytes;
  const size = new TextEncoder().encode(draft).length;
  const profileItems = overview.items.filter((i) => i.scope === "profile");
  const projectItems = overview.items.filter((i) => i.scope === "project");
  const starters = overview.starters.filter((s) => !s.installed);

  return (
    <section className="settings-page rules-page">
      <h3>Rules &amp; skills</h3>
      <p className="hint">
        One rulebook for every agent. Your profile applies to every project; a
        project&apos;s own files apply there, and when both name the same skill,
        command or agent, the project&apos;s wins. Rules and skills shape how
        agents work. They never grant permissions: approvals and the sandbox
        stay as they are.
      </p>
      <div className="kv">
        <div>
          <span>Profile folder</span>
          <code title={overview.profile.path}>{overview.profile.path}</code>
        </div>
        {overview.workspace && (
          <div>
            <span>Project</span>
            <code title={overview.workspace}>{overview.workspace}</code>
          </div>
        )}
      </div>
      <div className="row">
        {canPickFiles() && (
          <button
            type="button"
            className="mini"
            disabled={Boolean(pending)}
            onClick={() =>
              void run("folder", async () => {
                await openRulesFolder();
              })
            }
          >
            Open folder
          </button>
        )}
        <button
          type="button"
          className="mini ghost"
          disabled={Boolean(pending)}
          onClick={() => void load()}
        >
          Refresh
        </button>
      </div>

      <h4>Your rules</h4>
      <div className="field">
        <label htmlFor="profile-agents-md">
          AGENTS.md in your profile, sent to every agent
        </label>
        <textarea
          id="profile-agents-md"
          rows={8}
          value={draft}
          spellCheck={false}
          onChange={(e) => setDraft(e.target.value)}
          placeholder="How you like agents to work, in every project…"
        />
        <p className={size > limit ? "health-bad" : "hint"}>
          {size.toLocaleString()} bytes. Agents read the first{" "}
          {limit.toLocaleString()} bytes of this file.
        </p>
        <div className="row">
          <button
            type="button"
            className="mini"
            disabled={!dirty || Boolean(pending)}
            onClick={() =>
              void run(
                "save",
                async () => {
                  const result = await api.saveProfileRules(draft, baseHash);
                  setBaseHash(result.hash);
                  setOverview((current) =>
                    current
                      ? {
                          ...current,
                          profile: {
                            ...current.profile,
                            agents_md: {
                              ...current.profile.agents_md,
                              content: draft,
                              hash: result.hash,
                            },
                          },
                        }
                      : current,
                  );
                },
                "Rules saved",
              )
            }
          >
            {pending === "save" ? "Saving…" : "Save rules"}
          </button>
          {dirty && (
            <button
              type="button"
              className="mini ghost"
              disabled={Boolean(pending)}
              onClick={() => {
                setDraft(saved);
                setBaseHash(overview.profile.agents_md.hash);
              }}
            >
              Discard changes
            </button>
          )}
        </div>
      </div>

      <label className="check">
        <input
          type="checkbox"
          checked={overview.share_with_cli_agents}
          disabled={Boolean(pending)}
          onChange={(e) =>
            void run("sharing", () => api.setRulesSharing(e.target.checked))
          }
        />{" "}
        Send rules and skills to Claude Code, Codex, Cursor, Grok and
        Antigravity
      </label>
      <p className="hint">
        Each gets them through its own per-run option. Nothing is written into
        their settings folders. ShadowCode&apos;s own agent always reads them.
      </p>

      {overview.issues.length > 0 && (
        <ul className="rules-issues" aria-label="Rule conflicts and problems">
          {overview.issues.map((issue) => (
            <li key={issue}>{issue}</li>
          ))}
        </ul>
      )}

      <RuleList
        title="From your profile"
        empty="Nothing in your profile yet. Write rules above, install starter skills, or import a profile."
        items={profileItems}
        pending={pending}
        onToggle={(item, enabled) =>
          void run(`toggle:${item.id}`, () =>
            api.setRuleEnabled(item.id, enabled),
          )
        }
      />
      <RuleList
        title="From this project"
        empty={
          overview.workspace
            ? "This project has no rules, skills or commands of its own."
            : "Open a project to see its rules."
        }
        items={projectItems}
        pending={pending}
        note="Switching a project file off keeps it out of what ShadowCode sends. A vendor CLI that reads the file itself (for example Claude Code and CLAUDE.md) still reads it."
        onToggle={(item, enabled) =>
          void run(`toggle:${item.id}`, () =>
            api.setRuleEnabled(item.id, enabled, overview.workspace),
          )
        }
      />

      {overview.workspace && <AgentPreview version={version} />}

      <h4>Starter skills</h4>
      <p className="hint">
        Skills written for ShadowCode. They are copied into your profile only
        when you install them, and you can edit or switch them off there.
      </p>
      <ul className="rules-starters">
        {overview.starters.map((starter) => (
          <li key={starter.name}>
            <label className="check">
              <input
                type="checkbox"
                disabled={starter.installed || Boolean(pending)}
                checked={starter.installed || chosen.includes(starter.name)}
                onChange={(e) =>
                  setChosen((current) =>
                    e.target.checked
                      ? [...current, starter.name]
                      : current.filter((n) => n !== starter.name),
                  )
                }
              />{" "}
              <strong>{starter.title}</strong>
              {starter.installed ? " · Installed" : ""}
            </label>
            <p className="hint">{starter.summary}</p>
          </li>
        ))}
      </ul>
      {starters.length > 0 && (
        <button
          type="button"
          className="mini"
          disabled={chosen.length === 0 || Boolean(pending)}
          onClick={() =>
            void run(
              "starters",
              async () => {
                await api.installStarters(chosen);
                setChosen([]);
              },
              "Starter skills installed",
            )
          }
        >
          {pending === "starters" ? "Installing…" : "Install selected"}
        </button>
      )}

      {transportKind() === "remote" ? (
        <p className="hint">
          Importing a profile from Git and using your rules in other CLIs work
          on the computer running ShadowCode.
        </p>
      ) : (
        <>
          <h4>Import a profile from Git</h4>
          <p className="hint">
            Clones an https:// or SSH repository with AGENTS.md, skills/,
            commands/ or agents/ into your profile. Hooks and scripts in it
            never run; its files are only read as text.
          </p>
          <div className="row rules-import">
            <label htmlFor="rules-import-url" className="sr-only">
              Repository address
            </label>
            <input
              id="rules-import-url"
              value={url}
              placeholder="https://github.com/you/agent-rules.git"
              spellCheck={false}
              onChange={(e) => setUrl(e.target.value)}
            />
            <button
              type="button"
              className="mini"
              disabled={!url.trim() || Boolean(pending)}
              onClick={() =>
                void run(
                  "import",
                  async () => {
                    const result = await api.importRules(url.trim());
                    setUrl("");
                    onToast(
                      `Imported ${result.name} at ${result.commit.short ?? "its latest commit"}`,
                      "ok",
                    );
                  },
                  "",
                )
              }
            >
              {pending === "import" ? "Importing…" : "Import"}
            </button>
          </div>
          {overview.imports.length > 0 && (
            <ul className="rules-imports" aria-label="Imported profiles">
              {overview.imports.map((item) => (
                <li key={item.name}>
                  <div>
                    <strong>{item.name}</strong>
                    <span className="dim">{item.url || item.path}</span>
                    <span className="dim">
                      {item.commit.short
                        ? `Commit ${item.commit.short}${item.commit.subject ? ` · ${item.commit.subject}` : ""}${item.commit.date ? ` · ${new Date(item.commit.date).toLocaleDateString()}` : ""}`
                        : "Commit unknown"}
                    </span>
                  </div>
                  <div className="row">
                    <button
                      type="button"
                      className="mini"
                      disabled={Boolean(pending)}
                      onClick={() =>
                        void run(`update:${item.name}`, async () => {
                          const result = await api.updateRulesImport(item.name);
                          onToast(
                            result.changed
                              ? `${item.name} updated to ${result.commit.short}`
                              : `${item.name} is up to date`,
                            "ok",
                          );
                        })
                      }
                    >
                      {pending === `update:${item.name}`
                        ? "Updating…"
                        : "Update"}
                    </button>
                    <button
                      type="button"
                      className="mini ghost danger-text"
                      disabled={Boolean(pending)}
                      onClick={() => setRemoving(item.name)}
                    >
                      Remove
                    </button>
                  </div>
                </li>
              ))}
            </ul>
          )}
        </>
      )}
      {removing && (
        <ConfirmDialog
          title={`Remove ${removing}?`}
          confirmLabel="Remove"
          danger
          onCancel={() => setRemoving(null)}
          onConfirm={async () => {
            const name = removing;
            setRemoving(null);
            await run(
              `remove:${name}`,
              () => api.removeRulesImport(name),
              `${name} removed`,
            );
          }}
        >
          <p>
            Its folder is deleted from your profile. The repository itself is
            not changed; you can import it again later.
          </p>
        </ConfirmDialog>
      )}

      {transportKind() !== "remote" && <ExportPanel onToast={onToast} />}
    </section>
  );
}

function RuleList({
  title,
  empty,
  items,
  pending,
  note,
  onToggle,
}: {
  title: string;
  empty: string;
  items: RuleItem[];
  pending: string;
  note?: string;
  onToggle: (item: RuleItem, enabled: boolean) => void;
}) {
  return (
    <>
      <h4>{title}</h4>
      {note && items.length > 0 && <p className="hint">{note}</p>}
      {items.length === 0 ? (
        <p className="dim">{empty}</p>
      ) : (
        <ul className="rules-items" aria-label={title}>
          {items.map((item) => {
            const label = `${KIND_LABEL[item.kind] ?? item.kind}: ${item.name}`;
            return (
              <li key={item.id} className={item.enabled ? "" : "off"}>
                <label className="check">
                  <input
                    type="checkbox"
                    checked={item.enabled}
                    disabled={Boolean(pending)}
                    aria-label={`Use ${label}`}
                    onChange={(e) => onToggle(item, e.target.checked)}
                  />{" "}
                  <strong>{label}</strong>
                  <span className="dim"> · {sourceLabel(item)}</span>
                </label>
                {item.description && <p className="hint">{item.description}</p>}
                <code className="rules-path">{item.path}</code>
                {item.overridden_by && (
                  <p className="hint">
                    Not used here: {item.overridden_by} has the same name.
                  </p>
                )}
              </li>
            );
          })}
        </ul>
      )}
    </>
  );
}

/** What each agent reads for this project, as a context inventory. */
function AgentPreview({ version }: { version: number }) {
  const [preview, setPreview] = useState<RulesPreview | null>(null);
  const [error, setError] = useState("");
  const [runner, setRunner] = useState("shadowcode");
  const load = useCallback(async () => {
    try {
      setPreview(await api.rulesPreview());
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load, version]);
  const selected = useMemo(
    () => preview?.runners.find((r) => r.id === runner) ?? null,
    [preview, runner],
  );
  return (
    <>
      <h4>What each agent reads</h4>
      {error && <LoadError message={error} onRetry={load} />}
      {!preview && !error && <p role="status">Working out what agents read…</p>}
      {preview && (
        <div className="rules-preview">
          <div className="field">
            <label htmlFor="rules-preview-agent">Agent</label>
            <select
              id="rules-preview-agent"
              value={runner}
              onChange={(e) => setRunner(e.target.value)}
            >
              {preview.runners.map((r) => (
                <option key={r.id} value={r.id}>
                  {r.label}
                </option>
              ))}
            </select>
          </div>
          {selected && (
            <div className="context-inventory rules-inventory">
              <p className="context-inventory-note">
                {selected.mechanism}
                {selected.sharing_off
                  ? " Sending rules to vendor agents is off, so nothing below is sent."
                  : ""}
              </p>
              {selected.preview.items.length === 0 ? (
                <p>No rules or skills for this agent here.</p>
              ) : (
                <InventoryItems
                  items={selected.preview.items}
                  label={`What ${selected.label} reads`}
                />
              )}
              <p className="context-inventory-note" role="status">
                {selected.delivered || selected.id === "shadowcode"
                  ? `About ${selected.preview.estimated_tokens.toLocaleString()} tokens estimated · ${selected.preview.included_bytes.toLocaleString()} bytes`
                  : "Nothing is sent to this agent."}
              </p>
              {selected.native_files.length > 0 && (
                <p className="context-inventory-note">
                  {selected.label} also reads {selected.native_files.join(", ")}{" "}
                  and finds skills in {selected.native_skill_folders.join(", ")}{" "}
                  by itself.
                </p>
              )}
            </div>
          )}
        </div>
      )}
    </>
  );
}

/** Optional: use the profile in the Claude Code and Codex CLIs outside
 * ShadowCode, only when the user asks. */
function ExportPanel({ onToast }: { onToast: Toast }) {
  const [targets, setTargets] = useState<RulesExportTarget[] | null>(null);
  const [error, setError] = useState("");
  const [pending, setPending] = useState("");
  const load = useCallback(async () => {
    try {
      setTargets((await api.rulesExport()).targets);
      setError("");
    } catch (e) {
      setError(String(e));
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);
  return (
    <>
      <h4>Use these rules outside ShadowCode</h4>
      <p className="hint">
        Adds links named shadowcode-… in the Claude Code or Codex folders so
        those CLIs read your profile too. Existing files are never replaced, and
        turning this off removes only the links it made.
      </p>
      {error && <LoadError message={error} onRetry={load} />}
      {targets?.map((target) => {
        const linked = target.links.filter((l) => l.state === "linked").length;
        const blocked = target.links.filter((l) => l.state === "blocked");
        return (
          <div className="rules-export" key={target.id}>
            <div>
              <strong>{target.label}</strong>{" "}
              <span className="dim">
                {target.enabled
                  ? `${linked} of ${target.links.length} linked`
                  : `${target.links.length} links available`}
              </span>
            </div>
            {target.links.length > 0 && (
              <ul aria-label={`${target.label} links`}>
                {target.links.map((l) => (
                  <li key={l.link}>
                    <code>{l.link}</code>
                    {l.state === "blocked" && (
                      <span className="dim"> · a file already exists here</span>
                    )}
                  </li>
                ))}
              </ul>
            )}
            {blocked.length > 0 && !target.enabled && (
              <p className="hint">
                Files that already exist are left alone; add your rules to them
                yourself if you want.
              </p>
            )}
            <button
              type="button"
              className={target.enabled ? "mini ghost" : "mini"}
              disabled={
                Boolean(pending) ||
                (!target.enabled && target.links.length === 0)
              }
              onClick={async () => {
                setPending(target.id);
                try {
                  await api.setRulesExport(target.id, !target.enabled);
                  onToast(
                    target.enabled
                      ? `Stopped using your rules in ${target.label}`
                      : `Your rules are now used in ${target.label}`,
                    "ok",
                  );
                } catch (e) {
                  onToast(String(e), "err");
                } finally {
                  setPending("");
                  await load();
                }
              }}
            >
              {target.enabled
                ? `Stop using in ${target.label}`
                : `Use in ${target.label}`}
            </button>
          </div>
        );
      })}
    </>
  );
}
