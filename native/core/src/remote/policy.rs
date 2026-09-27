//! What a remote client may ask of the application API, and what it sees.
//!
//! Remote clients hold an owner's access token, so they use the same routes
//! as the window, with these exceptions:
//! - `/api/remote…` (tokens, binding, phone notifications) is managed only on
//!   this computer, so a stolen token cannot mint more tokens, widen the
//!   bind address or turn terminals on.
//! - The app preview (`/api/preview…`) is refused: its proxies listen on this
//!   computer's loopback only, and server detection lists local processes.
//! - Interactive terminals (`/api/terminals…`) and the direct command runner
//!   (`/api/workspace/exec`), including background processes (`/api/background…`),
//!   are refused unless the user turned on "Allow
//!   terminals over remote access". Agent shell commands still go through
//!   the usual approvals.
//! - The computer's microphone (`/api/voice/start`, `recording`, `stop`,
//!   `cancel`) is never switched on remotely; a remote client may still send
//!   its own recording to `/api/voice/transcribe`.
//! - Secret files (`.env`, `secrets.env`, keys…) are not shown, the profile's
//!   own folders cannot be opened as a project, and response bodies pass
//!   through [`redact_response`], which removes recognizable credentials.
//! - A request body that carries a redaction placeholder is refused, so a
//!   redacted value is never written back over the real one.
use crate::{paths::AppPaths, redaction};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub struct Access {
    pub allow_terminals: bool,
}

/// Why a remote request was refused (always HTTP 403).
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal(pub &'static str);

pub const TERMINALS_OFF: &str = "Terminals are turned off for remote access. Turn on \"Allow terminals over remote access\" in Settings › Remote access on the computer running ShadowCode.";
pub const MANAGED_LOCALLY: &str =
    "Remote access is managed in Settings › Remote access on the computer running ShadowCode.";
pub const SECRET_FILE: &str = "Secret files such as .env are not shown over remote access.";
pub const PROFILE_FOLDER: &str =
    "ShadowCode's own settings folder cannot be opened over remote access.";
pub const REDACTED_INPUT: &str =
    "This text contains a hidden secret. Edit it on the computer running ShadowCode.";
pub const MICROPHONE: &str =
    "The microphone of the computer running ShadowCode can't be switched on over remote access.";
pub const INVALID_PATH: &str = "Invalid application command path";
pub const PREVIEW_LOCAL: &str =
    "The app preview works only in the ShadowCode window on the computer running your dev server.";

/// The path's segments after `/api/`, or `None` for a path the router could
/// read differently (encoded characters, empty or dot segments).
pub fn segments(path: &str) -> Option<Vec<&str>> {
    let path = path.split('?').next().unwrap_or(path);
    let rest = path.strip_prefix("/api/")?;
    if path.len() > 4096 || path.contains(['%', '\\', '\0']) {
        return None;
    }
    let parts: Vec<&str> = rest.trim_end_matches('/').split('/').collect();
    if parts
        .iter()
        .any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return None;
    }
    Some(parts)
}

fn query_value(path: &str, key: &str) -> Option<String> {
    let url = reqwest::Url::parse(&format!("http://remote.local{path}")).ok()?;
    url.query_pairs()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

fn contains_placeholder(value: &Value) -> bool {
    match value {
        Value::String(text) => {
            text.contains(redaction::placeholder()) || text.contains(HIDDEN_FILE)
        }
        Value::Array(items) => items.iter().any(contains_placeholder),
        Value::Object(map) => map.values().any(contains_placeholder),
        _ => false,
    }
}

fn expand(text: &str) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match (text, home) {
        ("~", Some(home)) => home,
        (t, Some(home)) if t.starts_with("~/") => home.join(&t[2..]),
        (t, _) => PathBuf::from(t),
    }
}

/// True when `candidate` is one of the profile's folders or inside one.
/// Opening a folder that merely contains the profile (the home folder) is
/// allowed; secret files there are still refused by name.
pub fn inside_profile(candidate: &str, paths: &AppPaths) -> bool {
    if candidate.is_empty() {
        return false;
    }
    let resolve = |p: &Path| p.canonicalize().unwrap_or_else(|_| p.to_path_buf());
    let candidate = resolve(&expand(candidate));
    [&paths.config, &paths.data, &paths.state]
        .into_iter()
        .any(|root| candidate.starts_with(resolve(root)))
}

/// Every string field named `workspace` (the routes that switch projects),
/// plus `path` for the project routes.
fn project_paths<'a>(parts: &[&str], body: &'a Value) -> Vec<&'a str> {
    let mut found = Vec::new();
    if let Some(workspace) = body["workspace"].as_str() {
        found.push(workspace);
    }
    if parts.first() == Some(&"projects") {
        if let Some(path) = body["path"].as_str() {
            found.push(path);
        }
    }
    found
}

/// Decide one remote request. `path` includes its query string.
pub fn check(path: &str, body: &Value, access: &Access, paths: &AppPaths) -> Result<(), Refusal> {
    let parts = segments(path).ok_or(Refusal(INVALID_PATH))?;
    let family = parts.first().copied().unwrap_or("");
    match family {
        "remote" | "views" | "runtime" | "owned-jobs" => return Err(Refusal(MANAGED_LOCALLY)),
        "terminals" | "background" if !access.allow_terminals => {
            return Err(Refusal(TERMINALS_OFF))
        }
        "preview" => return Err(Refusal(PREVIEW_LOCAL)),
        _ => {}
    }
    if parts == ["workspace", "exec"] && !access.allow_terminals {
        return Err(Refusal(TERMINALS_OFF));
    }
    if family == "voice"
        && matches!(
            parts.get(1).copied(),
            Some("start" | "recording" | "stop" | "cancel")
        )
    {
        return Err(Refusal(MICROPHONE));
    }
    // Any route that reads one file by `?path=` (workspace file and diff,
    // a task's review of one file, …).
    if query_value(path, "path").is_some_and(|p| redaction::is_secret_path(&p)) {
        return Err(Refusal(SECRET_FILE));
    }
    if contains_placeholder(body) {
        return Err(Refusal(REDACTED_INPUT));
    }
    if project_paths(&parts, body)
        .into_iter()
        .any(|p| inside_profile(p, paths))
    {
        return Err(Refusal(PROFILE_FOLDER));
    }
    Ok(())
}

/// Fields that carry a file's contents or changes next to its `path`.
const CONTENT_FIELDS: &[&str] = &[
    "content",
    "diff",
    "staged",
    "patch",
    "hunks",
    "staged_hunks",
    "lines",
    "before",
    "after",
    "preview",
    "text",
];
const HIDDEN_FILE: &str = "[secret file hidden over remote access]";

/// Blank the contents of secret files (`.env`, keys…) wherever a response
/// lists them by `path`, and drop their sections from unified diffs.
fn hide_secret_files(value: &mut Value) {
    match value {
        Value::Object(map) => {
            let secret = ["path", "file"]
                .iter()
                .filter_map(|key| map.get(*key).and_then(Value::as_str))
                .any(redaction::is_secret_path);
            for (key, field) in map.iter_mut() {
                if secret && CONTENT_FIELDS.contains(&key.as_str()) {
                    match field {
                        Value::String(text) if !text.is_empty() => {
                            *text = HIDDEN_FILE.to_owned();
                        }
                        Value::Array(items) => items.clear(),
                        _ => {}
                    }
                } else {
                    hide_secret_files(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(hide_secret_files),
        Value::String(text) if text.contains("diff --git ") => {
            if let Some(filtered) = filter_diff(text) {
                *text = filtered;
            }
        }
        _ => {}
    }
}

/// A unified diff without the bodies of secret files, or `None` when it
/// has none.
fn filter_diff(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut hiding = false;
    let mut changed = false;
    for line in text.split_inclusive('\n') {
        if let Some(header) = line.strip_prefix("diff --git ") {
            let target = header.trim_end().rsplit(" b/").next().unwrap_or("");
            hiding = redaction::is_secret_path(target);
            out.push_str(line);
            if hiding {
                changed = true;
                out.push_str(HIDDEN_FILE);
                out.push('\n');
            }
            continue;
        }
        if !hiding {
            out.push_str(line);
        }
    }
    changed.then_some(out)
}

/// Remove secret files' contents and recognizable credentials from a
/// response a remote client will see.
pub fn redact_response(value: &mut Value) {
    hide_secret_files(value);
    redaction::redact_known_secrets(value);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn paths() -> (tempfile::TempDir, AppPaths) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(dir.path()).unwrap();
        (dir, paths)
    }

    #[test]
    fn direct_process_routes_need_the_explicit_switch() {
        let (_dir, paths) = paths();
        let off = Access {
            allow_terminals: false,
        };
        let on = Access {
            allow_terminals: true,
        };
        for path in [
            "/api/terminals",
            "/api/terminals/abc/input",
            "/api/terminals/abc/output?cursor=0",
            "/api/workspace/exec",
            "/api/background",
            "/api/background/task-id",
            "/api/background/task-id/stop",
        ] {
            assert_eq!(
                check(path, &Value::Null, &off, &paths),
                Err(Refusal(TERMINALS_OFF)),
                "{path}"
            );
            assert!(check(path, &Value::Null, &on, &paths).is_ok());
        }
        // Routes that start agent work (tasks, automations, reviews) are
        // allowed: their commands still go through approvals.
        for path in [
            "/api/feed",
            "/api/jobs",
            "/api/automations",
            "/api/automations/a1/run",
            "/api/issues?state=open",
            "/api/review/tasks/t1/undo",
            "/api/approvals/a1",
        ] {
            assert!(check(path, &Value::Null, &off, &paths).is_ok(), "{path}");
        }
    }

    #[test]
    fn the_app_preview_stays_on_this_computer() {
        let (_dir, paths) = paths();
        let on = Access {
            allow_terminals: true,
        };
        for path in ["/api/preview/servers", "/api/preview/open"] {
            assert_eq!(
                check(path, &Value::Null, &on, &paths),
                Err(Refusal(PREVIEW_LOCAL)),
                "{path}"
            );
        }
    }

    #[test]
    fn the_host_microphone_is_never_switched_on_remotely() {
        let (_root, paths) = paths();
        let access = Access {
            allow_terminals: true,
        };
        for path in [
            "/api/voice/start",
            "/api/voice/recording",
            "/api/voice/stop",
            "/api/voice/cancel",
        ] {
            assert_eq!(
                check(path, &Value::Null, &access, &paths),
                Err(Refusal(MICROPHONE)),
                "{path}"
            );
        }
        for path in ["/api/voice/status", "/api/voice/transcribe"] {
            assert!(check(path, &Value::Null, &access, &paths).is_ok(), "{path}");
        }
    }

    #[test]
    fn remote_management_and_ambiguous_paths_are_refused() {
        let (_dir, paths) = paths();
        let on = Access {
            allow_terminals: true,
        };
        for path in [
            "/api/remote",
            "/api/remote/pair",
            "/api/remote/devices/revoke",
            "/api/views",
            "/api/owned-jobs",
        ] {
            assert_eq!(
                check(path, &Value::Null, &on, &paths),
                Err(Refusal(MANAGED_LOCALLY))
            );
        }
        for path in [
            "/api/%74erminals",
            "/api//terminals",
            "/api/./terminals",
            "/api/sessions/../terminals",
            "/other",
            "/api/",
        ] {
            assert_eq!(
                check(path, &Value::Null, &on, &paths),
                Err(Refusal(INVALID_PATH)),
                "{path}"
            );
        }
    }

    #[test]
    fn secret_files_profile_folders_and_placeholders() {
        let (_dir, paths) = paths();
        let access = Access {
            allow_terminals: false,
        };
        assert_eq!(
            check(
                "/api/workspace/file?path=.env",
                &Value::Null,
                &access,
                &paths
            ),
            Err(Refusal(SECRET_FILE))
        );
        assert_eq!(
            check(
                "/api/workspace/file?path=.env",
                &json!({"content":"overwrite","expected_hash":"missing"}),
                &access,
                &paths
            ),
            Err(Refusal(SECRET_FILE))
        );
        assert_eq!(
            check(
                "/api/workspace/diff?path=config%2Fsecrets.env",
                &Value::Null,
                &access,
                &paths
            ),
            Err(Refusal(SECRET_FILE))
        );
        assert!(check(
            "/api/workspace/file?path=src/main.rs",
            &Value::Null,
            &access,
            &paths
        )
        .is_ok());
        let config = paths.config.to_string_lossy().into_owned();
        assert_eq!(
            check("/api/projects", &json!({"path": config}), &access, &paths),
            Err(Refusal(PROFILE_FOLDER))
        );
        assert_eq!(
            check(
                "/api/sessions",
                &json!({"workspace": paths.data.join("x").to_string_lossy()}),
                &access,
                &paths
            ),
            Err(Refusal(PROFILE_FOLDER))
        );
        assert!(check("/api/projects", &json!({"path": "/tmp"}), &access, &paths).is_ok());
        assert_eq!(
            check(
                "/api/workspace/instructions",
                &json!({"content": format!("key {}", redaction::placeholder())}),
                &access,
                &paths
            ),
            Err(Refusal(REDACTED_INPUT))
        );
    }

    #[test]
    fn responses_lose_credentials_but_keep_ids() {
        let github = format!("{}{}", "ghp_", "abcdefghijklmnopqrstuvwxyz012345");
        let mut value = json!({
            "id": "0f3c9a4e2b7d4e1f8a6b5c3d2e1f0a9b",
            "hash": "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
            "api_key_env": "OPENAI_API_KEY",
            "token": "abc-plain-value",
            "tokens": 1234,
            "mcp": {"servers": [{"env": {"GITHUB_TOKEN": github.clone()}}]},
            "text": format!("pushed with {github}"),
        });
        redact_response(&mut value);
        let text = value.to_string();
        assert!(!text.contains(&github));
        assert!(!text.contains("abc-plain-value"));
        assert_eq!(value["id"], "0f3c9a4e2b7d4e1f8a6b5c3d2e1f0a9b");
        assert_eq!(
            value["hash"],
            "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08"
        );
        assert_eq!(value["api_key_env"], "OPENAI_API_KEY");
        assert_eq!(value["tokens"], 1234);
    }

    #[test]
    fn secret_files_are_blanked_in_diffs_and_reviews() {
        let key = format!("{}{}", "sk-test", "abcdefghijklmnopqrstuvwxyz0123");
        let diff = format!(
            "diff --git a/src/a.rs b/src/a.rs\n+fn a() {{}}\ndiff --git a/.env b/.env\n+API={key}\n+PLAIN=hunter2\ndiff --git a/b.txt b/b.txt\n+ok\n"
        );
        let mut value = json!({
            "diff": diff,
            "files": [
                {"path": ".env", "diff": "+PLAIN=hunter2", "hunks": [{"lines": ["+PLAIN=hunter2"]}], "add": 1},
                {"path": "src/a.rs", "diff": "+fn a() {}", "hunks": [{"lines": ["+fn a() {}"]}]},
            ],
            "file": {"path": "keys/server.pem", "content": "-----BEGIN CERT"},
        });
        redact_response(&mut value);
        let text = value.to_string();
        assert!(!text.contains("hunter2"), "{text}");
        assert!(!text.contains(&key));
        assert!(!text.contains("BEGIN CERT"));
        assert!(value["diff"].as_str().unwrap().contains("+fn a() {}"));
        assert!(value["diff"].as_str().unwrap().contains("+ok"));
        assert_eq!(value["files"][0]["hunks"], json!([]));
        assert_eq!(value["files"][0]["add"], 1);
        assert_eq!(value["files"][1]["diff"], "+fn a() {}");
        // A hidden file never goes back.
        let (_dir, paths) = paths();
        let access = Access {
            allow_terminals: false,
        };
        assert_eq!(
            check(
                "/api/workspace/instructions",
                &json!({"content": HIDDEN_FILE}),
                &access,
                &paths
            ),
            Err(Refusal(REDACTED_INPUT))
        );
        assert_eq!(
            check(
                "/api/review/tasks/t1/file?path=.env.local",
                &Value::Null,
                &access,
                &paths
            ),
            Err(Refusal(SECRET_FILE))
        );
    }
}
