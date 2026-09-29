//! Resuming a conversation after a subscription's plan limit resets.
//!
//! When a vendor stops a turn at its plan limit, the reset time comes from
//! the vendor's own usage windows (`resets_at`) or, failing that, from the
//! vendor's error text ("try again at 3:40 PM", "resets in 2h 5m",
//! `…|1759180800`). With a known time the conversation offers "Resume at
//! 3:40 PM": a one-shot continuation on the *same* model, run by the
//! automations scheduler. It is saved in `native_meta` (so it survives a
//! restart), can be cancelled, and never switches to another model: when that
//! model cannot run at the time, the conversation says so instead.
use anyhow::{ensure, Result};
use chrono::{Datelike, Local, NaiveDate, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;

/// `native_meta` key of the scheduled resumes (a JSON list).
pub const KEY: &str = "scheduled_resumes";
/// A resume found this late (ShadowCode was closed at the time) still runs;
/// later than this it is reported as missed instead.
pub const LATE_SECS: f64 = 12.0 * 3600.0;
/// Reset times further away than this are not believed.
const HORIZON_SECS: f64 = 8.0 * 86_400.0;

/// One scheduled continuation of a conversation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Resume {
    pub id: String,
    pub session_id: String,
    pub workspace: PathBuf,
    /// The job that stopped at the limit.
    pub job_id: String,
    pub task_id: String,
    /// The exact picker id to continue on (the limited job's model).
    pub target: String,
    /// "Codex", "Claude Code", …
    pub label: String,
    /// The request that was stopped.
    pub task: String,
    pub mode: String,
    #[serde(default)]
    pub web: bool,
    pub at: f64,
    pub created_at: f64,
    /// The user already agreed to hand the conversation to this cloud route.
    #[serde(default)]
    pub handoff_consent: bool,
}

impl Resume {
    /// The follow-up task's text.
    pub fn continuation(&self) -> String {
        format!(
            "Continue where {} stopped when its plan limit was reached. The request was:\n\n{}",
            self.label, self.task
        )
    }
}

/// Every scheduled resume.
pub fn list(store: &crate::store::Store) -> Result<Vec<Resume>> {
    Ok(store
        .native_meta(KEY)?
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default())
}

/// The scheduled resume of one conversation.
pub fn for_session(store: &crate::store::Store, session_id: &str) -> Result<Option<Resume>> {
    Ok(list(store)?
        .into_iter()
        .find(|r| r.session_id == session_id))
}

/// Save `resume`, replacing the conversation's earlier one.
pub fn save(store: &crate::store::Store, resume: &Resume) -> Result<()> {
    ensure!(resume.at.is_finite(), "Invalid resume time");
    store.update_native_json(KEY, |all: &mut Vec<Resume>| {
        all.retain(|r| r.session_id != resume.session_id);
        all.push(resume.clone());
    })
}

/// Remove and return the conversation's scheduled resume.
pub fn take(store: &crate::store::Store, session_id: &str) -> Result<Option<Resume>> {
    store.update_native_json(KEY, |all: &mut Vec<Resume>| {
        let index = all.iter().position(|r| r.session_id == session_id)?;
        Some(all.remove(index))
    })
}

/// Remove and return every resume due at `now`.
pub fn take_due(store: &crate::store::Store, now: f64) -> Result<Vec<Resume>> {
    store.update_native_json(KEY, |all: &mut Vec<Resume>| {
        let (due, later): (Vec<_>, Vec<_>) = all.drain(..).partition(|r| r.at <= now);
        *all = later;
        due
    })
}

fn plausible(at: f64, now: f64) -> Option<f64> {
    (at.is_finite() && at > now + 30.0 && at < now + HORIZON_SECS).then_some(at)
}

/// When the plan limit resets: the vendor's usage windows first (the
/// latest reset among exhausted windows), then the vendor's error text, then
/// the earliest reported reset. `None` when nothing believable is known.
pub fn reset_time(usage: &Value, detail: &str, now: f64) -> Option<f64> {
    let windows: Vec<(f64, f64)> = usage["windows"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|w| {
            let at = plausible(w["resets_at"].as_f64()?, now)?;
            Some((w["remaining_percent"].as_f64().unwrap_or(100.0), at))
        })
        .collect();
    let exhausted = windows
        .iter()
        .filter(|(left, _)| *left <= 0.5)
        .map(|(_, at)| *at)
        .fold(None, |latest: Option<f64>, at| {
            Some(latest.map_or(at, |l| l.max(at)))
        });
    exhausted.or_else(|| from_text(detail, now)).or_else(|| {
        windows
            .iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, at)| *at)
    })
}

/// A reset time in a vendor's error text, read in this computer's time
/// zone unless the text names UTC.
pub fn from_text(text: &str, now: f64) -> Option<f64> {
    let lower = text.to_ascii_lowercase();
    // Claude Code: "Claude AI usage limit reached|1759180800".
    if let Some((_, tail)) = lower.rsplit_once('|') {
        let digits: String = tail
            .trim()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if digits.len() == 10 {
            if let Some(at) = digits.parse::<f64>().ok().and_then(|at| plausible(at, now)) {
                return Some(at);
            }
        }
    }
    // An RFC 3339 time anywhere in the text.
    for word in lower.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | ',' | '"')) {
        let word = word.trim_end_matches('.');
        if word.len() >= 20 && word.as_bytes().get(4) == Some(&b'-') {
            if let Ok(at) = chrono::DateTime::parse_from_rfc3339(&word.to_ascii_uppercase()) {
                if let Some(at) = plausible(at.timestamp() as f64, now) {
                    return Some(at);
                }
            }
        }
    }
    const CUES: &[&str] = &[
        "try again in",
        "resets in",
        "reset in",
        "available again in",
        "try again at",
        "resets at",
        "reset at",
        "available again at",
        "resets on",
        "resets",
    ];
    for cue in CUES {
        let mut from = 0;
        while let Some(found) = lower[from..].find(cue) {
            let start = from + found + cue.len();
            from = start;
            let rest = lower[start..].trim_start();
            let parsed = if cue.ends_with(" in") {
                duration(rest).and_then(|secs| plausible(now + secs, now))
            } else {
                wall_time(
                    rest.trim_start_matches("at ").trim_start_matches("on "),
                    now,
                )
            };
            if parsed.is_some() {
                return parsed;
            }
        }
    }
    None
}

/// "2 hours 5 minutes", "2h 5m", "45 min", "1 day" → seconds.
fn duration(text: &str) -> Option<f64> {
    let mut total = 0.0;
    let mut found = false;
    let mut words = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty() && *w != "and")
        .peekable();
    while let Some(word) = words.next() {
        let digits: String = word
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if digits.is_empty() {
            break;
        }
        let Ok(value) = digits.parse::<f64>() else {
            break;
        };
        let unit = match &word[digits.len()..] {
            "" => match words.next() {
                Some(unit) => unit.trim_end_matches('.'),
                None => break,
            },
            unit => unit.trim_end_matches('.'),
        };
        let scale = match unit {
            "d" | "day" | "days" => 86_400.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            _ => break,
        };
        total += value * scale;
        found = true;
    }
    found.then_some(total)
}

fn month(word: &str) -> Option<u32> {
    const MONTHS: [&str; 12] = [
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    let word = word.trim_end_matches('.');
    (word.len() >= 3)
        .then(|| MONTHS.iter().position(|m| word.starts_with(m)))
        .flatten()
        .map(|i| i as u32 + 1)
}

/// "3:40 pm", "3pm", "15:40", "sep 30th, 2026 3:40 pm", optionally
/// followed by "utc" → the next such local (or UTC) time after `now`.
fn wall_time(text: &str, now: f64) -> Option<f64> {
    let words: Vec<&str> = text
        .split(|c: char| c.is_whitespace() || c == ',')
        .filter(|w| !w.is_empty())
        .take(6)
        .collect();
    let mut index = 0;
    let mut date: Option<(Option<i32>, u32, u32)> = None;
    if let Some(m) = words.first().and_then(|w| month(w)) {
        let day: String = words
            .get(1)?
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        let day: u32 = day.parse().ok()?;
        index = 2;
        let year = words
            .get(2)
            .filter(|w| w.len() == 4 && w.chars().all(|c| c.is_ascii_digit()))
            .and_then(|w| w.parse().ok());
        if year.is_some() {
            index = 3;
        }
        if words.get(index) == Some(&"at") {
            index += 1;
        }
        date = Some((year, m, day));
    }
    let word = *words.get(index)?;
    let (clock, mut suffix) = match word.find(|c: char| c.is_ascii_alphabetic()) {
        Some(split) => (&word[..split], word[split..].to_owned()),
        None => (word, String::new()),
    };
    if suffix.is_empty() {
        if let Some(next) = words.get(index + 1) {
            if matches!(next.trim_end_matches('.'), "am" | "pm" | "a.m" | "p.m") {
                suffix = next.to_string();
                index += 1;
            }
        }
    }
    let suffix = suffix.replace('.', "");
    let (hour, minute) = match clock.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.get(..2)?.parse::<u32>().ok()?),
        None if !suffix.is_empty() => (clock.parse::<u32>().ok()?, 0),
        // A bare number is not a time.
        None => return None,
    };
    let hour = match suffix.as_str() {
        "am" if (1..=12).contains(&hour) => hour % 12,
        "pm" if (1..=12).contains(&hour) => hour % 12 + 12,
        "" if hour < 24 => hour,
        _ => return None,
    };
    let time = NaiveTime::from_hms_opt(hour, minute, 0)?;
    let utc = words.get(index + 1).is_some_and(|w| {
        matches!(
            w.trim_matches(|c| c == '(' || c == ')' || c == '.'),
            "utc" | "gmt"
        )
    });
    let today = Local.timestamp_opt(now as i64, 0).earliest()?.date_naive();
    let to_ts = |day: NaiveDate| -> Option<f64> {
        let at = day.and_time(time);
        let ts = if utc {
            chrono::Utc.from_utc_datetime(&at).timestamp()
        } else {
            Local.from_local_datetime(&at).earliest()?.timestamp()
        };
        Some(ts as f64)
    };
    match date {
        Some((explicit, m, day)) => {
            let year = explicit.unwrap_or(today.year());
            let mut at = to_ts(NaiveDate::from_ymd_opt(year, m, day)?)?;
            if at <= now && explicit.is_none() {
                at = to_ts(NaiveDate::from_ymd_opt(year + 1, m, day)?)?;
            }
            plausible(at, now)
        }
        None => {
            let mut at = to_ts(today)?;
            if at <= now + 30.0 {
                at = to_ts(today.succ_opt()?)?;
            }
            plausible(at, now)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> f64 {
        Local
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .earliest()
            .unwrap()
            .timestamp() as f64
    }

    #[test]
    fn windows_win_and_the_latest_exhausted_reset_is_used() {
        let now = 1_000_000.0;
        let usage = json!({"windows":[
            {"remaining_percent": 0.0, "resets_at": now + 3600.0},
            {"remaining_percent": 0.0, "resets_at": now + 7200.0},
            {"remaining_percent": 40.0, "resets_at": now + 600.0},
        ]});
        assert_eq!(
            reset_time(&usage, "try again in 5 minutes", now),
            Some(now + 7200.0)
        );
        // No exhausted window: the text, then the earliest reported reset.
        let open = json!({"windows":[{"remaining_percent": 3.0, "resets_at": now + 900.0}]});
        assert_eq!(
            reset_time(&open, "try again in 20 minutes", now),
            Some(now + 1200.0)
        );
        assert_eq!(reset_time(&open, "usage limit", now), Some(now + 900.0));
        // Past or absurd times are not believed.
        let stale = json!({"windows":[{"remaining_percent": 0.0, "resets_at": now - 5.0}]});
        assert_eq!(reset_time(&stale, "", now), None);
        let far =
            json!({"windows":[{"remaining_percent": 0.0, "resets_at": now + 90.0 * 86400.0}]});
        assert_eq!(reset_time(&far, "", now), None);
    }

    #[test]
    fn reads_vendor_error_text() {
        let now = local(2026, 9, 29, 12, 0);
        assert_eq!(
            from_text("Claude AI usage limit reached|1790000000", 1_789_990_000.0),
            Some(1_790_000_000.0)
        );
        assert_eq!(
            from_text(
                "You've hit your usage limit. Try again in 2 hours 5 minutes.",
                now
            ),
            Some(now + 7500.0)
        );
        assert_eq!(
            from_text("Limit reached, resets in 4h 12m", now),
            Some(now + 15_120.0)
        );
        assert_eq!(
            from_text("You've hit your usage limit. Try again at 3:40 PM.", now),
            Some(local(2026, 9, 29, 15, 40))
        );
        assert_eq!(
            from_text("5-hour limit reached ∙ resets 3pm", now),
            Some(local(2026, 9, 29, 15, 0))
        );
        // A time already past today means tomorrow.
        assert_eq!(
            from_text("try again at 9:15 am", now),
            Some(local(2026, 9, 30, 9, 15))
        );
        assert_eq!(
            from_text("Try again at Oct 1st, 2026 3:40 PM.", now),
            Some(local(2026, 10, 1, 15, 40))
        );
        assert_eq!(
            from_text("quota resets at 2026-09-29T18:00:00Z", now),
            chrono::DateTime::parse_from_rfc3339("2026-09-29T18:00:00Z")
                .ok()
                .map(|t| t.timestamp() as f64)
        );
        assert_eq!(
            from_text("resets at 18:00 UTC", now),
            Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 9, 29, 18, 0, 0)
                    .unwrap()
                    .timestamp() as f64
            )
            .filter(|at| *at > now)
            .or(Some(
                chrono::Utc
                    .with_ymd_and_hms(2026, 9, 30, 18, 0, 0)
                    .unwrap()
                    .timestamp() as f64
            ))
        );
        for vague in [
            "usage limit reached",
            "try again later",
            "resets soon",
            "rate limit reached for 5 requests",
            "try again in a while",
        ] {
            assert_eq!(from_text(vague, now), None, "{vague}");
        }
    }

    #[test]
    fn scheduled_resumes_are_saved_replaced_and_taken_when_due() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::store::Store::open(&dir.path().join("db")).unwrap();
        let resume = |session: &str, at: f64| Resume {
            id: crate::id(),
            session_id: session.into(),
            workspace: "/tmp/p".into(),
            job_id: "j".into(),
            task_id: "t".into(),
            target: "cli:codex".into(),
            label: "Codex".into(),
            task: "Fix the bug".into(),
            mode: "code".into(),
            web: false,
            at,
            created_at: 1.0,
            handoff_consent: false,
        };
        save(&store, &resume("a", 100.0)).unwrap();
        save(&store, &resume("a", 200.0)).unwrap();
        save(&store, &resume("b", 300.0)).unwrap();
        assert_eq!(list(&store).unwrap().len(), 2, "one per conversation");
        assert_eq!(for_session(&store, "a").unwrap().unwrap().at, 200.0);
        assert!(take_due(&store, 150.0).unwrap().is_empty());
        let due = take_due(&store, 250.0).unwrap();
        assert_eq!(due.len(), 1);
        assert_eq!(due[0].session_id, "a");
        assert!(due[0]
            .continuation()
            .starts_with("Continue where Codex stopped"));
        assert_eq!(take(&store, "b").unwrap().unwrap().at, 300.0);
        assert!(take(&store, "b").unwrap().is_none());
        assert!(list(&store).unwrap().is_empty());
    }
}
