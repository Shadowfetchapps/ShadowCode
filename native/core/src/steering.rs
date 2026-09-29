//! Live steering: pause an active job, inject instructions, note manual edits,
//! resume without replaying completed tools, and rewind file checkpoints.
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
};
use tokio::sync::Notify;

#[derive(Default)]
pub struct SteerState {
    parked: bool,
    pub instruction: Option<String>,
    pub file_notes: Vec<String>,
    pub pause_hashes: BTreeMap<String, String>,
    pub rewind_note: Option<String>,
}

pub struct SteerControl {
    paused: AtomicBool,
    notify: Notify,
    state: Mutex<SteerState>,
}

impl Default for SteerControl {
    fn default() -> Self {
        Self {
            paused: AtomicBool::new(false),
            notify: Notify::new(),
            state: Mutex::new(SteerState::default()),
        }
    }
}

pub(crate) struct Parked<'a>(&'a SteerControl);
impl Drop for Parked<'_> {
    fn drop(&mut self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.parked = false;
        }
    }
}
impl SteerControl {
    pub(crate) fn park(&self) -> anyhow::Result<Parked<'_>> {
        self.state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?
            .parked = true;
        Ok(Parked(self))
    }
    pub(crate) fn rewind(
        &self,
        restore: impl FnOnce() -> anyhow::Result<Vec<String>>,
    ) -> anyhow::Result<Vec<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        anyhow::ensure!(self.is_paused() && state.parked,"Wait for the current operation to finish at the pause boundary before rewinding, or stop the task first");
        let paths = restore()?;
        state.rewind_note=Some(format!("File checkpoint restored for {} path(s): {}. Re-read the restored files before editing; do not replay completed commands.",paths.len(),paths.join(", ")));
        Ok(paths)
    }

    pub fn is_paused(&self) -> bool {
        self.paused.load(Ordering::Acquire)
    }

    /// Resume may arrive before the engine reaches a pause boundary. Keep its
    /// note consumable even after the paused flag has already been cleared.
    pub(crate) fn has_pending_resume(&self) -> anyhow::Result<bool> {
        let state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        Ok(state.instruction.is_some()
            || !state.file_notes.is_empty()
            || state.rewind_note.is_some()
            || !state.pause_hashes.is_empty())
    }

    pub fn pause(&self, hashes: BTreeMap<String, String>) -> anyhow::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        state.pause_hashes = hashes;
        self.paused.store(true, Ordering::Release);
        self.notify.notify_waiters();
        Ok(())
    }

    pub fn ensure_pause_hashes(&self, hashes: BTreeMap<String, String>) -> anyhow::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        if state.pause_hashes.is_empty() {
            state.pause_hashes = hashes;
        }
        Ok(())
    }

    pub fn set_instruction(&self, text: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.is_paused(), "Pause the task before steering");
        let trimmed = text.trim();
        anyhow::ensure!(
            !trimmed.is_empty() && trimmed.len() <= 4_000,
            "Steering instruction must be 1–4000 characters"
        );
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        state.instruction = Some(trimmed.to_owned());
        Ok(())
    }

    pub fn note_edit(&self, path: &str, detail: &str) -> anyhow::Result<()> {
        anyhow::ensure!(self.is_paused(), "Pause the task before noting an edit");
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        let note = if detail.trim().is_empty() {
            format!("User noted a manual edit to {path}. Re-read it and incorporate the change.")
        } else {
            format!("User noted a manual edit to {path}: {detail}")
        };
        state.file_notes.push(note);
        Ok(())
    }

    pub fn note_rewind(&self, paths: &[String]) -> anyhow::Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        state.rewind_note = Some(format!(
            "File checkpoint restored for {} path(s): {}. Continue from the restored files; do not undo this rewind unless asked.",
            paths.len(),
            paths.join(", ")
        ));
        Ok(())
    }

    pub fn resume(&self) -> anyhow::Result<()> {
        let _state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        anyhow::ensure!(self.is_paused(), "Task is not paused");
        self.paused.store(false, Ordering::Release);
        self.notify.notify_one();
        Ok(())
    }

    pub fn notify(&self) -> &Notify {
        &self.notify
    }

    pub fn consume_resume(
        &self,
        current_hashes: &BTreeMap<String, String>,
    ) -> anyhow::Result<Option<String>> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| anyhow::anyhow!("Steer lock poisoned"))?;
        Ok(take_resume_messages(&mut state, current_hashes))
    }
}

pub fn take_resume_messages(
    state: &mut SteerState,
    current_hashes: &BTreeMap<String, String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(instruction) = state.instruction.take() {
        parts.push(format!(
            "Live steering instruction from the user:\n{instruction}"
        ));
    }
    for note in state.file_notes.drain(..) {
        parts.push(note);
    }
    if let Some(note) = state.rewind_note.take() {
        parts.push(note);
    }
    let watched = std::mem::take(&mut state.pause_hashes);
    for (path, before) in watched {
        match current_hashes.get(&path) {
            Some(after) if after != &before => {
                parts.push(format!(
                    "File {path} changed while paused (hash {before} → {after}). Re-read it and incorporate the change; do not assume prior contents."
                ));
            }
            None if before != "missing" => {
                parts.push(format!(
                    "File {path} was removed while paused. Treat prior contents as stale."
                ));
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

pub fn steering_system_note(body: &str) -> Value {
    json!({
        "role": "system",
        "content": format!(
            "Steering update (process note, not a new user turn):\n{body}\nFollow this for the remainder of the task unless a later steering note supersedes it."
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steering_text_is_queued_for_next_context() {
        let control = SteerControl::default();
        control.pause(BTreeMap::new()).unwrap();
        control
            .set_instruction("do not refactor schema; use db/v2.sql")
            .unwrap();
        control.resume().unwrap();
        let note = control
            .consume_resume(&BTreeMap::new())
            .unwrap()
            .expect("steering present");
        assert!(note.contains("db/v2.sql"));
        assert!(note.contains("do not refactor schema"));
    }

    #[test]
    fn file_hash_change_is_reported_on_resume() {
        let control = SteerControl::default();
        let mut hashes = BTreeMap::new();
        hashes.insert("db/v2.sql".into(), "aaa".into());
        control.pause(hashes).unwrap();
        control.resume().unwrap();
        let mut current = BTreeMap::new();
        current.insert("db/v2.sql".into(), "bbb".into());
        let note = control.consume_resume(&current).unwrap().unwrap();
        assert!(note.contains("changed while paused"));
        assert!(note.contains("db/v2.sql"));
    }

    #[test]
    fn steer_without_pause_is_rejected() {
        let control = SteerControl::default();
        assert!(control.set_instruction("x").is_err());
    }
}
