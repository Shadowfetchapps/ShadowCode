use super::{
    args::{ApprovalMode, TaskOptions},
    backend::Backend,
    Outcome,
};
use anyhow::{ensure, Context, Result};
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    io::{IsTerminal, Write},
    time::Duration,
};

/// Strip terminal control sequences from model text and external tool output.
/// JSON output remains lossless because serde escapes the control characters.
pub fn plain(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_control() || matches!(c, '\n' | '\t'))
        .collect()
}
pub use crate::lifecycle::interrupted;
async fn line() -> Result<String> {
    ensure!(
        std::io::stdin().is_terminal(),
        "Interactive approval requires a terminal on stdin"
    );
    // Temporarily use nonblocking reads so Ctrl-C can drop this future without
    // leaving a blocking stdin task that prevents runtime shutdown.
    // SAFETY: fcntl operates on the existing stdin descriptor; no pointer is used.
    let old = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFL) };
    ensure!(old >= 0, "Could not inspect terminal input");
    struct Restore(i32);
    impl Drop for Restore {
        fn drop(&mut self) {
            unsafe {
                libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, self.0);
            }
        }
    }
    let _restore = Restore(old);
    ensure!(
        unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETFL, old | libc::O_NONBLOCK) } >= 0,
        "Could not prepare terminal input"
    );
    let mut bytes = Vec::new();
    loop {
        let mut byte = 0u8;
        // SAFETY: byte is a valid writable one-byte buffer for the duration.
        let count = unsafe { libc::read(libc::STDIN_FILENO, (&mut byte as *mut u8).cast(), 1) };
        if count == 0 {
            return Ok(String::new());
        }
        if count == 1 {
            if byte == b'\n' {
                return Ok(String::from_utf8_lossy(&bytes).trim().into());
            }
            ensure!(bytes.len() < 8000, "Approval response is too long");
            bytes.push(byte);
        } else {
            let error = std::io::Error::last_os_error();
            if !matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) {
                return Err(error.into());
            }
            tokio::time::sleep(Duration::from_millis(30)).await;
        }
    }
}
async fn approve(backend: &Backend, approval: &Value) -> Result<()> {
    errln!(
        "\nApproval required: {}\n{}\n{}",
        plain(approval["tool"].as_str().unwrap_or("tool")),
        plain(approval["command"].as_str().unwrap_or("")),
        plain(approval["reason"].as_str().unwrap_or(""))
    );
    err!("Allow this exact operation? [y/N] ");
    std::io::stderr().flush()?;
    let answer = line().await?;
    let result=backend.call("POST",format!("/api/approvals/{}",approval["id"].as_str().context("Approval ID missing")?),json!({"session_id":approval["session_id"],"decision":if matches!(answer.to_ascii_lowercase().as_str(),"y"|"yes"){"approve"}else{"deny"}})).await;
    if let Err(error) = result {
        errln!("Approval was not applied: {}", plain(&format!("{error:#}")));
    }
    Ok(())
}
/// `Provider busy, retrying (2 of 5) in 4 s…` for a `model.retry` event.
pub fn retry_line(payload: &Value) -> String {
    let wait = payload["delay_ms"].as_u64().unwrap_or(0) as f64 / 1000.0;
    let wait = if wait >= 1.0 {
        format!("{wait:.0} s")
    } else {
        "a moment".into()
    };
    let why = match payload["reason"].as_str().unwrap_or("") {
        "disconnected" | "stalled" | "connect_failed" => "Connection to the provider dropped",
        _ => "Provider busy",
    };
    format!(
        "{why}, retrying ({} of {}) in {wait}…",
        payload["attempt"].as_u64().unwrap_or(1),
        payload["max_attempts"].as_u64().unwrap_or(1)
    )
}

/// What a non-interactive run says when it stops at a spending limit.
pub fn spend_stop_message(card: &Value) -> String {
    let raise = card["raise_to"].as_f64().unwrap_or(0.0);
    let text = card["text"].as_str().unwrap_or("");
    if card["kind"] == "daily" {
        format!(
            "Stopped at the daily spending limit. {text} Raise spending.daily_usd (`shadowcode config spending.daily_usd {raise:.2}`), wait until midnight, or use --interactive to decide in the terminal."
        )
    } else {
        format!(
            "Stopped at the spending limit for one task. {text} Run it again with --max-cost {raise:.2} to allow more, or use --interactive to decide in the terminal."
        )
    }
}

pub async fn job(
    backend: &Backend,
    initial: Value,
    options: &TaskOptions,
    json_output: bool,
    owns_job: bool,
) -> Result<Outcome> {
    let id = initial["id"].as_str().context("Job ID missing")?.to_owned();
    let task_id = initial["task_id"].as_str().unwrap_or("").to_owned();
    let sid = initial["session_id"]
        .as_str()
        .context("Session ID missing")?
        .to_owned();
    let interactive = options.interactive
        || (!json_output
            && !options.events
            && std::io::stdin().is_terminal()
            && std::io::stderr().is_terminal());
    if options.interactive {
        ensure!(
            std::io::stdin().is_terminal(),
            "--interactive requires a terminal on stdin"
        );
    }
    // A freshly started task returns its starting cursor; an existing job may
    // already hold its final cursor. Replay it from history, filtering task IDs,
    // so `jobs --watch` also shows the saved output of completed jobs.
    let mut cursor = if owns_job {
        initial["event_cursor"].as_i64().unwrap_or(0)
    } else {
        0
    };
    let mut streamed = HashSet::new();
    let mut announced = HashSet::new();
    // A spending limit card waiting for an answer (`spend.limit_reached`).
    let mut spend_card: Option<Value> = None;
    let mut text_open = false;
    let signal = interrupted(backend.parent);
    tokio::pin!(signal);
    let result = async {
        loop {
            let page = backend
                .call(
                    "GET",
                    format!("/api/jobs/{id}/events?after={cursor}&limit=512"),
                    Value::Null,
                )
                .await?;
            let rows = page["events"].as_array().context("Job events missing")?;
            for event in rows {
                cursor = cursor.max(event["id"].as_i64().unwrap_or(0));
                if event["task_id"].as_str() != Some(task_id.as_str()) {
                    continue;
                }
                match event["type"].as_str().unwrap_or("") {
                    "spend.limit_reached" => spend_card = Some(event["payload"].clone()),
                    "spend.limit_resolved"
                        if spend_card
                            .as_ref()
                            .is_some_and(|card| card["id"] == event["payload"]["prompt_id"]) =>
                    {
                        spend_card = None
                    }
                    _ => {}
                }
                if options.events {
                    outln!(
                        "{}",
                        serde_json::to_string(&json!({"type":"event","event":event}))?
                    );
                } else if !json_output {
                    let payload = &event["payload"];
                    match event["type"].as_str().unwrap_or("") {
                        "model.stream" => {
                            streamed
                                .insert(payload["message_id"].as_str().unwrap_or("").to_owned());
                            out!("{}", plain(payload["text"].as_str().unwrap_or("")));
                            std::io::stdout().flush()?;
                            text_open = true;
                        }
                        "model.delta" => {
                            if !streamed.contains(payload["message_id"].as_str().unwrap_or("")) {
                                outln!("{}", plain(payload["text"].as_str().unwrap_or("")));
                            } else if text_open {
                                outln!();
                                text_open = false;
                            }
                        }
                        "tool.started" => {
                            if text_open {
                                outln!();
                                text_open = false;
                            }
                            errln!(
                                "→ {} {}",
                                plain(payload["tool"].as_str().unwrap_or("tool")),
                                plain(
                                    payload["arguments"]["command"]
                                        .as_str()
                                        .or(payload["arguments"]["path"].as_str())
                                        .unwrap_or("")
                                )
                            );
                        }
                        "model.retry" => errln!("{}", plain(&retry_line(payload))),
                        "spend.notice" | "spend.unknown" | "spend.limit_resolved" => {
                            if text_open {
                                outln!();
                                text_open = false;
                            }
                            errln!("{}", plain(payload["text"].as_str().unwrap_or("")))
                        }
                        "routing.selected" | "routing.fallback" => errln!(
                            "Model: {} · {}",
                            plain(payload["model_name"].as_str().unwrap_or("")),
                            plain(payload["purpose"].as_str().unwrap_or(""))
                        ),
                        "workflow.selected" => errln!(
                            "Workflow: {} ({})",
                            plain(payload["name"].as_str().unwrap_or("")),
                            plain(payload["path"].as_str().unwrap_or(""))
                        ),
                        "hook.started" | "hook.completed" => errln!(
                            "Hook: {} · {} · {}{}",
                            plain(payload["name"].as_str().unwrap_or("")),
                            plain(payload["event"].as_str().unwrap_or("")),
                            plain(payload["status"].as_str().unwrap_or("")),
                            if payload["success"] == false {
                                format!(
                                    "\n{}\n{}{}",
                                    plain(payload["detail"].as_str().unwrap_or("")),
                                    plain(payload["process"]["stdout"].as_str().unwrap_or("")),
                                    plain(payload["process"]["stderr"].as_str().unwrap_or(""))
                                )
                            } else {
                                String::new()
                            }
                        ),
                        _ => {}
                    }
                }
            }
            let current = &page["job"];
            if !matches!(
                current["status"].as_str(),
                Some("queued" | "running" | "cancelling")
            ) && cursor >= current["event_cursor"].as_i64().unwrap_or(0)
            {
                return Ok(Outcome {
                    code: if current["status"] == "completed" {
                        0
                    } else {
                        1
                    },
                    value: current.clone(),
                    raw: (!json_output && !options.events).then(|| {
                        format!(
                            "{}{} · conversation {} · job {}\n",
                            if text_open { "\n" } else { "" },
                            current["status"].as_str().unwrap_or(""),
                            sid,
                            id
                        )
                    }),
                });
            }
            if rows.len() == 512 {
                continue;
            }
            if let Some(card) = spend_card.clone() {
                let prompt = card["id"].as_str().unwrap_or("").to_owned();
                let decide = |action: &'static str| {
                    let path = format!(
                        "/api/jobs/{}/spending",
                        card["job_id"].as_str().unwrap_or(id.as_str())
                    );
                    let body = json!({"prompt_id": prompt, "action": action});
                    async move { backend.call("POST", path, body).await }
                };
                if interactive {
                    if text_open {
                        outln!();
                        text_open = false;
                    }
                    errln!(
                        "\n{}\n{}",
                        plain(card["title"].as_str().unwrap_or("")),
                        plain(card["text"].as_str().unwrap_or(""))
                    );
                    err!(
                        "{}? [y/N] ",
                        plain(card["continue_label"].as_str().unwrap_or("Continue"))
                    );
                    std::io::stderr().flush()?;
                    let answer = line().await?;
                    let action = if matches!(answer.to_ascii_lowercase().as_str(), "y" | "yes") {
                        "continue"
                    } else {
                        "stop"
                    };
                    if let Err(error) = decide(action).await {
                        errln!(
                            "The answer was not applied: {}",
                            plain(&format!("{error:#}"))
                        );
                    }
                    spend_card = None;
                } else if owns_job && !matches!(options.approval, ApprovalMode::Wait) {
                    // Nobody can answer here: stop cleanly and say how to
                    // allow more. `--approval approve` never raises a limit.
                    let _ = decide("stop").await;
                    let stopped = backend
                        .call("POST", format!("/api/jobs/{id}/cancel"), json!({}))
                        .await?;
                    return Ok(Outcome {
                        code: 2,
                        value: json!({"status":"spending_limit","message":spend_stop_message(&card),"limit":card,"job":stopped}),
                        raw: None,
                    });
                } else if announced.insert(prompt.clone()) && !json_output && !options.events {
                    errln!(
                        "Waiting at the spending limit: {} Answer in the desktop, or run `shadowcode spending --job {} --decision continue` (or stop).",
                        plain(card["text"].as_str().unwrap_or("")),
                        plain(card["job_id"].as_str().unwrap_or(id.as_str()))
                    );
                }
            }
            let approvals = backend
                .call(
                    "GET",
                    format!("/api/approvals?session_id={sid}"),
                    Value::Null,
                )
                .await?;
            for approval in approvals["approvals"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|approval| approval["task_id"] == task_id)
            {
                if interactive {
                    approve(backend, approval).await?;
                } else if owns_job && matches!(options.approval, ApprovalMode::Cancel) {
                    let stopped = backend
                        .call("POST", format!("/api/jobs/{id}/cancel"), json!({}))
                        .await?;
                    return Ok(Outcome {
                        code: 2,
                        value: json!({"status":"needs_approval","message":"Task stopped because this non-interactive invocation cannot grant tool approval. Use --interactive in a terminal, or --approval wait and approve through the desktop/approvals command.","approval":approval,"job":stopped}),
                        raw: None,
                    });
                } else if owns_job && matches!(options.approval, ApprovalMode::Approve) {
                    // Explicitly requested for disposable machines (CI):
                    // grant this task's own request and say so on stderr.
                    if announced.insert(approval["id"].as_str().unwrap_or("").to_owned()) {
                        errln!(
                            "Approved automatically (--approval approve): {} {}",
                            plain(approval["tool"].as_str().unwrap_or("tool")),
                            plain(approval["command"].as_str().unwrap_or(""))
                        );
                        let result = backend
                            .call(
                                "POST",
                                format!(
                                    "/api/approvals/{}",
                                    approval["id"].as_str().context("Approval ID missing")?
                                ),
                                json!({"session_id":approval["session_id"],"decision":"approve"}),
                            )
                            .await;
                        if let Err(error) = result {
                            errln!("Approval was not applied: {}", plain(&format!("{error:#}")));
                        }
                    }
                } else if announced.insert(approval["id"].as_str().unwrap_or("").to_owned())
                    && !json_output
                    && !options.events
                {
                    errln!(
                        "Waiting for tool approval: {}",
                        plain(approval["id"].as_str().unwrap_or(""))
                    );
                }
            }
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
    };
    let outcome: Result<Outcome> = tokio::select! {
        outcome=result=>outcome,
        _=&mut signal=>{
            if owns_job {let stopped=backend.call("POST",format!("/api/jobs/{id}/cancel"),json!({})).await?;Ok(Outcome{code:130,value:stopped,raw:None})}
            else {Ok(Outcome{code:130,value:json!({"status":"detached","job_id":id,"message":"Stopped watching; the existing job continues."}),raw:None})}
        }
    };
    if let Err(error) = outcome {
        if owns_job {
            match tokio::time::timeout(
                Duration::from_secs(20),
                backend.call("POST", format!("/api/jobs/{id}/cancel"), json!({})),
            )
            .await
            {
                Ok(Ok(_)) => {
                    return Err(error.context("Task cancelled after its CLI observer failed"))
                }
                _ => {
                    return Err(error.context(format!(
                        "Could not confirm cancellation; inspect job {id} in the desktop or CLI"
                    )))
                }
            }
        }
        return Err(error);
    }
    outcome
}
pub async fn goal(
    backend: &Backend,
    id: &str,
    interactive: bool,
    json_output: bool,
) -> Result<Outcome> {
    if interactive {
        ensure!(
            std::io::stdin().is_terminal(),
            "--interactive requires a terminal on stdin"
        );
    }
    let signal = interrupted(backend.parent);
    tokio::pin!(signal);
    let progress = async {
        let mut previous = String::new();
        loop {
            let goal = backend
                .call("GET", format!("/api/goals/{id}"), Value::Null)
                .await?;
            if goal["running"] != true {
                return Ok(Outcome {
                    code: if goal["status"] == "completed" { 0 } else { 1 },
                    value: goal,
                    raw: None,
                });
            }
            let detail = goal["run_detail"].as_str().unwrap_or("");
            if detail != previous && !json_output {
                errln!("{}", plain(detail));
                previous = detail.into();
            }
            if let Some(sid) = goal["session_id"].as_str() {
                let approvals = backend
                    .call(
                        "GET",
                        format!("/api/approvals?session_id={sid}"),
                        Value::Null,
                    )
                    .await?;
                let active = if let Some(job) = goal["job_id"].as_str() {
                    backend
                        .call("GET", format!("/api/jobs/{job}"), Value::Null)
                        .await?
                } else {
                    Value::Null
                };
                for approval in approvals["approvals"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|approval| {
                        !active["task_id"].is_null() && approval["task_id"] == active["task_id"]
                    })
                {
                    if interactive || (!json_output && std::io::stdin().is_terminal()) {
                        approve(backend, approval).await?;
                    } else {
                        let paused = backend
                            .call("POST", format!("/api/goals/{id}/pause"), json!({}))
                            .await?;
                        return Ok(Outcome {
                            code: 2,
                            value: json!({"status":"needs_approval","goal":paused,"approval":approval}),
                            raw: None,
                        });
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    let outcome: Result<Outcome> = tokio::select! {result=progress=>result,_=&mut signal=>Ok(Outcome{code:130,value:backend.call("POST",format!("/api/goals/{id}/pause"),json!({})).await?,raw:None})};
    if let Err(error) = outcome {
        return match tokio::time::timeout(
            Duration::from_secs(20),
            backend.call("POST", format!("/api/goals/{id}/pause"), json!({})),
        )
        .await
        {
            Ok(Ok(_)) => Err(error.context("Goal paused after its CLI observer failed")),
            _ => Err(error.context(format!(
                "Could not confirm pause; inspect goal {id} in the desktop or CLI"
            ))),
        };
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_and_spending_lines_read_plainly() {
        assert_eq!(
            retry_line(
                &json!({"attempt":2,"max_attempts":5,"reason":"rate_limited","delay_ms":4000})
            ),
            "Provider busy, retrying (2 of 5) in 4 s…"
        );
        assert_eq!(
            retry_line(
                &json!({"attempt":1,"max_attempts":3,"reason":"disconnected","delay_ms":300})
            ),
            "Connection to the provider dropped, retrying (1 of 3) in a moment…"
        );
        let task =
            json!({"kind":"task","raise_to":2.0,"text":"It has spent $1.04 on paid models."});
        let message = spend_stop_message(&task);
        assert!(message.starts_with("Stopped at the spending limit for one task."));
        assert!(message.contains("--max-cost 2.00"));
        let daily =
            json!({"kind":"daily","raise_to":20.0,"text":"Paid models have cost $10.20 today."});
        assert!(spend_stop_message(&daily).contains("spending.daily_usd 20.00"));
    }

    #[test]
    fn max_cost_is_a_dollar_amount() {
        use clap::Parser;
        let parsed = super::super::args::Options::try_parse_from([
            "shadowcode",
            "run",
            "--max-cost",
            "$0.50",
            "Fix it",
        ])
        .unwrap();
        match parsed.command {
            Some(super::super::args::Command::Run(run)) => {
                assert_eq!(run.options.max_cost, Some(0.5))
            }
            other => panic!("{other:?}"),
        }
        for bad in ["0", "-1", "lots", "1e9"] {
            assert!(super::super::args::Options::try_parse_from([
                "shadowcode",
                "run",
                "--max-cost",
                bad,
                "Fix it"
            ])
            .is_err());
        }
    }
}
