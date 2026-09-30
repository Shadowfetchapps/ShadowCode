//! Spending limits for paid API models: OpenRouter, or any endpoint that
//! bills per token. Subscriptions (vendor CLIs) and models on this computer
//! are never limited here.
//!
//! Two limits, both in US dollars and both optional (`spending` in the
//! settings): one per task (default $1) and one per day across every task
//! and project (default $10, reset at local midnight). A task's subagents
//! count toward the task that started them.
//!
//! - At 75% of a limit the task shows a calm `spend.notice`, once.
//! - At 100% the task waits *between* model turns (never inside a tool call)
//!   and shows a `spend.limit_reached` card: continue with the limit raised by
//!   one step, or stop. The answer is `spend.limit_resolved`.
//! - Costs worked out from the published price list count and are marked
//!   estimated. A paid turn whose price is not known at all is said once
//!   (`spend.unknown`), never counted as $0.
//!
//! The per-task total lives in the task's [`Meter`] (shared with its
//! subagents); the day's total is a `native_meta` document, so it spans
//! processes and survives a restart.
use crate::{config::ModelConfig, events::TaskEvents, models::Usage, store::Store};
use anyhow::{bail, ensure, Result};
use chrono::{Local, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Mutex;
use tokio::sync::Notify;

/// The per-task limit when the settings do not say otherwise.
pub const DEFAULT_TASK_USD: f64 = 1.0;
/// The daily limit when the settings do not say otherwise.
pub const DEFAULT_DAILY_USD: f64 = 10.0;
/// Share of a limit at which the task mentions it.
pub const NOTICE_SHARE: f64 = 0.75;
/// The largest limit accepted, as a guard against typos.
pub const MAX_LIMIT_USD: f64 = 100_000.0;
/// `native_meta` key of the day's total.
pub const DAY_KEY: &str = "spending_day";

/// `spending` in the settings. `null` turns a limit off.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SpendingConfig {
    /// Ask before one task spends more than this on paid API models.
    pub task_usd: Option<f64>,
    /// Ask before all tasks together spend more than this in one day.
    pub daily_usd: Option<f64>,
}
impl Default for SpendingConfig {
    fn default() -> Self {
        Self {
            task_usd: Some(DEFAULT_TASK_USD),
            daily_usd: Some(DEFAULT_DAILY_USD),
        }
    }
}
impl SpendingConfig {
    pub fn validate(&self) -> Result<()> {
        for (name, value) in [("task_usd", self.task_usd), ("daily_usd", self.daily_usd)] {
            if let Some(value) = value {
                ensure!(
                    value.is_finite() && value > 0.0 && value <= MAX_LIMIT_USD,
                    "spending.{name} must be empty (no limit) or an amount between $0.01 and ${MAX_LIMIT_USD:.0}"
                );
                ensure!(
                    value >= 0.01,
                    "spending.{name} must be at least $0.01, or empty for no limit"
                );
            }
        }
        Ok(())
    }
}

/// A model billed per token: OpenRouter, or a compatible endpoint that is
/// not on this computer. Vendor CLIs (subscriptions), local models and the
/// offline preview are not.
pub fn is_paid(model: &ModelConfig) -> bool {
    if crate::cli_agent::is_cli_provider(&model.provider) || model.provider == "mock" {
        return false;
    }
    model.provider == crate::openrouter::PROVIDER || !crate::config::runs_on_this_computer(model)
}

/// US dollars for people: `$1.00`, `$0.04`, or `less than $0.01`.
pub fn money(usd: f64) -> String {
    if usd > 0.0 && usd < 0.005 {
        "less than $0.01".into()
    } else {
        format!("${:.2}", usd.max(0.0))
    }
}

/// An amount, labelled when some of it was worked out from the price list.
fn amount(usd: f64, estimated: bool) -> String {
    if estimated {
        format!("about {} (estimated)", money(usd))
    } else {
        money(usd)
    }
}

/// The next limit after `limit` when the user continues: one more `step`,
/// and more than what is already spent.
pub fn raised(limit: f64, step: f64, spent: f64) -> f64 {
    let step = if step > 0.0 { step } else { limit.max(0.01) };
    let mut next = limit + step;
    while next <= spent && next < MAX_LIMIT_USD {
        next += step;
    }
    (next * 100.0).round() / 100.0
}

/// The local calendar day (`YYYY-MM-DD`) of a Unix time.
pub fn day_of(ts: f64) -> String {
    Local
        .timestamp_opt(ts.floor() as i64, 0)
        .earliest()
        .map(|t| t.date_naive().to_string())
        .unwrap_or_default()
}

/// Unix time of the next local midnight after `ts`.
pub fn next_midnight(ts: f64) -> f64 {
    let Some(now) = Local.timestamp_opt(ts.floor() as i64, 0).earliest() else {
        return ts + 86_400.0;
    };
    now.date_naive()
        .succ_opt()
        .and_then(|day| day.and_hms_opt(0, 0, 0))
        .and_then(|midnight| Local.from_local_datetime(&midnight).earliest())
        .map(|t| t.timestamp() as f64)
        .unwrap_or(ts + 86_400.0)
}

/// The day's total across every task (`native_meta` [`DAY_KEY`]).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Day {
    /// Local date this total belongs to.
    pub day: String,
    pub usd: f64,
    /// Some of it was worked out from the price list.
    pub estimated: bool,
    /// Paid turns whose price was not known (not in `usd`).
    pub unknown_turns: u64,
    /// The daily limit for this day after "Continue".
    pub raised_to: Option<f64>,
    /// The 75% notice was shown today.
    pub noticed: bool,
}
impl Day {
    /// Start a new day when the stored one is older.
    fn roll(&mut self, today: &str) {
        if self.day != today {
            *self = Self {
                day: today.to_owned(),
                ..Default::default()
            };
        }
    }
}

/// Today's total as stored (zero when nothing was spent yet today).
pub fn today(store: &Store, now: f64) -> Result<Day> {
    let today = day_of(now);
    let mut day: Day = store
        .native_meta(DAY_KEY)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    day.roll(&today);
    Ok(day)
}

fn update_day<R>(store: &Store, now: f64, change: impl FnOnce(&mut Day) -> R) -> Result<R> {
    let today = day_of(now);
    store.update_native_json(DAY_KEY, |day: &mut Day| {
        day.roll(&today);
        change(day)
    })
}

/// The daily limit in force today: the setting, or what "Continue" raised
/// it to.
pub fn daily_limit(config: &SpendingConfig, day: &Day) -> Option<f64> {
    config
        .daily_usd
        .map(|base| day.raised_to.map_or(base, |raised| raised.max(base)))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Task,
    Daily,
}

/// A limit reached and waiting for the user's answer.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Prompt {
    pub id: String,
    /// The task's own job (subagents ask in their parent task).
    pub job_id: String,
    pub kind: Kind,
    pub limit: f64,
    pub spent: f64,
    pub estimated: bool,
    /// The limit after "Continue".
    pub raise_to: f64,
    /// For the daily limit: when it resets on its own.
    pub resets_at: Option<f64>,
}
impl Prompt {
    pub fn to_json(&self) -> Value {
        let mut value = json!(self);
        value["title"] = json!(match self.kind {
            Kind::Task => "This task reached its spending limit",
            Kind::Daily => "Today's spending limit is reached",
        });
        value["text"] = json!(match self.kind {
            Kind::Task => format!(
                "It has spent {} on paid models, and the limit for one task is {}. It is paused between steps; nothing is running.",
                amount(self.spent, self.estimated),
                money(self.limit)
            ),
            Kind::Daily => format!(
                "Paid models have cost {} today across all tasks, and your daily limit is {}. This task is paused between steps; nothing is running. The daily total starts again at midnight.",
                amount(self.spent, self.estimated),
                money(self.limit)
            ),
        });
        value["continue_label"] = json!(match self.kind {
            Kind::Task => format!("Continue (limit raised to {})", money(self.raise_to)),
            Kind::Daily => format!(
                "Continue (today's limit raised to {})",
                money(self.raise_to)
            ),
        });
        value
    }
}

/// What the check before a model turn found.
#[derive(Clone, Debug, PartialEq)]
pub enum Check {
    Clear,
    Reached(Kind, f64),
}

#[derive(Default)]
struct State {
    spent: f64,
    estimated: bool,
    /// The task limit after "Continue".
    raised: Option<f64>,
    task_noticed: bool,
    unknown_noted: bool,
    prompt: Option<Prompt>,
    /// The user chose Stop.
    stopped: Option<Kind>,
}

/// One task's paid spending, shared with its subagents. Its events go to
/// the task the user started.
pub struct Meter {
    job_id: String,
    events: TaskEvents,
    /// `--max-cost` for this task: replaces the per-task setting.
    max_cost: Option<f64>,
    /// No one sees this task's conversation (a second opinion's hidden
    /// review): at a limit it stops instead of waiting for an answer.
    unattended: bool,
    state: Mutex<State>,
    changed: Notify,
}

impl Meter {
    pub fn new(job_id: &str, events: TaskEvents, max_cost: Option<f64>) -> Self {
        Self {
            job_id: job_id.to_owned(),
            events,
            max_cost,
            unattended: false,
            state: Mutex::default(),
            changed: Notify::new(),
        }
    }
    /// A meter whose task stops at a limit instead of showing a card.
    pub fn unattended(mut self) -> Self {
        self.unattended = true;
        self
    }
    pub fn is_unattended(&self) -> bool {
        self.unattended
    }
    pub fn job_id(&self) -> &str {
        &self.job_id
    }
    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    /// Spent so far and whether any of it is an estimate.
    pub fn spent(&self) -> (f64, bool) {
        let state = self.state();
        (state.spent, state.estimated)
    }
    /// Which limit the user stopped at, if they chose Stop.
    pub fn stopped(&self) -> Option<Kind> {
        self.state().stopped
    }
    pub fn pending(&self) -> Option<Prompt> {
        self.state().prompt.clone()
    }
    /// The waiting card with the conversation and task it is shown in.
    pub fn waiting_card(&self) -> Option<Value> {
        let mut card = self.pending()?.to_json();
        card["session_id"] = json!(self.events.session_id);
        card["task_id"] = json!(self.events.task_id);
        Some(card)
    }
    pub fn notified(&self) -> tokio::sync::futures::Notified<'_> {
        self.changed.notified()
    }

    /// The per-task limit in force: `--max-cost`, else the setting, raised
    /// by any "Continue".
    pub fn task_limit(&self, config: &SpendingConfig) -> Option<f64> {
        let base = self.max_cost.or(config.task_usd)?;
        Some(self.state().raised.map_or(base, |raised| raised.max(base)))
    }

    /// Count one priced model request of `model` (a no-op for models that
    /// are not paid). Adds to the task and to today's total.
    pub fn record(&self, store: &Store, model: &ModelConfig, turn: &Usage) -> Result<()> {
        if !is_paid(model) || (turn.turns == 0 && turn.total_tokens == 0) {
            return Ok(());
        }
        let now = crate::now();
        match turn.cost_usd {
            Some(cost) if cost.is_finite() && cost >= 0.0 => {
                {
                    let mut state = self.state();
                    state.spent += cost;
                    state.estimated |= turn.cost_estimated;
                }
                update_day(store, now, |day| {
                    day.usd += cost;
                    day.estimated |= turn.cost_estimated;
                })?;
            }
            _ => {
                update_day(store, now, |day| day.unknown_turns += 1)?;
                let first = !std::mem::replace(&mut self.state().unknown_noted, true);
                if first {
                    self.events.emit(
                        "spend.unknown",
                        json!({
                            "job_id": self.job_id,
                            "model": model.name,
                            "text": format!(
                                "The price of {} isn't known, so what this task spends on it can't be counted toward your spending limits.",
                                if model.name.is_empty() { "this model" } else { model.name.as_str() }
                            ),
                        }),
                    )?;
                }
            }
        }
        Ok(())
    }

    /// Before a model turn: is a limit reached? Shows the 75% notices once.
    pub fn check(&self, store: &Store, config: &SpendingConfig, now: f64) -> Result<Check> {
        let (spent, estimated) = self.spent();
        let task_limit = self.task_limit(config);
        if let Some(limit) = task_limit {
            if spent >= limit {
                return Ok(Check::Reached(Kind::Task, limit));
            }
        }
        let day = today(store, now)?;
        let daily = daily_limit(config, &day);
        if let Some(limit) = daily {
            if day.usd >= limit {
                return Ok(Check::Reached(Kind::Daily, limit));
            }
        }
        if let Some(limit) = task_limit {
            let first = spent >= NOTICE_SHARE * limit
                && !std::mem::replace(&mut self.state().task_noticed, true);
            if first {
                self.events.emit(
                    "spend.notice",
                    json!({
                        "job_id": self.job_id,
                        "kind": Kind::Task,
                        "spent": spent,
                        "limit": limit,
                        "estimated": estimated,
                        "text": format!(
                            "This task has spent {} of its {} limit on paid models.",
                            amount(spent, estimated),
                            money(limit)
                        ),
                    }),
                )?;
            }
        }
        if let Some(limit) = daily {
            if day.usd >= NOTICE_SHARE * limit && !day.noticed {
                let first =
                    update_day(store, now, |day| !std::mem::replace(&mut day.noticed, true))?;
                if first {
                    self.events.emit(
                        "spend.notice",
                        json!({
                            "job_id": self.job_id,
                            "kind": Kind::Daily,
                            "spent": day.usd,
                            "limit": limit,
                            "estimated": day.estimated,
                            "text": format!(
                                "Paid models have cost {} today, of your {} daily limit.",
                                amount(day.usd, day.estimated),
                                money(limit)
                            ),
                        }),
                    )?;
                }
            }
        }
        Ok(Check::Clear)
    }

    /// The card for a reached limit: the one already waiting for this
    /// limit, or a new one (announced with `spend.limit_reached`). A card
    /// waiting for another limit, or for this one before a setting changed,
    /// is lifted first.
    pub fn ask(
        &self,
        store: &Store,
        config: &SpendingConfig,
        kind: Kind,
        limit: f64,
        now: f64,
    ) -> Result<Prompt> {
        if let Some(prompt) = self.pending() {
            if prompt.kind == kind && prompt.limit == limit {
                return Ok(prompt);
            }
            self.lift("limit_changed")?;
        }
        let (spent, estimated, step) = match kind {
            Kind::Task => {
                let (spent, estimated) = self.spent();
                let step = self
                    .max_cost
                    .or(config.task_usd)
                    .unwrap_or(DEFAULT_TASK_USD);
                (spent, estimated, step)
            }
            Kind::Daily => {
                let day = today(store, now)?;
                (
                    day.usd,
                    day.estimated,
                    config.daily_usd.unwrap_or(DEFAULT_DAILY_USD),
                )
            }
        };
        let prompt = Prompt {
            id: crate::id(),
            job_id: self.job_id.clone(),
            kind,
            limit,
            spent,
            estimated,
            raise_to: raised(limit, step, spent),
            resets_at: (kind == Kind::Daily).then(|| next_midnight(now)),
        };
        {
            let mut state = self.state();
            if let Some(existing) = &state.prompt {
                return Ok(existing.clone());
            }
            state.prompt = Some(prompt.clone());
        }
        self.events.emit("spend.limit_reached", prompt.to_json())?;
        Ok(prompt)
    }

    /// Clear a waiting card whose limit no longer applies (the setting was
    /// raised or turned off, or a new day began).
    pub fn lift(&self, reason: &str) -> Result<()> {
        let Some(prompt) = self.state().prompt.take() else {
            return Ok(());
        };
        self.events.emit(
            "spend.limit_resolved",
            json!({"prompt_id": prompt.id, "job_id": self.job_id, "kind": prompt.kind, "action": "continue", "reason": reason}),
        )?;
        self.changed.notify_waiters();
        Ok(())
    }

    /// The user's answer to the waiting card: `continue` raises the limit
    /// one step, `stop` ends the task (the engine cancels it).
    pub fn decide(&self, store: &Store, prompt_id: &str, action: &str) -> Result<Value> {
        let prompt = {
            let mut state = self.state();
            let Some(prompt) = state.prompt.clone().filter(|p| p.id == prompt_id) else {
                bail!("This spending question was already answered");
            };
            match action {
                "continue" => {
                    if prompt.kind == Kind::Task {
                        state.raised = Some(prompt.raise_to);
                        state.task_noticed = false;
                    }
                }
                "stop" => state.stopped = Some(prompt.kind),
                _ => bail!("Answer with continue or stop"),
            }
            state.prompt = None;
            prompt
        };
        if action == "continue" && prompt.kind == Kind::Daily {
            update_day(store, crate::now(), |day| {
                day.raised_to = Some(day.raised_to.unwrap_or(0.0).max(prompt.raise_to));
                day.noticed = false;
            })?;
        }
        let payload = json!({
            "prompt_id": prompt.id,
            "job_id": self.job_id,
            "kind": prompt.kind,
            "action": action,
            "limit": (action == "continue").then_some(prompt.raise_to),
            "text": if action == "continue" {
                match prompt.kind {
                    Kind::Task => format!("Continuing. This task's limit is now {}.", money(prompt.raise_to)),
                    Kind::Daily => format!("Continuing. Today's limit is now {}.", money(prompt.raise_to)),
                }
            } else {
                "Stopped at your spending limit. Changes made so far are kept.".to_owned()
            },
        });
        self.events.emit("spend.limit_resolved", payload.clone())?;
        self.changed.notify_waiters();
        Ok(payload)
    }
}

/// A task's own limit from a request (`max_cost_usd`, the CLI's
/// `--max-cost`): absent or `null` keeps the setting.
pub fn parse_max_cost(value: Option<&Value>) -> Result<Option<f64>> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let usd = value.as_f64().or_else(|| {
        value
            .as_str()
            .and_then(|s| s.trim().trim_start_matches('$').parse().ok())
    });
    match usd {
        Some(usd) if usd.is_finite() && (0.01..=MAX_LIMIT_USD).contains(&usd) => Ok(Some(usd)),
        _ => bail!(
            "max_cost_usd must be an amount in US dollars between 0.01 and {MAX_LIMIT_USD:.0}"
        ),
    }
}

/// The finished task's summary after the user chose Stop at a limit.
pub fn stopped_summary(kind: Kind) -> String {
    format!(
        "Stopped at your {} spending limit. Changes made so far remain available for review or rewind.",
        match kind {
            Kind::Task => "per-task",
            Kind::Daily => "daily",
        }
    )
}

/// The finished task's summary when it could not wait for an answer at a
/// limit (see [`Meter::unattended`]).
pub fn unattended_summary(kind: Kind, limit: f64) -> String {
    format!(
        "{} Raise it in Settings › Accounts › Spending limits, or choose a subscription or a model on this computer.",
        match kind {
            Kind::Task => format!(
                "Stopped at the per-task spending limit for paid models ({}).",
                money(limit)
            ),
            Kind::Daily => format!(
                "Today's spending limit for paid models ({}) is reached.",
                money(limit)
            ),
        }
    )
}

/// A rough price for the next message on a paid model: the conversation so
/// far read once (a short answer) to three times (a few tool steps), plus
/// 200 to 4,000 output tokens. `None` without both prices.
pub fn estimate(
    context_tokens: u64,
    input_price: Option<f64>,
    output_price: Option<f64>,
) -> Option<(f64, f64)> {
    let (input, output) = (input_price?, output_price?);
    if !(input.is_finite() && output.is_finite()) || input < 0.0 || output < 0.0 {
        return None;
    }
    let context = context_tokens as f64;
    let low = context * input + 200.0 * output;
    let high = 3.0 * context * input + 4_000.0 * output;
    Some((low, high))
}

/// The composer's label for an estimate: `about $0.01–$0.05`.
pub fn estimate_label(low: f64, high: f64) -> String {
    let cents = |usd: f64| (usd * 100.0).ceil() / 100.0;
    let (low, high) = (cents(low).max(0.01), cents(high).max(0.01));
    if high <= 0.01 {
        "less than $0.01".into()
    } else if (high - low).abs() < 0.005 {
        format!("about {}", money(high))
    } else {
        format!("about {}–{}", money(low), money(high))
    }
}

#[cfg(test)]
mod tests;
