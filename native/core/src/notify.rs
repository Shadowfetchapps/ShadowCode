//! Desktop notifications: which engine events deserve one and what it says.
//! The desktop shell shows them (and decides focus/visibility); the choice
//! lives here so it is tested without a desktop.
//!
//! Settings (`ui` group): `notify` turns them all off; `notify_approval`,
//! `notify_failed`, `notify_limit` and `notify_finished` each default to on;
//! `notify_sound` (off by default) asks the notification server to play its
//! message sound.
use serde_json::{json, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A tool or vendor command waits for approval.
    Approval,
    /// A pending approval will be denied soon (`approval.expiring`).
    ApprovalExpiring,
    Failed,
    /// A subscription reached its plan limit (and maybe continued locally),
    /// or a task waits at a spending limit for paid models.
    Limit,
    Finished,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Prefs {
    pub enabled: bool,
    pub approval: bool,
    pub failed: bool,
    pub limit: bool,
    pub finished: bool,
    pub sound: bool,
}
impl Default for Prefs {
    fn default() -> Self {
        Self {
            enabled: true,
            approval: true,
            failed: true,
            limit: true,
            finished: true,
            sound: false,
        }
    }
}
impl Prefs {
    /// Read from the saved `ui` settings group.
    pub fn from_ui(ui: &Value) -> Self {
        let on = |key: &str, default: bool| ui[key].as_bool().unwrap_or(default);
        Self {
            enabled: on("notify", true),
            approval: on("notify_approval", true),
            failed: on("notify_failed", true),
            limit: on("notify_limit", true),
            finished: on("notify_finished", true),
            sound: on("notify_sound", false),
        }
    }
    fn allows(&self, kind: Kind) -> bool {
        self.enabled
            && match kind {
                Kind::Approval | Kind::ApprovalExpiring => self.approval,
                Kind::Failed => self.failed,
                Kind::Limit => self.limit,
                Kind::Finished => self.finished,
            }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Notice {
    pub kind: Kind,
    pub title: String,
    pub body: String,
    /// The conversation a click opens.
    pub session_id: String,
}

fn clip(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    if line.chars().count() > limit {
        format!("{}…", line.chars().take(limit).collect::<String>())
    } else {
        line.to_owned()
    }
}

/// What an approval asks for, in one line.
fn approval_subject(payload: &Value) -> String {
    let command = payload["command"].as_str().unwrap_or("");
    let subject = if command.trim().is_empty() {
        payload["tool"].as_str().unwrap_or("a tool")
    } else {
        command
    };
    clip(subject, 140)
}

/// The notification an engine event deserves under `prefs`, if any.
pub fn select(event: &Value, prefs: &Prefs) -> Option<Notice> {
    let payload = &event["payload"];
    let session_id = event["session_id"]
        .as_str()
        .or_else(|| payload["session_id"].as_str())
        .unwrap_or("")
        .to_owned();
    let (kind, title, body) = match event["type"].as_str()? {
        "approval.requested" => (
            Kind::Approval,
            "ShadowCode · approval needed".to_owned(),
            format!("Waiting for you: {}", approval_subject(payload)),
        ),
        "approval.expiring" => {
            let minutes = (payload["seconds_left"].as_f64().unwrap_or(120.0) / 60.0)
                .ceil()
                .max(1.0) as u64;
            (
                Kind::ApprovalExpiring,
                "ShadowCode · approval expires soon".to_owned(),
                format!(
                    "Answer within {minutes} minute{} or it will be denied: {}",
                    if minutes == 1 { "" } else { "s" },
                    approval_subject(payload)
                ),
            )
        }
        "limit.fallback" => {
            // `ask: true` is the "ask me" setting; the limit card says it.
            let from = payload["from"].as_str().unwrap_or("The subscription");
            if payload["ok"] == true {
                let to = payload["to"].as_str().unwrap_or("a local model");
                (
                    Kind::Limit,
                    "ShadowCode · plan limit reached".to_owned(),
                    format!("{from} reached its plan limit. Continuing on {to}."),
                )
            } else {
                let reason = payload["reason"].as_str().unwrap_or("");
                (
                    Kind::Limit,
                    "ShadowCode · plan limit reached".to_owned(),
                    if reason.is_empty() {
                        format!("{from} reached its plan limit. Choose another model to continue.")
                    } else {
                        format!("{from} reached its plan limit. {}", clip(reason, 140))
                    },
                )
            }
        }
        "spend.limit_reached" => {
            // The task waits between steps until the card is answered.
            let title = payload["title"]
                .as_str()
                .filter(|t| !t.trim().is_empty())
                .unwrap_or("A task reached a spending limit");
            (
                Kind::Limit,
                "ShadowCode · spending limit reached".to_owned(),
                format!(
                    "{}. Continue or stop it in its conversation.",
                    clip(title, 120)
                ),
            )
        }
        "agent.completed" => {
            if payload["cancelled"] == true {
                return None;
            }
            // A plan limit is told by `limit.fallback`, which knows whether
            // the task continued on a local model.
            if !payload["limit_reached"].is_null() {
                return None;
            }
            let summary = payload["summary"].as_str().unwrap_or("");
            if payload["success"] == true {
                (
                    Kind::Finished,
                    "ShadowCode · task finished".to_owned(),
                    if summary.trim().is_empty() {
                        "Task finished".to_owned()
                    } else {
                        clip(summary, 180)
                    },
                )
            } else {
                (
                    Kind::Failed,
                    "ShadowCode · task failed".to_owned(),
                    if summary.trim().is_empty() {
                        "The task stopped with an error".to_owned()
                    } else {
                        clip(summary, 180)
                    },
                )
            }
        }
        "automation.finished" => {
            // Only automations set to notify. Its task's own
            // `agent.completed` is not shown (see `automation_session`).
            if payload["notify"] != true {
                return None;
            }
            let name = clip(payload["name"].as_str().unwrap_or("Automation"), 80);
            let title = format!("ShadowCode · {name}");
            let summary = payload["summary"].as_str().unwrap_or("");
            match payload["status"].as_str().unwrap_or("") {
                "completed" => (
                    Kind::Finished,
                    title,
                    if summary.trim().is_empty() {
                        "Finished".to_owned()
                    } else {
                        clip(summary, 180)
                    },
                ),
                // Stopped by the user, or ShadowCode was closing.
                "cancelled" => return None,
                "needs_approval" => (
                    Kind::Failed,
                    title,
                    "Stopped because it asked for approval".to_owned(),
                ),
                "timed_out" => (Kind::Failed, title, "Stopped at its time limit".to_owned()),
                _ => {
                    let detail = payload["detail"].as_str().unwrap_or("");
                    (
                        Kind::Failed,
                        title,
                        if detail.trim().is_empty() {
                            "The automation failed".to_owned()
                        } else {
                            format!("Failed: {}", clip(detail, 160))
                        },
                    )
                }
            }
        }
        _ => return None,
    };
    prefs.allows(kind).then_some(Notice {
        kind,
        title,
        body,
        session_id,
    })
}

/// Automation runs are announced by `automation.finished` (when the
/// automation asks for it), not by their task's own `agent.completed`.
/// `automation.started` names the run's conversation; the desktop keeps a
/// short list of them and skips their `agent.completed`.
pub fn automation_session(event: &Value) -> Option<&str> {
    match event["type"].as_str()? {
        "automation.started" | "automation.finished" => event["session_id"].as_str(),
        _ => None,
    }
}

/// Show it only when the window is in the background or another
/// conversation is open.
pub fn should_show(notice: &Notice, focused: bool, visible_session: &str) -> bool {
    !focused || notice.session_id.is_empty() || notice.session_id != visible_session
}

/// A bounded copy of an event for attached windows: enough for `select`,
/// never a transcript payload.
pub fn hint(event: &Value) -> Value {
    let payload = &event["payload"];
    let text = |key: &str, limit: usize| -> Value {
        payload[key]
            .as_str()
            .map(|s| json!(s.chars().take(limit).collect::<String>()))
            .unwrap_or(Value::Null)
    };
    match event["type"].as_str().unwrap_or("") {
        "agent.completed" => json!({
            "summary": payload["summary"].as_str().unwrap_or("Task finished").chars().take(180).collect::<String>(),
            "success": payload["success"],
            "cancelled": payload["cancelled"],
            "limit_reached": if payload["limit_reached"].is_null() { Value::Null } else { json!(true) },
        }),
        "approval.requested" | "approval.expiring" => json!({
            "command": text("command", 300),
            "tool": text("tool", 80),
            "seconds_left": payload["seconds_left"],
        }),
        "spend.limit_reached" => json!({"title": text("title", 120)}),
        "limit.fallback" => json!({
            "ok": payload["ok"],
            "from": text("from", 80),
            "to": text("to", 120),
            "reason": text("reason", 300),
        }),
        "automation.finished" => json!({
            "notify": payload["notify"],
            "name": text("name", 80),
            "status": text("status", 40),
            "summary": text("summary", 180),
            "detail": text("detail", 300),
        }),
        "automation.started" => json!({}),
        _ => Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(kind: &str, payload: Value) -> Value {
        json!({"type": kind, "session_id": "s1", "payload": payload})
    }

    #[test]
    fn approvals_name_the_command() {
        let notice = select(
            &event(
                "approval.requested",
                json!({"command":"cargo test --all\nsecond line","tool":"run_command"}),
            ),
            &Prefs::default(),
        )
        .unwrap();
        assert_eq!(notice.kind, Kind::Approval);
        assert_eq!(notice.body, "Waiting for you: cargo test --all");
        assert_eq!(notice.session_id, "s1");
        let tool_only = select(
            &event(
                "approval.requested",
                json!({"command":"","tool":"write_file"}),
            ),
            &Prefs::default(),
        )
        .unwrap();
        assert!(tool_only.body.ends_with("write_file"));
        let expiring = select(
            &event(
                "approval.expiring",
                json!({"command":"rm -rf build","seconds_left":120}),
            ),
            &Prefs::default(),
        )
        .unwrap();
        assert_eq!(expiring.kind, Kind::ApprovalExpiring);
        assert!(expiring.body.starts_with("Answer within 2 minutes"));
    }

    #[test]
    fn finished_failed_cancelled_and_limits() {
        let prefs = Prefs::default();
        let done = select(
            &event(
                "agent.completed",
                json!({"success":true,"summary":"Fixed the parser"}),
            ),
            &prefs,
        )
        .unwrap();
        assert_eq!(
            (done.kind, done.body.as_str()),
            (Kind::Finished, "Fixed the parser")
        );
        let failed = select(
            &event(
                "agent.completed",
                json!({"success":false,"summary":"Model stream failed"}),
            ),
            &prefs,
        )
        .unwrap();
        assert_eq!(failed.kind, Kind::Failed);
        assert!(select(
            &event("agent.completed", json!({"success":false,"cancelled":true})),
            &prefs
        )
        .is_none());
        // The limit is told once, by limit.fallback.
        assert!(select(
            &event(
                "agent.completed",
                json!({"success":false,"limit_reached":{"vendor":"codex"}})
            ),
            &prefs
        )
        .is_none());
        let continued = select(
            &event(
                "limit.fallback",
                json!({"ok":true,"from":"Codex","to":"Qwen3 8B"}),
            ),
            &prefs,
        )
        .unwrap();
        assert_eq!(continued.kind, Kind::Limit);
        assert!(continued.body.contains("Continuing on Qwen3 8B"));
        let stopped = select(
            &event("limit.fallback", json!({"ok":false,"from":"Codex"})),
            &prefs,
        )
        .unwrap();
        assert!(stopped.body.contains("Choose another model"));
        assert!(select(&event("agent.message", json!({})), &prefs).is_none());
    }

    #[test]
    fn a_task_waiting_at_a_spending_limit_is_announced() {
        let reached = event(
            "spend.limit_reached",
            json!({"title":"This task reached its spending limit","text":"It has spent $1.02 on paid models.","kind":"task"}),
        );
        let notice = select(&reached, &Prefs::default()).unwrap();
        assert_eq!(notice.kind, Kind::Limit);
        assert_eq!(notice.title, "ShadowCode · spending limit reached");
        assert_eq!(
            notice.body,
            "This task reached its spending limit. Continue or stop it in its conversation."
        );
        assert_eq!(notice.session_id, "s1");
        // Attached windows get enough to say the same.
        let hinted =
            json!({"type":"spend.limit_reached","session_id":"s1","payload":hint(&reached)});
        assert_eq!(select(&hinted, &Prefs::default()), Some(notice));
        assert!(hinted["payload"]["text"].is_null());
        let off = Prefs::from_ui(&json!({"notify_limit": false}));
        assert!(select(&reached, &off).is_none());
    }

    #[test]
    fn settings_turn_kinds_off() {
        let ui = json!({"notify": true, "notify_finished": false, "notify_approval": false});
        let prefs = Prefs::from_ui(&ui);
        assert!(!prefs.sound);
        assert!(select(
            &event("agent.completed", json!({"success":true,"summary":"ok"})),
            &prefs
        )
        .is_none());
        assert!(select(&event("approval.expiring", json!({"command":"x"})), &prefs).is_none());
        assert!(select(
            &event("agent.completed", json!({"success":false,"summary":"boom"})),
            &prefs
        )
        .is_some());
        let off = Prefs::from_ui(&json!({"notify": false}));
        assert!(select(
            &event("agent.completed", json!({"success":false,"summary":"boom"})),
            &off
        )
        .is_none());
    }

    #[test]
    fn automations_notify_when_asked() {
        let prefs = Prefs::default();
        let finished = |status: &str, notify: bool| {
            select(
                &event(
                    "automation.finished",
                    json!({"name":"Nightly tests","status":status,"notify":notify,
                           "summary":"All 42 tests passed","detail":"Model stream failed"}),
                ),
                &prefs,
            )
        };
        let done = finished("completed", true).unwrap();
        assert_eq!(done.kind, Kind::Finished);
        assert_eq!(done.title, "ShadowCode · Nightly tests");
        assert_eq!(done.body, "All 42 tests passed");
        assert!(finished("completed", false).is_none());
        assert!(finished("cancelled", true).is_none());
        assert_eq!(
            finished("needs_approval", true).unwrap().body,
            "Stopped because it asked for approval"
        );
        let failed = finished("failed", true).unwrap();
        assert_eq!(failed.kind, Kind::Failed);
        assert_eq!(failed.body, "Failed: Model stream failed");
        let off = Prefs::from_ui(&json!({"notify_failed": false}));
        assert!(select(
            &event(
                "automation.finished",
                json!({"name":"x","status":"timed_out","notify":true})
            ),
            &off
        )
        .is_none());
        let started = event("automation.started", json!({}));
        assert_eq!(automation_session(&started), Some("s1"));
        assert_eq!(
            automation_session(&event("agent.completed", json!({}))),
            None
        );
        // Attached windows get enough to decide.
        let hinted = json!({"type":"automation.finished","session_id":"s1",
            "payload":hint(&event("automation.finished", json!({"name":"n","status":"completed","notify":true,"summary":"ok"})))});
        assert_eq!(select(&hinted, &prefs).unwrap().body, "ok");
    }

    #[test]
    fn only_when_unfocused_or_another_conversation_is_open() {
        let notice = Notice {
            kind: Kind::Finished,
            title: String::new(),
            body: String::new(),
            session_id: "s1".into(),
        };
        assert!(!should_show(&notice, true, "s1"));
        assert!(should_show(&notice, true, "s2"));
        assert!(should_show(&notice, false, "s1"));
    }

    #[test]
    fn hints_are_bounded_and_still_selectable() {
        let big = "x".repeat(10_000);
        let original = event(
            "agent.completed",
            json!({"success":false,"summary":big,"private":"no"}),
        );
        let hinted = json!({"type":"agent.completed","session_id":"s1","payload":hint(&original)});
        assert!(hinted["payload"]["private"].is_null());
        assert_eq!(
            hinted["payload"]["summary"]
                .as_str()
                .unwrap()
                .chars()
                .count(),
            180
        );
        assert_eq!(
            select(&hinted, &Prefs::default()).unwrap().kind,
            Kind::Failed
        );
    }
}
