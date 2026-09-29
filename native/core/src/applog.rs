//! The app log for bug reports: `<state>/logs/shadowcode.log`
//! (`~/.local/state/shadow-agent/logs/` by default), rotated at 5 MB into
//! `shadowcode.log.1` and `.2`, so it never holds more than about 15 MB.
//!
//! It records events, errors and timings only: which task started or
//! finished, retries, limits, tool names and durations, warnings from the
//! engine. Never prompts, answers, file contents or tool output. Every line
//! passes through [`crate::redaction`] before it is written, and the home
//! folder is written as `~`.
//!
//! Engine code logs with `tracing::{info, warn, error}!`; task events are
//! logged from [`crate::events::TaskEvents::emit`] with an allow-list of
//! fields per event type ([`task_event`]).
use crate::paths::AppPaths;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// A log file is rotated when it reaches this size.
pub const MAX_BYTES: u64 = 5 * 1024 * 1024;
/// Files kept: the current one and two older ones.
pub const FILES: usize = 3;
pub const FILE_NAME: &str = "shadowcode.log";
/// Longest single line; longer text is cut.
const MAX_LINE: usize = 2_000;

struct Sink {
    dir: PathBuf,
    file: Option<fs::File>,
    size: u64,
    home: Option<String>,
}

static SINK: Mutex<Option<Sink>> = Mutex::new(None);
static SUBSCRIBER: std::sync::Once = std::sync::Once::new();

/// The logs folder of a profile.
pub fn dir(paths: &AppPaths) -> PathBuf {
    paths.state.join("logs")
}

/// Start writing this process's log into `paths`' logs folder and route
/// `tracing` events there. Later calls switch the folder (the last profile
/// opened wins). `level` is `logging.level`: error, warn, info or debug.
pub fn init(paths: &AppPaths, level: &str) -> std::io::Result<PathBuf> {
    let dir = dir(paths);
    crate::paths::private_directory(&dir).map_err(std::io::Error::other)?;
    {
        let mut sink = SINK.lock().unwrap_or_else(|e| e.into_inner());
        let same = sink.as_ref().is_some_and(|s| s.dir == dir);
        if !same {
            *sink = Some(Sink {
                dir: dir.clone(),
                file: None,
                size: 0,
                home: std::env::var("HOME").ok().filter(|h| h.len() > 1),
            });
        }
    }
    let max = match level.trim().to_ascii_lowercase().as_str() {
        "error" => tracing::Level::ERROR,
        "warn" | "warning" => tracing::Level::WARN,
        "debug" | "trace" => tracing::Level::DEBUG,
        _ => tracing::Level::INFO,
    };
    SUBSCRIBER.call_once(|| {
        let _ = tracing::subscriber::set_global_default(FileSubscriber { max });
    });
    Ok(dir)
}

fn open(dir: &Path) -> std::io::Result<(fs::File, u64)> {
    let path = dir.join(FILE_NAME);
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    let size = file.metadata().map(|m| m.len()).unwrap_or(0);
    Ok((file, size))
}

/// Move `shadowcode.log` to `.1`, `.1` to `.2`, dropping the oldest.
fn rotate(dir: &Path) {
    let name = |index: usize| {
        if index == 0 {
            dir.join(FILE_NAME)
        } else {
            dir.join(format!("{FILE_NAME}.{index}"))
        }
    };
    let _ = fs::remove_file(name(FILES - 1));
    for index in (0..FILES - 1).rev() {
        let _ = fs::rename(name(index), name(index + 1));
    }
}

/// The text of one line as it is stored: redacted, home folder as `~`, one
/// line, bounded.
pub fn clean(text: &str, home: Option<&str>) -> String {
    let mut text = crate::redaction::redact_text(text).text;
    if let Some(home) = home {
        text = text.replace(home, "~");
    }
    let mut line: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if line.len() > MAX_LINE {
        let mut cut = MAX_LINE;
        while !line.is_char_boundary(cut) {
            cut -= 1;
        }
        line.truncate(cut);
        line.push('…');
    }
    line
}

impl Sink {
    /// Append one finished line, rotating first when it would not fit.
    fn append(&mut self, line: &str) {
        let needed = line.len() as u64;
        if self.file.is_some() && self.size + needed > MAX_BYTES {
            self.file = None;
            rotate(&self.dir);
        }
        if self.file.is_none() {
            let Ok((mut file, mut size)) = open(&self.dir) else {
                return;
            };
            // The file on disk may already be full (an earlier run).
            if size > 0 && size + needed > MAX_BYTES {
                drop(file);
                rotate(&self.dir);
                let Ok(fresh) = open(&self.dir) else {
                    return;
                };
                (file, size) = fresh;
            }
            self.file = Some(file);
            self.size = size;
        }
        if let Some(file) = self.file.as_mut() {
            if file.write_all(line.as_bytes()).is_ok() {
                self.size += needed;
            } else {
                self.file = None;
            }
        }
    }
    fn line(&self, level: &str, target: &str, message: &str) -> String {
        format!(
            "{} {level:<5} {target}: {}\n",
            chrono::Local::now().format("%Y-%m-%dT%H:%M:%S%.3f%:z"),
            clean(message, self.home.as_deref())
        )
    }
}

/// Write one line: `<local time> <LEVEL> <target>: <message>`. Does nothing
/// before [`init`] and never fails the caller.
pub fn write(level: &str, target: &str, message: &str) {
    let mut guard = SINK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(sink) = guard.as_mut() {
        let line = sink.line(level, target, message);
        sink.append(&line);
    }
}

fn scalar(value: &Value) -> Option<String> {
    match value {
        Value::String(text) if !text.is_empty() => {
            let short: String = text.chars().take(160).collect();
            Some(format!("{short:?}"))
        }
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(flag) => Some(flag.to_string()),
        _ => None,
    }
}

/// The fields of a task event that may be logged. Anything not listed here
/// (text, arguments, output, paths) is never written.
fn allowed(kind: &str) -> Option<&'static [&'static str]> {
    Some(match kind {
        "agent.started" => &["job_id", "mode", "model", "native", "vendor_agent"],
        "agent.completed" => &["success", "cancelled"],
        "agent.paused" | "agent.resumed" => &["job_id", "status"],
        "agent.warning" => &["kind", "vendor"],
        "model.retry" => &[
            "attempt",
            "max_attempts",
            "reason",
            "status",
            "delay_ms",
            "retry_after",
        ],
        "model.request_timing" => &[
            "elapsed_seconds",
            "first_text_seconds",
            "success",
            "cancelled",
        ],
        "model.switched" => &["provider", "resumed"],
        "routing.selected" | "routing.fallback" => &["model_id", "provider", "route", "purpose"],
        "tool.completed" => &["tool", "success", "duration_ms", "elapsed_seconds"],
        "limit.reached" => &["vendor", "resets_at"],
        "limit.fallback" => &["ok", "target"],
        "spend.notice" => &["kind", "spent", "limit", "estimated"],
        "spend.limit_reached" => &["kind", "spent", "limit", "raise_to", "estimated"],
        "spend.limit_resolved" => &["kind", "action", "limit", "reason"],
        "spend.unknown" => &["model"],
        "resume.scheduled"
        | "resume.cancelled"
        | "resume.started"
        | "resume.missed"
        | "resume.failed"
        | "resume.needs_consent" => &["at", "target", "job_id"],
        "rules.delivered" => &["vendor", "bytes", "skills", "truncated", "hash"],
        "context.compacted" => &["removed", "kept", "strategy"],
        "local.runtime_ready" => &["model_id", "preparation_seconds"],
        "hook.completed" => &["name", "event", "success", "status"],
        "subagent.started" | "subagent.completed" => &["agent", "status"],
        "automation.started" => &["automation_id", "run_id"],
        _ => return None,
    })
}

/// Log a task event's allowed fields (see [`allowed`]); other events are
/// not logged.
pub fn task_event(kind: &str, task_id: &str, payload: &Value) {
    if SINK.lock().map(|s| s.is_none()).unwrap_or(true) {
        return;
    }
    if let Some(message) = event_message(kind, task_id, payload) {
        write("INFO", "event", &message);
    }
}

/// The logged text of a task event, or `None` for events never logged.
pub fn event_message(kind: &str, task_id: &str, payload: &Value) -> Option<String> {
    let fields = allowed(kind)?;
    let mut message = format!("{kind} task={task_id:?}");
    for field in fields {
        if let Some(value) = scalar(&payload[*field]) {
            message.push_str(&format!(" {field}={value}"));
        }
    }
    Some(message)
}

/// The log files, newest first, with their sizes.
pub fn files(paths: &AppPaths) -> Vec<(String, u64)> {
    let dir = dir(paths);
    (0..FILES)
        .map(|index| {
            if index == 0 {
                FILE_NAME.to_owned()
            } else {
                format!("{FILE_NAME}.{index}")
            }
        })
        .filter_map(|name| {
            let size = fs::metadata(dir.join(&name)).ok()?.len();
            Some((name, size))
        })
        .collect()
}

/// The last lines of the log, at most `max_bytes`, for the diagnostic
/// export.
pub fn tail(paths: &AppPaths, max_bytes: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut total = 0;
    for (name, _) in files(paths) {
        let Ok(text) = fs::read_to_string(dir(paths).join(&name)) else {
            continue;
        };
        for line in text.lines().rev() {
            if total + line.len() + 1 > max_bytes {
                lines.reverse();
                return lines;
            }
            total += line.len() + 1;
            lines.push(line.to_owned());
        }
    }
    lines.reverse();
    lines
}

/// Routes `tracing` events into the log file.
struct FileSubscriber {
    max: tracing::Level,
}

#[derive(Default)]
struct Fields(String);
impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0.insert_str(0, &format!("{value:?}"));
        } else {
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        if field.name() == "message" {
            self.0.insert_str(0, value);
        } else {
            self.0.push_str(&format!(" {}={value:?}", field.name()));
        }
    }
}

impl tracing::Subscriber for FileSubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        // Other crates only when something is wrong.
        let ours = metadata.target().starts_with("shadowcode");
        metadata.level() <= &self.max && (ours || metadata.level() <= &tracing::Level::WARN)
    }
    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }
    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}
    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}
    fn event(&self, event: &tracing::Event<'_>) {
        let mut fields = Fields::default();
        event.record(&mut fields);
        let metadata = event.metadata();
        let target = metadata
            .target()
            .strip_prefix("shadowcode_core::")
            .unwrap_or(metadata.target());
        write(metadata.level().as_str(), target, fields.0.trim());
    }
    fn enter(&self, _: &tracing::span::Id) {}
    fn exit(&self, _: &tracing::span::Id) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn lines_are_redacted_bounded_and_keep_home_private() {
        let secret = "sk-or-v1-0123456789abcdef0123456789abcdef0123456789abcdef";
        let line = clean(
            &format!("failed with key {secret} in /home/ada/project\nsecond line"),
            Some("/home/ada"),
        );
        assert!(!line.contains(secret), "{line}");
        assert!(line.contains("~/project"));
        assert!(!line.contains('\n'));
        assert!(clean(&"x".repeat(5000), None).chars().count() <= MAX_LINE + 1);
        // Job ids survive the entropy check.
        let id = crate::id();
        assert!(clean(&format!("job={id:?}"), None).contains(&id));
    }

    #[test]
    fn only_allowed_fields_of_known_events_are_logged() {
        let tool = event_message(
            "tool.completed",
            "t1",
            &json!({"tool":"read_file","success":true,"output":"SECRET FILE BODY","arguments":{"path":"/etc/x"}}),
        )
        .unwrap();
        assert_eq!(
            tool,
            "tool.completed task=\"t1\" tool=\"read_file\" success=true"
        );
        assert_eq!(
            event_message("model.delta", "t1", &json!({"text":"the model's answer"})),
            None
        );
        let retry = event_message(
            "model.retry",
            "t1",
            &json!({"attempt":2,"max_attempts":5,"reason":"rate_limited","delay_ms":4000}),
        )
        .unwrap();
        assert!(retry.contains("attempt=2 max_attempts=5 reason=\"rate_limited\" delay_ms=4000"));
    }

    #[test]
    fn a_sink_writes_rotates_at_the_cap_and_lists_and_tails_files() {
        let root = tempfile::tempdir().unwrap();
        let paths = AppPaths::isolated(&root.path().join("profile")).unwrap();
        let dir = dir(&paths);
        fs::create_dir_all(&dir).unwrap();
        let mut sink = Sink {
            dir: dir.clone(),
            file: None,
            size: 0,
            home: None,
        };
        let first = sink.line("INFO", "engine", "job.finished status=\"completed\"");
        sink.append(&first);
        assert!(first.contains(" INFO  engine: job.finished"));
        // A full file is rotated before the next line.
        fs::write(dir.join(FILE_NAME), vec![b'x'; MAX_BYTES as usize]).unwrap();
        sink.file = None;
        sink.append("after rotation\n");
        let names: Vec<String> = files(&paths).into_iter().map(|f| f.0).collect();
        assert_eq!(names, [FILE_NAME.to_owned(), format!("{FILE_NAME}.1")]);
        assert_eq!(tail(&paths, 1024), ["after rotation"]);
        assert_eq!(tail(&paths, 10), Vec::<String>::new());
    }

    #[test]
    fn files_rotate_and_at_most_three_are_kept() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        for round in 0..5 {
            fs::write(dir.join(FILE_NAME), format!("round {round}")).unwrap();
            rotate(dir);
        }
        assert!(!dir.join(FILE_NAME).exists());
        assert_eq!(
            fs::read_to_string(dir.join(format!("{FILE_NAME}.1"))).unwrap(),
            "round 4"
        );
        assert_eq!(
            fs::read_to_string(dir.join(format!("{FILE_NAME}.2"))).unwrap(),
            "round 3"
        );
        assert!(!dir.join(format!("{FILE_NAME}.3")).exists());
    }
}
