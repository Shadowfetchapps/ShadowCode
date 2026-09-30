use super::*;
use std::{path::Path, sync::Arc};

fn model(provider: &str, endpoint: &str) -> ModelConfig {
    ModelConfig {
        default: String::new(),
        name: "acme/coder".into(),
        provider: provider.into(),
        endpoint: endpoint.into(),
        api_key_env: String::new(),
        keep_alive: "5m".into(),
        context_limit: 32_768,
    }
}

fn paid() -> ModelConfig {
    model("openrouter", "https://openrouter.ai/api/v1")
}

fn turn(cost: Option<f64>, estimated: bool) -> Usage {
    Usage {
        prompt_tokens: 100,
        completion_tokens: 10,
        total_tokens: 110,
        cost_usd: cost,
        cost_estimated: estimated,
        turns: 1,
        ..Default::default()
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    store: Arc<Store>,
    meter: Meter,
    session: String,
}

fn fixture(max_cost: Option<f64>) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("db")).unwrap());
    let session = store.create_session(Path::new("/tmp/p"), "m", "t").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (sender, _) = tokio::sync::broadcast::channel(64);
    let events = TaskEvents {
        store: store.clone(),
        session_id: session.clone(),
        task_id: "task".into(),
        sender,
    };
    Fixture {
        meter: Meter::new("job", events, max_cost),
        _dir: dir,
        store,
        session,
    }
}

fn events(f: &Fixture, kind: &str) -> Vec<Value> {
    f.store
        .recent_events(&f.session, 100)
        .unwrap()
        .into_iter()
        .filter(|e| e["type"] == kind)
        .map(|e| e["payload"].clone())
        .collect()
}

#[test]
fn only_per_token_api_models_are_paid() {
    assert!(is_paid(&paid()));
    // OpenRouter stays paid even on a loopback test endpoint.
    assert!(is_paid(&model("openrouter", "http://127.0.0.1:9/v1")));
    assert!(is_paid(&model(
        "openai_compatible",
        "https://api.example.com/v1"
    )));
    assert!(!is_paid(&model("llamacpp", "")));
    assert!(!is_paid(&model("ollama", "http://localhost:11434/v1")));
    assert!(!is_paid(&model(
        "openai_compatible",
        "http://127.0.0.1:8080/v1"
    )));
    assert!(!is_paid(&model("cli:codex", "")));
    assert!(!is_paid(&model("cli:claude", "")));
    assert!(!is_paid(&model("mock", "")));
}

#[test]
fn config_defaults_validate_and_turn_off() {
    let config = SpendingConfig::default();
    assert_eq!((config.task_usd, config.daily_usd), (Some(1.0), Some(10.0)));
    config.validate().unwrap();
    let off: SpendingConfig = serde_json::from_value(json!({"task_usd":null})).unwrap();
    assert_eq!((off.task_usd, off.daily_usd), (None, Some(10.0)));
    off.validate().unwrap();
    for bad in [json!(0), json!(-1), json!(0.001), json!(1e9)] {
        let config: SpendingConfig = serde_json::from_value(json!({"daily_usd":bad})).unwrap();
        assert!(config.validate().is_err(), "{bad}");
    }
    // The whole settings file validates the section too.
    let mut settings = crate::config::Config::default();
    settings.spending.task_usd = Some(f64::NAN);
    assert!(settings.validate().is_err());
}

#[test]
fn money_raising_and_estimates_read_plainly() {
    assert_eq!(money(1.0), "$1.00");
    assert_eq!(money(0.004), "less than $0.01");
    assert_eq!(money(0.0), "$0.00");
    assert_eq!(raised(1.0, 1.0, 1.02), 2.0);
    assert_eq!(raised(1.0, 1.0, 2.5), 3.0, "past what is already spent");
    assert_eq!(raised(10.0, 10.0, 10.0), 20.0);
    assert_eq!(estimate(10_000, None, Some(1e-6)), None);
    let (low, high) = estimate(10_000, Some(1e-6), Some(4e-6)).unwrap();
    assert!((low - 0.0108).abs() < 1e-9 && (high - 0.046).abs() < 1e-9);
    assert_eq!(estimate_label(low, high), "about $0.02–$0.05");
    assert_eq!(estimate_label(0.0001, 0.004), "less than $0.01");
    assert_eq!(estimate_label(0.03, 0.03), "about $0.03");
    let midnight = next_midnight(crate::now());
    assert!(midnight > crate::now() && midnight - crate::now() <= 25.0 * 3600.0);
    assert_ne!(day_of(midnight + 1.0), day_of(midnight - 1.0));
}

#[test]
fn the_task_limit_notices_at_75_percent_then_asks_and_continues() {
    let f = fixture(None);
    let config = SpendingConfig::default();
    let now = crate::now();
    // Local and subscription turns never count.
    f.meter
        .record(&f.store, &model("llamacpp", ""), &turn(Some(5.0), false))
        .unwrap();
    assert_eq!(f.meter.spent(), (0.0, false));
    f.meter
        .record(&f.store, &paid(), &turn(Some(0.5), false))
        .unwrap();
    assert_eq!(f.meter.check(&f.store, &config, now).unwrap(), Check::Clear);
    assert!(events(&f, "spend.notice").is_empty());
    f.meter
        .record(&f.store, &paid(), &turn(Some(0.3), true))
        .unwrap();
    assert_eq!(f.meter.check(&f.store, &config, now).unwrap(), Check::Clear);
    assert_eq!(f.meter.check(&f.store, &config, now).unwrap(), Check::Clear);
    let notices = events(&f, "spend.notice");
    assert_eq!(notices.len(), 1, "said once");
    assert_eq!(notices[0]["kind"], "task");
    assert_eq!(
        notices[0]["text"],
        "This task has spent about $0.80 (estimated) of its $1.00 limit on paid models."
    );
    f.meter
        .record(&f.store, &paid(), &turn(Some(0.3), false))
        .unwrap();
    let check = f.meter.check(&f.store, &config, now).unwrap();
    assert_eq!(check, Check::Reached(Kind::Task, 1.0));
    let prompt = f
        .meter
        .ask(&f.store, &config, Kind::Task, 1.0, now)
        .unwrap();
    assert_eq!(prompt.raise_to, 2.0);
    assert!(prompt.estimated);
    // Asking again (another subagent, the next check) shows the same card.
    assert_eq!(
        f.meter
            .ask(&f.store, &config, Kind::Task, 1.0, now)
            .unwrap(),
        prompt
    );
    let cards = events(&f, "spend.limit_reached");
    assert_eq!(cards.len(), 1);
    assert_eq!(
        cards[0]["continue_label"],
        "Continue (limit raised to $2.00)"
    );
    assert!(cards[0]["text"]
        .as_str()
        .unwrap()
        .starts_with("It has spent about $1.10 (estimated) on paid models"));
    assert!(f.meter.decide(&f.store, "other", "continue").is_err());
    assert!(f.meter.decide(&f.store, &prompt.id, "maybe").is_err());
    let answer = f.meter.decide(&f.store, &prompt.id, "continue").unwrap();
    assert_eq!(answer["limit"], 2.0);
    assert!(f.meter.pending().is_none());
    assert!(
        f.meter.decide(&f.store, &prompt.id, "continue").is_err(),
        "answered once"
    );
    assert_eq!(f.meter.task_limit(&config), Some(2.0));
    assert_eq!(f.meter.check(&f.store, &config, now).unwrap(), Check::Clear);
    assert_eq!(events(&f, "spend.limit_resolved").len(), 1);
    assert_eq!(f.meter.stopped(), None);
}

#[test]
fn stop_is_remembered_and_max_cost_replaces_the_setting() {
    let f = fixture(Some(0.2));
    let config = SpendingConfig::default();
    let now = crate::now();
    assert_eq!(f.meter.task_limit(&config), Some(0.2));
    let off = SpendingConfig {
        task_usd: None,
        daily_usd: None,
    };
    assert_eq!(f.meter.task_limit(&off), Some(0.2), "--max-cost wins");
    f.meter
        .record(&f.store, &paid(), &turn(Some(0.25), false))
        .unwrap();
    let Check::Reached(kind, limit) = f.meter.check(&f.store, &config, now).unwrap() else {
        panic!("limit not reached");
    };
    let prompt = f.meter.ask(&f.store, &config, kind, limit, now).unwrap();
    assert_eq!(prompt.raise_to, 0.4);
    let answer = f.meter.decide(&f.store, &prompt.id, "stop").unwrap();
    assert_eq!(answer["action"], "stop");
    assert_eq!(answer["limit"], Value::Null);
    assert_eq!(f.meter.stopped(), Some(Kind::Task));
}

#[test]
fn unknown_prices_are_said_once_and_never_count_as_zero() {
    let f = fixture(None);
    let hosted = model("openai_compatible", "https://api.example.com/v1");
    for _ in 0..3 {
        f.meter
            .record(&f.store, &hosted, &turn(None, false))
            .unwrap();
    }
    assert_eq!(f.meter.spent(), (0.0, false));
    let said = events(&f, "spend.unknown");
    assert_eq!(said.len(), 1);
    assert!(said[0]["text"]
        .as_str()
        .unwrap()
        .contains("can't be counted toward your spending limits"));
    let day = today(&f.store, crate::now()).unwrap();
    assert_eq!((day.usd, day.unknown_turns), (0.0, 3));
}

#[test]
fn the_daily_total_spans_tasks_resets_each_day_and_can_be_raised() {
    let a = fixture(None);
    let config = SpendingConfig {
        task_usd: None,
        daily_usd: Some(1.0),
    };
    let now = crate::now();
    a.meter
        .record(&a.store, &paid(), &turn(Some(0.6), false))
        .unwrap();
    // A second task on the same profile adds to the same day.
    let (sender, _) = tokio::sync::broadcast::channel(8);
    let other = Meter::new(
        "job-2",
        TaskEvents {
            store: a.store.clone(),
            session_id: a.session.clone(),
            task_id: "task-2".into(),
            sender,
        },
        None,
    );
    other
        .record(&a.store, &paid(), &turn(Some(0.2), false))
        .unwrap();
    assert_eq!(other.check(&a.store, &config, now).unwrap(), Check::Clear);
    assert_eq!(a.meter.check(&a.store, &config, now).unwrap(), Check::Clear);
    assert_eq!(events(&a, "spend.notice").len(), 1, "once a day");
    other
        .record(&a.store, &paid(), &turn(Some(0.25), false))
        .unwrap();
    assert_eq!(
        a.meter.check(&a.store, &config, now).unwrap(),
        Check::Reached(Kind::Daily, 1.0)
    );
    let prompt = a
        .meter
        .ask(&a.store, &config, Kind::Daily, 1.0, now)
        .unwrap();
    assert_eq!(prompt.raise_to, 2.0);
    assert!(prompt.resets_at.is_some_and(|at| at > now));
    assert!(prompt.to_json()["continue_label"]
        .as_str()
        .unwrap()
        .contains("today's limit raised to $2.00"));
    a.meter.decide(&a.store, &prompt.id, "continue").unwrap();
    let day = today(&a.store, now).unwrap();
    assert_eq!(day.raised_to, Some(2.0));
    assert_eq!(daily_limit(&config, &day), Some(2.0));
    // Both tasks see the raised limit.
    assert_eq!(other.check(&a.store, &config, now).unwrap(), Check::Clear);
    // A stored total from another day starts again at zero.
    a.store
        .set_native_meta(
            DAY_KEY,
            &json!({"day":"2001-01-01","usd":99.0,"raised_to":500.0}).to_string(),
        )
        .unwrap();
    let fresh = today(&a.store, now).unwrap();
    assert_eq!((fresh.usd, fresh.raised_to), (0.0, None));
    assert_eq!(a.meter.check(&a.store, &config, now).unwrap(), Check::Clear);
}

#[test]
fn a_waiting_card_is_lifted_when_the_limit_no_longer_applies() {
    let f = fixture(None);
    let config = SpendingConfig::default();
    let now = crate::now();
    f.meter
        .record(&f.store, &paid(), &turn(Some(1.5), false))
        .unwrap();
    let prompt = f
        .meter
        .ask(&f.store, &config, Kind::Task, 1.0, now)
        .unwrap();
    f.meter.lift("setting_changed").unwrap();
    assert!(f.meter.pending().is_none());
    let resolved = events(&f, "spend.limit_resolved");
    assert_eq!(resolved[0]["prompt_id"], prompt.id.as_str());
    assert_eq!(resolved[0]["reason"], "setting_changed");
    // Lifting with nothing waiting says nothing.
    f.meter.lift("setting_changed").unwrap();
    assert_eq!(events(&f, "spend.limit_resolved").len(), 1);
}

#[test]
fn a_card_for_a_limit_that_no_longer_blocks_is_replaced() {
    let f = fixture(None);
    let config = SpendingConfig {
        task_usd: Some(1.0),
        daily_usd: Some(1.0),
    };
    let now = crate::now();
    f.meter
        .record(&f.store, &paid(), &turn(Some(1.5), false))
        .unwrap();
    let task = f
        .meter
        .ask(&f.store, &config, Kind::Task, 1.0, now)
        .unwrap();
    // The per-task limit was raised in Settings; today's limit still blocks.
    let daily = f
        .meter
        .ask(&f.store, &config, Kind::Daily, 1.0, now)
        .unwrap();
    assert_eq!(daily.kind, Kind::Daily);
    assert_ne!(daily.id, task.id);
    let resolved = events(&f, "spend.limit_resolved");
    assert_eq!(resolved.len(), 1);
    assert_eq!(resolved[0]["prompt_id"], task.id.as_str());
    assert_eq!(resolved[0]["reason"], "limit_changed");
    assert_eq!(events(&f, "spend.limit_reached").len(), 2);
    // Only the card for the limit that blocks can be answered, and it raises
    // that limit alone.
    assert!(f.meter.decide(&f.store, &task.id, "continue").is_err());
    f.meter.decide(&f.store, &daily.id, "continue").unwrap();
    assert_eq!(f.meter.task_limit(&config), Some(1.0));
    assert_eq!(today(&f.store, now).unwrap().raised_to, Some(2.0));
    // A changed setting for the same limit gets a card with the new amount.
    let raised = SpendingConfig {
        task_usd: Some(1.2),
        daily_usd: None,
    };
    let first = f
        .meter
        .ask(&f.store, &config, Kind::Task, 1.0, now)
        .unwrap();
    let second = f
        .meter
        .ask(&f.store, &raised, Kind::Task, 1.2, now)
        .unwrap();
    assert_ne!(first.id, second.id);
    assert_eq!(second.limit, 1.2);
    assert_eq!(f.meter.pending(), Some(second));
}
