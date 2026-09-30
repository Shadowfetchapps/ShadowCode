//! Phone notifications through an ntfy server (<https://ntfy.sh> or a
//! self-hosted one).
//!
//! Nothing is sent unless the user entered both a server address and a
//! topic; there is no default server. Messages say what happened and in
//! which project; the task summary or approval request is included only when
//! the user turned on "Include task details". Each message links back to the
//! conversation in the web interface when remote access (or a public
//! address) is set up.
use super::settings::Ntfy;
use crate::notify::{Kind, Notice, Prefs};
use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;

/// Secret-store name of the optional ntfy access token.
pub const TOKEN_SECRET: &str = "SHADOWCODE_NTFY_TOKEN";
/// Most messages sent in one [`BURST_WINDOW`]; more are dropped.
pub const BURST_LIMIT: usize = 20;
pub const BURST_WINDOW: Duration = Duration::from_secs(10 * 60);

/// A server address the user typed: `http(s)://host[:port][/path]`, without
/// credentials, query or fragment. Returns the normalized address without a
/// trailing slash.
pub fn validate_server(text: &str) -> Result<String> {
    let text = text.trim();
    ensure!(!text.is_empty(), "Enter the ntfy server address");
    let url = reqwest::Url::parse(text).context("Enter a full address such as https://ntfy.sh")?;
    ensure!(
        matches!(url.scheme(), "http" | "https"),
        "The ntfy server address must start with https:// or http://"
    );
    ensure!(
        url.host_str().is_some(),
        "The ntfy server address needs a host name"
    );
    ensure!(
        url.username().is_empty() && url.password().is_none(),
        "Put the access token in the token field, not in the address"
    );
    ensure!(
        url.query().is_none() && url.fragment().is_none(),
        "The ntfy server address cannot have a query or fragment"
    );
    Ok(url.as_str().trim_end_matches('/').to_owned())
}

/// ntfy topics are 1–64 letters, digits, `-` or `_`.
pub fn validate_topic(text: &str) -> Result<String> {
    let text = text.trim();
    ensure!(
        !text.is_empty()
            && text.len() <= 64
            && text
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
        "Use 1–64 letters, digits, - or _ for the topic"
    );
    Ok(text.to_owned())
}

/// A hard-to-guess topic name for the "Generate" button.
pub fn random_topic() -> Result<String> {
    let code = super::auth::new_code()?;
    Ok(format!(
        "shadowcode-{}",
        code.chars()
            .filter(char::is_ascii_alphanumeric)
            .take(20)
            .collect::<String>()
    ))
}

/// The per-kind switches of the phone settings, in the shape the shared
/// notification decision (`crate::notify::select`) reads.
pub fn prefs(settings: &Ntfy) -> Prefs {
    Prefs {
        enabled: true,
        approval: settings.events.approval,
        failed: settings.events.failed,
        limit: settings.events.limit,
        finished: settings.events.finished,
        sound: false,
    }
}

/// The JSON message ntfy accepts at its root URL.
pub fn message(
    notice: &Notice,
    settings: &Ntfy,
    project: Option<&str>,
    link_base: Option<&str>,
) -> Value {
    let (tags, priority, fallback) = match notice.kind {
        Kind::Approval => (["raised_hand"], 4, "A task is waiting for your decision."),
        Kind::ApprovalExpiring => (
            ["alarm_clock"],
            5,
            "A pending approval will be denied soon.",
        ),
        Kind::Finished => (["white_check_mark"], 3, "A task finished."),
        Kind::Failed => (["x"], 4, "A task stopped with an error."),
        Kind::Limit => (["hourglass"], 4, "A task reached a plan or spending limit."),
    };
    let title = match project.filter(|p| !p.is_empty()) {
        Some(project) => format!("{} · {project}", notice.title),
        None => notice.title.clone(),
    };
    let body = if settings.details && !notice.body.is_empty() {
        crate::redaction::redact_text(&notice.body).text
    } else {
        fallback.to_owned()
    };
    let mut value = json!({
        "topic": settings.topic,
        "title": title,
        "message": body,
        "tags": tags,
        "priority": priority,
    });
    if let Some(base) = link_base.filter(|_| !notice.session_id.is_empty()) {
        value["click"] = json!(conversation_link(base, &notice.session_id));
    }
    value
}

/// The web interface address that opens one conversation.
pub fn conversation_link(base: &str, session: &str) -> String {
    let session: String = session
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(64)
        .collect();
    format!("{}/#session={session}", base.trim_end_matches('/'))
}

/// Publish one message. The server address comes only from the user's
/// settings; the token (when set) goes in the Authorization header.
pub async fn publish(
    client: &reqwest::Client,
    server: &str,
    token: Option<&str>,
    message: &Value,
) -> Result<()> {
    let server = validate_server(server)?;
    let mut request = client
        .post(format!("{server}/"))
        .timeout(Duration::from_secs(10))
        .json(message);
    if let Some(token) = token.filter(|t| !t.is_empty()) {
        request = request.bearer_auth(token);
    }
    let response = request
        .send()
        .await
        .context("Could not reach the ntfy server")?;
    let status = response.status();
    if !status.is_success() {
        bail!(
            "The ntfy server answered {}{}",
            status.as_u16(),
            match status.as_u16() {
                401 | 403 => " (check the access token)",
                429 => " (too many messages)",
                _ => "",
            }
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(details: bool) -> Ntfy {
        Ntfy {
            server: "https://ntfy.example".into(),
            topic: "shadow-test".into(),
            details,
            ..Ntfy::default()
        }
    }

    #[test]
    fn addresses_and_topics_are_checked() {
        assert_eq!(
            validate_server(" https://ntfy.sh/ ").unwrap(),
            "https://ntfy.sh"
        );
        assert_eq!(
            validate_server("http://10.0.0.2:8080/ntfy").unwrap(),
            "http://10.0.0.2:8080/ntfy"
        );
        for bad in [
            "",
            "ntfy.sh",
            "ftp://ntfy.sh",
            "https://user:pass@ntfy.sh",
            "https://ntfy.sh/?x=1",
            "https://ntfy.sh/#x",
        ] {
            assert!(validate_server(bad).is_err(), "{bad}");
        }
        assert!(validate_topic("my_topic-1").is_ok());
        for bad in ["", "has space", "slash/topic", &"x".repeat(65)] {
            assert!(validate_topic(bad).is_err(), "{bad}");
        }
        let topic = random_topic().unwrap();
        assert!(validate_topic(&topic).is_ok() && topic.len() > 25);
    }

    #[test]
    fn messages_hide_details_unless_asked_and_link_back() {
        let notice = Notice {
            kind: Kind::Approval,
            title: "ShadowCode · approval needed".into(),
            body: "Waiting for you: rm -rf build".into(),
            session_id: "abc123".into(),
        };
        let plain = message(
            &notice,
            &settings(false),
            Some("demo"),
            Some("http://100.64.0.2:7390/"),
        );
        assert_eq!(plain["topic"], "shadow-test");
        assert_eq!(plain["title"], "ShadowCode · approval needed · demo");
        assert_eq!(plain["message"], "A task is waiting for your decision.");
        assert_eq!(plain["click"], "http://100.64.0.2:7390/#session=abc123");
        assert_eq!(plain["priority"], 4);
        let detailed = message(&notice, &settings(true), None, None);
        assert_eq!(detailed["message"], "Waiting for you: rm -rf build");
        assert_eq!(detailed["title"], "ShadowCode · approval needed");
        assert!(detailed.get("click").is_none());
        assert_eq!(
            conversation_link("https://box.ts.net", "a/../b?c"),
            "https://box.ts.net/#session=abc"
        );
    }

    #[test]
    fn phone_switches_feed_the_shared_decision() {
        let mut phone = settings(false);
        phone.events.finished = false;
        let prefs = prefs(&phone);
        let finished = serde_json::json!({"type":"agent.completed","session_id":"s","payload":{"success":true,"summary":"ok"}});
        let failed = serde_json::json!({"type":"agent.completed","session_id":"s","payload":{"success":false,"summary":"boom"}});
        assert!(crate::notify::select(&finished, &prefs).is_none());
        assert_eq!(
            crate::notify::select(&failed, &prefs).unwrap().kind,
            Kind::Failed
        );
    }
}
