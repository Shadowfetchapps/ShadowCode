//! `/api/spending…`: spending limits for paid API models (`crate::spending`).
//! The answer to a task's limit card is `POST /api/jobs/<id>/spending`
//! (`service::jobs`).
use super::*;
use crate::store::keys;

/// Tokens sent with every request besides the conversation when a
/// conversation has no saved turn yet: the system prompt and rules.
const SYSTEM_TOKENS: u64 = 2_000;

impl Service {
    pub(super) async fn spending_routes(&self, call: &Arc<Call>) -> Result<Value> {
        match (call.method.as_str(), call.path.as_str()) {
            ("GET", "/api/spending") => self.blocking(call, Self::spending_status).await,
            ("GET", "/api/spending/estimate") => self.blocking(call, Self::spending_estimate).await,
            _ => Err(call.unavailable()),
        }
    }

    /// GET /api/spending: the limits, today's total and waiting cards.
    fn spending_status(&self, _call: &Call) -> Result<Value> {
        let config = self.config()?;
        let now = crate::now();
        let day = crate::spending::today(&self.engine.store(), now)?;
        Ok(json!({
            "limits": config.spending,
            "today": {
                "day": day.day,
                "usd": day.usd,
                "estimated": day.estimated,
                "unknown_turns": day.unknown_turns,
                "limit": crate::spending::daily_limit(&config.spending, &day),
                "resets_at": crate::spending::next_midnight(now),
            },
            "waiting": self.engine.spending_waiting()?,
        }))
    }

    /// GET /api/spending/estimate?session_id=&model=: a rough price for the
    /// next message. `show` is false for subscriptions, models on this
    /// computer, and models without published prices.
    fn spending_estimate(&self, call: &Call) -> Result<Value> {
        let config = self.config()?;
        let store = self.engine.store();
        let session = call.q("session_id").trim();
        let session = (!session.is_empty()).then_some(session);
        let target = match call.q("model").trim() {
            "" => match session {
                Some(sid) => store.session_meta(sid, keys::EXECUTION_TARGET)?,
                None => None,
            }
            .or(store.native_meta(&keys::execution_target(&self.workspace()?))?),
            id => Some(id.to_owned()),
        };
        let model = match &target {
            Some(id) => self.resolve_model(id, &config.model)?,
            None => config.model.clone(),
        };
        let hidden = |reason: &str| Ok(json!({"show": false, "reason": reason}));
        if !crate::spending::is_paid(&model) {
            return hidden("not_paid");
        }
        let listed = (model.provider == crate::openrouter::PROVIDER)
            .then(|| crate::openrouter::model_in(&self.engine.paths().state, &model.name))
            .flatten();
        let Some(listed) = listed else {
            return hidden("no_prices");
        };
        let messages = match session {
            Some(sid) => store.latest_session_messages(sid, "")?,
            None => Vec::new(),
        };
        let has_system = messages.first().is_some_and(|m| m["role"] == "system");
        let context_tokens = crate::context::estimate_tokens(&json!(messages)) as u64
            + crate::context::estimate_tokens(&json!(crate::tools::schemas())) as u64
            + if has_system { 0 } else { SYSTEM_TOKENS }
            + call
                .q("draft_chars")
                .parse::<u64>()
                .unwrap_or(0)
                .min(512_000)
                / 3;
        let Some((low, high)) =
            crate::spending::estimate(context_tokens, listed.prompt_price, listed.completion_price)
        else {
            return hidden("no_prices");
        };
        Ok(json!({
            "show": true,
            "low_usd": low,
            "high_usd": high,
            "label": crate::spending::estimate_label(low, high),
            "context_tokens": context_tokens,
            "model": model.name,
            "detail": "Worked out from the conversation so far and the model's listed prices, for a short answer up to a few steps with tools. Longer tasks cost more.",
        }))
    }
}
