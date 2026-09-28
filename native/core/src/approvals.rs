//! Pending tool approvals and the grants a user gives for the rest of a task.
//!
//! A prompt is answered once (Allow / Deny), for the rest of its task
//! ("Allow for this task": the same tool kind, or the same command prefix,
//! for the task's remaining steps), or denied with a note that goes back to
//! the model. Grants live in memory and end with the task (`deny_task`).
use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{broadcast, oneshot};
use tokio_util::sync::CancellationToken;

pub mod preview;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Approval {
    pub id: String,
    pub session_id: String,
    pub task_id: String,
    pub tool: String,
    pub arguments: Value,
    pub command: String,
    pub reason: String,
    pub pending: bool,
    pub created_at: f64,
    pub expires_at: f64,
    /// What the action would do: a diff or new-file content for file
    /// changes, the full command and its folder for commands
    /// (`approvals::preview`). Null when there is nothing to show.
    #[serde(default)]
    pub preview: Value,
    /// What "Allow for this task" would cover, in words; empty when the
    /// action cannot be allowed for the rest of the task.
    #[serde(default)]
    pub grant: String,
    /// A note given with Deny reaches the model.
    #[serde(default)]
    pub note: bool,
}

/// The scope of an "Allow for this task" grant: a tool kind, and for
/// commands the program and its subcommand (`cargo test`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Grant {
    pub kind: String,
    pub prefix: Vec<String>,
    /// Shown on the button: "file edits", "`cargo test` commands".
    pub label: String,
}

/// Programs a command grant never covers: they delete, change ownership or
/// run other programs whose effect the prefix cannot describe. This includes
/// command wrappers (a numeric or option first argument collapses the grant to
/// the wrapper's name, which would then cover whatever program it runs) and
/// script interpreters.
const NEVER_GRANTED: &[&str] = &[
    // Destructive or privilege-changing.
    "rm", "rmdir", "dd", "mkfs", "chmod", "chown", "reboot", "shutdown", "kill", "killall", "sudo",
    "doas", "su", "pkexec", "run0", // Shells and evaluators.
    "sh", "bash", "zsh", "fish", "dash", "ksh", "csh", "tcsh", "eval", "exec", "xargs", "env",
    "find", // Wrappers that run another program named in their arguments.
    "timeout", "nice", "nohup", "stdbuf", "setsid", "command", "time", "busybox", "watch", "chrt",
    "ionice", "setpriv", "unshare", "flock", // Script interpreters.
    "python", "python2", "python3", "node", "nodejs", "deno", "bun", "perl", "ruby", "php", "lua",
    "Rscript",
];

/// `git` options that let it run another program or use another repository, so
/// a `git` grant carrying one cannot be summarized by its subcommand.
const UNSAFE_GIT_OPTIONS: &[&str] = &[
    "-c",
    "--config-env",
    "-C",
    "--git-dir",
    "--work-tree",
    "--exec-path",
    "--namespace",
];

impl Grant {
    pub fn kind(kind: &str, label: &str) -> Self {
        Self {
            kind: kind.into(),
            prefix: Vec::new(),
            label: label.into(),
        }
    }
    /// A command grant covers commands with the same program and first
    /// plain argument. Commands that chain, redirect, substitute, set
    /// variables, run privileged or destroy work are never covered, and
    /// cannot be granted.
    pub fn command(kind: &str, command: &str) -> Option<Self> {
        let command = command.trim();
        if command.is_empty()
            || command.len() > 4000
            || command.contains(['\n', '\r', ';', '&', '|', '`', '>', '<', '(', ')', '$'])
            || crate::permissions::destructive_git(command)
        {
            return None;
        }
        let words: Vec<&str> = command.split_whitespace().collect();
        let program = *words.first()?;
        let base = |word: &str| word.rsplit('/').next().unwrap_or(word).to_owned();
        if program.contains('=')
            || words
                .iter()
                .any(|w| *w == "--privileged" || NEVER_GRANTED.contains(&base(w).as_str()))
        {
            return None;
        }
        // A `git` invocation carrying a config or path override could run
        // another program (e.g. a pager or alias) or reach another repository,
        // which its subcommand does not describe.
        if base(program) == "git"
            && words
                .iter()
                .any(|w| UNSAFE_GIT_OPTIONS.contains(&w.split('=').next().unwrap_or(w)))
        {
            return None;
        }
        let mut prefix = vec![program.to_owned()];
        if let Some(sub) = words.get(1).filter(|w| {
            w.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
                && w.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ':'))
        }) {
            prefix.push((*sub).to_owned());
        }
        Some(Self {
            kind: kind.into(),
            label: format!("`{}` commands", prefix.join(" ")),
            prefix,
        })
    }
}

/// How the user answered a prompt.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Answer {
    pub allow: bool,
    /// Allowed for the rest of the task.
    pub for_task: bool,
    /// The note given with Deny.
    pub note: Option<String>,
    /// Allowed by an earlier "Allow for this task", without a prompt.
    pub automatic: bool,
}

impl Answer {
    pub fn allow() -> Self {
        Self {
            allow: true,
            ..Self::default()
        }
    }
    pub fn deny() -> Self {
        Self::default()
    }
    /// The tool error the model reads after a denial.
    pub fn denial(&self) -> String {
        match self.note.as_deref() {
            Some(note) => format!(
                "The user denied this action and said: {note}\nDo not retry it unchanged; follow their note."
            ),
            None => "Permission was denied, cancelled, or expired".into(),
        }
    }
}

struct Pending {
    record: Approval,
    grant: Option<Grant>,
    answer: oneshot::Sender<Answer>,
}
type PendingMap = Arc<Mutex<HashMap<String, Pending>>>;
type GrantMap = Arc<Mutex<HashMap<String, Vec<Grant>>>>;

#[derive(Clone, Default)]
pub struct ApprovalHub {
    pending: PendingMap,
    grants: GrantMap,
    /// Where `approval.expiring` warnings go (the engine's broadcast).
    notices: Option<broadcast::Sender<Value>>,
}
struct Ticket {
    id: String,
    pending: PendingMap,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if let Ok(mut map) = self.pending.lock() {
            map.remove(&self.id);
        }
    }
}
/// Deny notes are kept short; they are one message to the model.
const MAX_NOTE: usize = 2000;

/// When a pending approval warns that it will soon be denied: after 80% of
/// its time (8 of 10 minutes). Timeouts under a minute get no warning.
pub fn warning_delay(timeout: Duration) -> Option<Duration> {
    (timeout >= Duration::from_secs(60)).then(|| timeout.mul_f64(0.8))
}
impl ApprovalHub {
    /// A hub that also broadcasts a transient `approval.expiring` event
    /// (never stored) shortly before a pending approval times out.
    pub fn with_notices(notices: broadcast::Sender<Value>) -> Self {
        Self {
            pending: PendingMap::default(),
            grants: GrantMap::default(),
            notices: Some(notices),
        }
    }
    fn warn(&self, record: &Approval) {
        let Some(notices) = &self.notices else {
            return;
        };
        let mut payload = json!({
            "approval_id": record.id,
            "session_id": record.session_id,
            "tool": record.tool,
            "command": record.command,
            "expires_at": record.expires_at,
            "seconds_left": (record.expires_at - crate::now()).max(0.0).round(),
        });
        crate::redaction::redact_value(&mut payload);
        let _ = notices.send(json!({
            "type": "approval.expiring",
            "session_id": record.session_id,
            "task_id": record.task_id,
            "payload": payload,
        }));
    }
    pub fn list(&self, session: Option<&str>) -> Vec<Approval> {
        let mut records: Vec<_> = self
            .pending
            .lock()
            .map(|map| {
                map.values()
                    .filter(|p| session.is_none_or(|s| s == p.record.session_id))
                    .map(|p| p.record.clone())
                    .collect()
            })
            .unwrap_or_default();
        records.sort_by(|a, b| a.created_at.total_cmp(&b.created_at));
        records
    }
    pub fn decide(&self, id: &str, session: &str, approve: bool) -> Result<Approval> {
        self.answer(
            id,
            session,
            if approve {
                Answer::allow()
            } else {
                Answer::deny()
            },
        )
    }
    /// Answer a prompt: once, for the rest of its task, or denied with a
    /// note. "For this task" is refused when the prompt offered no grant.
    pub fn answer(&self, id: &str, session: &str, mut answer: Answer) -> Result<Approval> {
        let mut map = self
            .pending
            .lock()
            .map_err(|_| anyhow::anyhow!("Approval lock poisoned"))?;
        let pending = map
            .get(id)
            .context("Approval expired or was already answered")?;
        ensure!(
            pending.record.session_id == session,
            "Approval belongs to a different session"
        );
        ensure!(pending.record.expires_at > crate::now(), "Approval expired");
        ensure!(
            !answer.for_task || (answer.allow && pending.grant.is_some()),
            "This action can only be allowed once"
        );
        answer.automatic = false;
        answer.note = answer
            .note
            .filter(|_| !answer.allow)
            .map(|note| crate::tools::truncate(note.trim(), MAX_NOTE).to_owned())
            .filter(|note| !note.is_empty());
        let mut pending = map.remove(id).context("Approval no longer exists")?;
        pending.record.pending = false;
        if answer.for_task {
            if let Some(grant) = pending.grant.take() {
                self.grants
                    .lock()
                    .map_err(|_| anyhow::anyhow!("Approval lock poisoned"))?
                    .entry(pending.record.task_id.clone())
                    .or_default()
                    .push(grant);
            }
        }
        pending
            .answer
            .send(answer)
            .map_err(|_| anyhow::anyhow!("Task is no longer waiting for this approval"))?;
        Ok(pending.record)
    }
    /// This task already holds a grant with `grant`'s scope.
    pub fn granted(&self, task_id: &str, grant: &Grant) -> bool {
        self.grants.lock().is_ok_and(|grants| {
            grants.get(task_id).is_some_and(|list| {
                list.iter()
                    .any(|g| g.kind == grant.kind && g.prefix == grant.prefix)
            })
        })
    }
    pub async fn request<F>(
        &self,
        record: Approval,
        timeout: Duration,
        cancel: CancellationToken,
        on_pending: F,
    ) -> Result<bool>
    where
        F: FnOnce(&Approval),
    {
        Ok(self
            .ask(record, None, timeout, cancel, on_pending)
            .await?
            .allow)
    }
    /// Ask the user, unless an earlier "Allow for this task" already covers
    /// `grant` (then `on_pending` is not called and the answer is
    /// `automatic`). `grant` also decides whether the prompt offers the
    /// choice.
    pub async fn ask<F>(
        &self,
        mut record: Approval,
        grant: Option<Grant>,
        timeout: Duration,
        cancel: CancellationToken,
        on_pending: F,
    ) -> Result<Answer>
    where
        F: FnOnce(&Approval),
    {
        ensure!(!cancel.is_cancelled(), "Task cancelled before approval");
        if let Some(grant) = &grant {
            if self.granted(&record.task_id, grant) {
                return Ok(Answer {
                    allow: true,
                    for_task: true,
                    note: None,
                    automatic: true,
                });
            }
        }
        record.id = crate::id();
        record.pending = true;
        record.created_at = crate::now();
        record.expires_at = record.created_at + timeout.as_secs_f64();
        record.grant = grant.as_ref().map(|g| g.label.clone()).unwrap_or_default();
        let (answer, receiver) = oneshot::channel();
        let ticket = Ticket {
            id: record.id.clone(),
            pending: self.pending.clone(),
        };
        self.pending
            .lock()
            .map_err(|_| anyhow::anyhow!("Approval lock poisoned"))?
            .insert(
                record.id.clone(),
                Pending {
                    record: record.clone(),
                    grant,
                    answer,
                },
            );
        on_pending(&record);
        let expiry = tokio::time::sleep(timeout);
        tokio::pin!(expiry);
        let warning = tokio::time::sleep(warning_delay(timeout).unwrap_or(timeout));
        tokio::pin!(warning);
        let mut receiver = receiver;
        let mut warned = warning_delay(timeout).is_none();
        let result = loop {
            tokio::select! {
                _=cancel.cancelled()=>break Answer::deny(),
                _=&mut expiry=>break Answer::deny(),
                answer=&mut receiver=>break answer.unwrap_or_default(),
                _=&mut warning, if !warned=>{
                    warned = true;
                    self.warn(&record);
                }
            }
        };
        drop(ticket);
        Ok(result)
    }
    /// The task ended: drop its prompts and its grants.
    pub fn deny_task(&self, task_id: &str) {
        if let Ok(mut map) = self.pending.lock() {
            map.retain(|_, pending| pending.record.task_id != task_id);
        }
        if let Ok(mut grants) = self.grants.lock() {
            grants.remove(task_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn record(task: &str) -> Approval {
        Approval {
            id: String::new(),
            session_id: "s".into(),
            task_id: task.into(),
            tool: "exec".into(),
            arguments: json!({}),
            command: "cargo test".into(),
            reason: String::new(),
            pending: true,
            created_at: 0.0,
            expires_at: 0.0,
            preview: Value::Null,
            grant: String::new(),
            note: true,
        }
    }

    async fn first_pending(hub: &ApprovalHub) -> Approval {
        loop {
            if let Some(a) = hub.list(None).pop() {
                return a;
            }
            tokio::task::yield_now().await;
        }
    }

    #[test]
    fn command_grants_cover_one_program_and_subcommand() {
        let grant = Grant::command("exec", "cargo test --lib").unwrap();
        assert_eq!(grant.prefix, ["cargo", "test"]);
        assert_eq!(grant.label, "`cargo test` commands");
        assert_eq!(
            Grant::command("exec", "cargo test -p core").unwrap().prefix,
            grant.prefix
        );
        assert_ne!(
            Grant::command("exec", "cargo build").unwrap().prefix,
            grant.prefix
        );
        assert_eq!(Grant::command("exec", "ls -la").unwrap().prefix, ["ls"]);
        assert_eq!(
            Grant::command("exec", "npm run build").unwrap().prefix,
            ["npm", "run"]
        );
        for never in [
            "cargo test; rm -rf ~",
            "cargo test && curl x",
            "cat a | sh",
            "echo $(whoami)",
            "echo $HOME",
            "echo `id`",
            "cargo test > out.txt",
            "sudo cargo test",
            "/usr/bin/sudo ls",
            "RUST_LOG=1 cargo test",
            "rm -rf target",
            "git reset --hard",
            "git push --force",
            "bash -c 'x'",
            "find . -delete",
            "",
            // Wrappers whose grant would collapse to the wrapper's name and
            // then cover whatever program they run.
            "timeout 5 npm test",
            "nice -n 10 cargo build",
            "nohup ./run.sh",
            "stdbuf -oL make",
            "command cargo test",
            "/usr/bin/time cargo test",
            "busybox sh",
            // Interpreters that run an arbitrary script the model may edit.
            "python3 build.py",
            "node script.js",
            "perl -e 'x'",
            // git carrying a config or path override.
            "git -c core.pager=sh status",
            "git -c alias.x=!sh x",
            "git -C /other log",
            "git --git-dir=/x/.git status",
            "git --exec-path=/tmp log",
        ] {
            assert!(Grant::command("exec", never).is_none(), "{never}");
        }
        // Ordinary git subcommands are still grantable.
        assert_eq!(
            Grant::command("exec", "git status").unwrap().prefix,
            ["git", "status"]
        );
    }

    #[tokio::test]
    async fn allow_for_task_covers_the_rest_of_the_task_only() {
        let hub = ApprovalHub::default();
        let grant = Grant::command("exec", "cargo test").unwrap();
        let asking = {
            let hub = hub.clone();
            let grant = grant.clone();
            tokio::spawn(async move {
                hub.ask(
                    record("t1"),
                    Some(grant),
                    Duration::from_secs(5),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap()
            })
        };
        let pending = first_pending(&hub).await;
        assert_eq!(pending.grant, "`cargo test` commands");
        hub.answer(
            &pending.id,
            "s",
            Answer {
                allow: true,
                for_task: true,
                ..Answer::default()
            },
        )
        .unwrap();
        let answer = asking.await.unwrap();
        assert!(answer.allow && answer.for_task && !answer.automatic);
        // The next matching request in the same task is allowed without a
        // prompt; another task, or another command, still asks.
        let again = hub
            .ask(
                record("t1"),
                Grant::command("exec", "cargo test --release"),
                Duration::from_secs(5),
                CancellationToken::new(),
                |_| panic!("must not prompt"),
            )
            .await
            .unwrap();
        assert!(again.allow && again.automatic);
        assert!(!hub.granted("t2", &grant));
        assert!(!hub.granted("t1", &Grant::command("exec", "cargo build").unwrap()));
        // The task ends: its grants go with it.
        hub.deny_task("t1");
        assert!(!hub.granted("t1", &grant));
    }

    #[tokio::test]
    async fn deny_with_note_and_once_only_prompts() {
        let hub = ApprovalHub::default();
        let asking = {
            let hub = hub.clone();
            tokio::spawn(async move {
                hub.ask(
                    record("t1"),
                    None,
                    Duration::from_secs(5),
                    CancellationToken::new(),
                    |_| {},
                )
                .await
                .unwrap()
            })
        };
        let pending = first_pending(&hub).await;
        assert!(pending.grant.is_empty());
        // Without a grant, "for this task" is refused and the prompt stays.
        assert!(hub
            .answer(
                &pending.id,
                "s",
                Answer {
                    allow: true,
                    for_task: true,
                    ..Answer::default()
                }
            )
            .is_err());
        hub.answer(
            &pending.id,
            "s",
            Answer {
                allow: false,
                note: Some("  use the Makefile instead  ".into()),
                ..Answer::default()
            },
        )
        .unwrap();
        let answer = asking.await.unwrap();
        assert!(!answer.allow);
        assert_eq!(answer.note.as_deref(), Some("use the Makefile instead"));
        assert!(answer.denial().contains("use the Makefile instead"));
        assert_eq!(
            Answer::deny().denial(),
            "Permission was denied, cancelled, or expired"
        );
    }
}

#[cfg(test)]
mod expiry_tests {
    use super::*;

    #[test]
    fn vendor_approvals_warn_two_minutes_before_they_expire() {
        assert_eq!(
            warning_delay(Duration::from_secs(600)),
            Some(Duration::from_secs(480))
        );
        assert_eq!(warning_delay(Duration::from_secs(30)), None);
        let (sender, mut receiver) = broadcast::channel(4);
        let hub = ApprovalHub::with_notices(sender);
        let now = crate::now();
        hub.warn(&Approval {
            id: "a1".into(),
            session_id: "s1".into(),
            task_id: "t1".into(),
            tool: "vendor".into(),
            arguments: Value::Null,
            command: "curl -H 'Authorization: Bearer sk-live-0123456789abcdef0123' x".into(),
            reason: String::new(),
            pending: true,
            created_at: now - 480.0,
            expires_at: now + 120.0,
            preview: Value::Null,
            grant: String::new(),
            note: false,
        });
        let event = receiver.try_recv().unwrap();
        assert_eq!(event["type"], "approval.expiring");
        assert_eq!(event["session_id"], "s1");
        let left = event["payload"]["seconds_left"].as_f64().unwrap();
        assert!((118.0..=120.0).contains(&left), "{left}");
        assert!(!event["payload"]["command"]
            .as_str()
            .unwrap()
            .contains("sk-live-0123456789abcdef0123"));
        // A hub without a broadcast (tests, tools) stays quiet.
        ApprovalHub::default().warn(&Approval {
            id: String::new(),
            session_id: String::new(),
            task_id: String::new(),
            tool: String::new(),
            arguments: Value::Null,
            command: String::new(),
            reason: String::new(),
            pending: true,
            created_at: 0.0,
            expires_at: 0.0,
            preview: Value::Null,
            grant: String::new(),
            note: false,
        });
    }
}
