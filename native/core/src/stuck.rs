//! Noticing that the agent is stuck: the same command failing the same way
//! again and again, or a file changed back and forth.
//!
//! Identical repeated tool calls and repeated text are handled by the
//! engine's loop checks (`autonomy::runaway_action`). This catches the
//! slower loops: a model that edits, runs the tests, sees the same failure,
//! and tries again; or one that undoes and redoes the same edit. When it
//! fires, the task pauses and the conversation offers Keep going, Give a
//! hint, Try another model or Stop. It fires once per loop, and again only
//! after the task made progress.
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

/// Failures of one command with the same output before it counts as stuck.
pub const SAME_FAILURES: usize = 3;
/// Times a file returns to an earlier version before it counts as stuck.
pub const FILE_RETURNS: usize = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stuck {
    /// `same_failure` or `edit_loop`.
    pub kind: &'static str,
    /// One plain sentence for the card.
    pub text: String,
    pub detail: Value,
}

#[derive(Default)]
pub struct Detector {
    failures: HashMap<(String, u64), usize>,
    versions: HashMap<String, Vec<String>>,
    returns: HashMap<String, usize>,
    fired: HashSet<String>,
}

fn normalize_command(command: &str) -> String {
    command.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// A fingerprint of a failure: the end of its output with numbers (times,
/// ports, line counts) and whitespace evened out.
fn signature(exit: Option<i64>, output: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let tail: String = output
        .chars()
        .rev()
        .take(800)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|c| if c.is_ascii_digit() { '#' } else { c })
        .collect();
    let tail = tail.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    exit.hash(&mut hasher);
    tail.hash(&mut hasher);
    hasher.finish()
}

impl Detector {
    /// A shell command finished.
    pub fn command(
        &mut self,
        command: &str,
        success: bool,
        exit: Option<i64>,
        output: &str,
    ) -> Option<Stuck> {
        let command = normalize_command(command);
        if command.is_empty() {
            return None;
        }
        if success {
            // Progress: this command works now.
            self.failures.retain(|(c, _), _| *c != command);
            self.fired
                .retain(|key| !key.starts_with(&format!("fail:{command}:")));
            return None;
        }
        let key = (command.clone(), signature(exit, output));
        let count = self.failures.entry(key.clone()).or_insert(0);
        *count += 1;
        let fired = format!("fail:{command}:{}", key.1);
        if *count >= SAME_FAILURES && self.fired.insert(fired) {
            let shown = crate::tools::truncate(&command, 120);
            return Some(Stuck {
                kind: "same_failure",
                text: format!(
                    "The agent seems stuck: it ran `{shown}` {count} times and it failed the same way each time."
                ),
                detail: json!({"command": shown, "count": *count}),
            });
        }
        None
    }

    /// A file tool changed `path` to content with hash `hash` (`missing`
    /// when deleted).
    pub fn file(&mut self, path: &str, hash: &str) -> Option<Stuck> {
        let versions = self.versions.entry(path.to_owned()).or_default();
        let returned = versions.len() >= 2
            && versions[..versions.len() - 1].iter().any(|v| v == hash)
            && versions.last().map(String::as_str) != Some(hash);
        versions.push(hash.to_owned());
        if versions.len() > 20 {
            versions.remove(0);
        }
        if !returned {
            return None;
        }
        let count = self.returns.entry(path.to_owned()).or_insert(0);
        *count += 1;
        if *count >= FILE_RETURNS && self.fired.insert(format!("file:{path}")) {
            return Some(Stuck {
                kind: "edit_loop",
                text: format!(
                    "The agent seems stuck: it keeps changing {path} back to an earlier version."
                ),
                detail: json!({"path": path, "count": *count}),
            });
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_failure_three_times_fires_once() {
        let mut d = Detector::default();
        let out = "FAIL src/app.test.ts (12 ms)\nExpected 3, received 4\n";
        assert!(d.command("npm test", false, Some(1), out).is_none());
        // Timings and spacing differ; it is still the same failure.
        assert!(d
            .command("npm  test", false, Some(1), &out.replace("12 ms", "15 ms"))
            .is_none());
        let stuck = d.command("npm test", false, Some(1), out).unwrap();
        assert_eq!(stuck.kind, "same_failure");
        assert_eq!(
            stuck.text,
            "The agent seems stuck: it ran `npm test` 3 times and it failed the same way each time."
        );
        assert!(
            d.command("npm test", false, Some(1), out).is_none(),
            "fires once"
        );
        // A pass is progress; a later loop can fire again.
        assert!(d.command("npm test", true, Some(0), "ok").is_none());
        for _ in 0..2 {
            assert!(d.command("npm test", false, Some(1), out).is_none());
        }
        assert!(d.command("npm test", false, Some(1), out).is_some());
    }

    #[test]
    fn different_failures_are_progress() {
        let mut d = Detector::default();
        for error in [
            "Expected 3, received 4",
            "Expected 3, received 5",
            "TypeError: x is undefined",
        ] {
            assert!(d.command("npm test", false, Some(1), error).is_none());
        }
    }

    #[test]
    fn a_file_going_back_and_forth_fires() {
        let mut d = Detector::default();
        assert!(d.file("src/a.rs", "h1").is_none());
        assert!(d.file("src/a.rs", "h2").is_none());
        assert!(d.file("src/a.rs", "h1").is_none(), "one return is normal");
        let stuck = d.file("src/a.rs", "h2").unwrap();
        assert_eq!(stuck.kind, "edit_loop");
        assert!(stuck.text.contains("src/a.rs"));
        // Steady forward edits never fire.
        let mut d = Detector::default();
        for i in 0..10 {
            assert!(d.file("src/b.rs", &format!("v{i}")).is_none());
        }
    }
}
