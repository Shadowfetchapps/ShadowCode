import { useEffect, useId, useState } from "react";
import { api, type SpendingStatus } from "../../api";
import { money, parseLimit } from "../../lib/spending";

type Draft = { on: boolean; amount: string };

const draftOf = (value: number | null | undefined, fallback: number): Draft =>
  value == null
    ? { on: false, amount: fallback.toFixed(2) }
    : { on: true, amount: value.toFixed(2) };

/** Settings › Accounts › Spending limits: when ShadowCode stops to ask
 * before paid API models (OpenRouter and other per-token providers) cost
 * more. Subscriptions and models on this computer are never limited. */
export function SpendingLimits({
  onToast,
}: {
  onToast: (text: string, kind: "ok" | "err" | "info") => void;
}) {
  const uid = useId().replace(/:/g, "");
  const [status, setStatus] = useState<SpendingStatus | null>(null);
  const [loadError, setLoadError] = useState("");
  const [task, setTask] = useState<Draft>({ on: true, amount: "1.00" });
  const [daily, setDaily] = useState<Draft>({ on: true, amount: "10.00" });
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState("");

  const load = async () => {
    try {
      const next = await api.spending();
      setStatus(next);
      setTask(draftOf(next.limits.task_usd, 1));
      setDaily(draftOf(next.limits.daily_usd, 10));
      setLoadError("");
    } catch (e) {
      setLoadError(String(e));
    }
  };
  useEffect(() => {
    void load();
  }, []);

  const save = async () => {
    const taskValue = task.on ? parseLimit(task.amount) : null;
    const dailyValue = daily.on ? parseLimit(daily.amount) : null;
    if (
      taskValue === "invalid" ||
      dailyValue === "invalid" ||
      (task.on && taskValue === null) ||
      (daily.on && dailyValue === null)
    ) {
      setError("Enter an amount between $0.01 and $100,000.");
      return;
    }
    setError("");
    setSaving(true);
    try {
      await api.saveConfig({
        spending: { task_usd: taskValue, daily_usd: dailyValue },
      });
      await load();
      onToast("Spending limits saved.", "ok");
    } catch (e) {
      setError(String(e));
    } finally {
      setSaving(false);
    }
  };

  const today = status?.today;
  const field = (
    id: string,
    label: string,
    draft: Draft,
    set: (draft: Draft) => void,
  ) => (
    <div className="spend-limit-row">
      <label className="check" htmlFor={`${uid}-${id}-on`}>
        <input
          id={`${uid}-${id}-on`}
          type="checkbox"
          checked={draft.on}
          onChange={(e) => set({ ...draft, on: e.target.checked })}
        />{" "}
        {label}
      </label>
      <span className="spend-limit-amount">
        <span aria-hidden="true">$</span>
        <input
          id={`${uid}-${id}`}
          type="text"
          inputMode="decimal"
          aria-label={`${label} (US dollars)`}
          disabled={!draft.on}
          value={draft.amount}
          onChange={(e) => set({ ...draft, amount: e.target.value })}
        />
      </span>
    </div>
  );

  return (
    <article className="account-card" aria-labelledby={`${uid}-title`}>
      <header>
        <h4 id={`${uid}-title`}>Spending limits</h4>
        <span className="cap-badge">Paid API models</span>
      </header>
      <p className="hint">
        When a paid model (such as one on OpenRouter) reaches a limit, the task
        pauses between steps and asks whether to continue. You'll also see a
        short note at 75%. Subscriptions and models on this computer are never
        limited.
      </p>
      {loadError && <p className="health-bad">{loadError}</p>}
      <div className="field spend-limits">
        {field("task", "Ask when one task spends more than", task, setTask)}
        {field(
          "daily",
          "Ask when all tasks together spend more than, per day",
          daily,
          setDaily,
        )}
      </div>
      {today && (
        <p className="dim" role="status">
          Today so far: {today.estimated ? "about " : ""}
          {money(today.usd)}
          {today.estimated ? " (estimated)" : ""}
          {today.limit != null ? ` of ${money(today.limit)}` : ""} · resets at
          midnight
          {today.unknown_turns > 0
            ? ` · ${today.unknown_turns} request${today.unknown_turns === 1 ? "" : "s"} with an unknown price not counted`
            : ""}
        </p>
      )}
      {error && (
        <p className="health-bad" role="alert">
          {error}
        </p>
      )}
      <div className="row">
        <button
          type="button"
          className="primary"
          disabled={saving || !status}
          onClick={() => void save()}
        >
          {saving ? "Saving…" : "Save limits"}
        </button>
      </div>
    </article>
  );
}
