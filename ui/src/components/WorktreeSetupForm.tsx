import { useEffect, useState } from "react";
import { api, type WorktreeSetup } from "../api";

const lines = (text: string) =>
  text
    .split("\n")
    .map((l) => l.trim())
    .filter(Boolean);

/** What each new task worktree of this project gets: copied files, setup
 * and teardown commands, and the range its own PORT comes from. */
export function WorktreeSetupForm({
  onToast,
}: {
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const [loaded, setLoaded] = useState<{
    setup: WorktreeSetup;
    suggested: WorktreeSetup;
  } | null>(null);
  const [copy, setCopy] = useState("");
  const [setup, setSetup] = useState("");
  const [teardown, setTeardown] = useState("");
  const [ports, setPorts] = useState("3100-3999");
  const [saving, setSaving] = useState(false);
  const fill = (s: WorktreeSetup) => {
    setCopy(s.copy.join("\n"));
    setSetup(s.setup.join("\n"));
    setTeardown(s.teardown.join("\n"));
    setPorts(`${s.port_start}-${s.port_end}`);
  };
  useEffect(() => {
    let live = true;
    Promise.resolve()
      .then(() => api.worktreeSetup())
      .then((answer) => {
        if (!live) return;
        setLoaded(answer);
        fill(answer.setup);
      })
      .catch(() => undefined);
    return () => {
      live = false;
    };
  }, []);
  if (!loaded) return null;
  // Offered only while nothing is set up yet.
  const suggestion =
    loaded.suggested.copy.length + loaded.suggested.setup.length > 0 &&
    loaded.setup.copy.length + loaded.setup.setup.length === 0;
  async function save() {
    const [start, end] = ports.split("-").map((p) => Number(p.trim()));
    setSaving(true);
    try {
      const answer = await api.saveWorktreeSetup({
        copy: lines(copy),
        setup: lines(setup),
        teardown: lines(teardown),
        port_start: start || 3100,
        port_end: end || start || 3999,
      });
      setLoaded({ ...loaded!, setup: answer.setup });
      onToast("Worktree setup saved", "ok");
    } catch (e) {
      onToast(String(e), "err");
    } finally {
      setSaving(false);
    }
  }
  return (
    <section className="worktree-setup" aria-label="Setup for new worktrees">
      <h4>Setup for new worktrees</h4>
      <p className="hint">
        Each task run in a new worktree gets these files from the project and
        runs these commands first (as you, in the worktree), with its own{" "}
        <code>PORT</code>. Teardown commands run before the worktree is removed.
      </p>
      {suggestion && (
        <button
          type="button"
          className="link"
          onClick={() => fill(loaded.suggested)}
        >
          Use suggestions for this project
        </button>
      )}
      <label>
        Files to copy (one per line)
        <textarea
          rows={2}
          value={copy}
          onChange={(e) => setCopy(e.target.value)}
        />
      </label>
      <label>
        Setup commands (one per line)
        <textarea
          rows={2}
          value={setup}
          onChange={(e) => setSetup(e.target.value)}
        />
      </label>
      <label>
        Teardown commands (one per line)
        <textarea
          rows={2}
          value={teardown}
          onChange={(e) => setTeardown(e.target.value)}
        />
      </label>
      <label>
        Ports
        <input value={ports} onChange={(e) => setPorts(e.target.value)} />
      </label>
      <button
        type="button"
        className="primary"
        disabled={saving}
        onClick={() => void save()}
      >
        Save setup
      </button>
    </section>
  );
}
