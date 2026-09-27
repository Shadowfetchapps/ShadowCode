//! Monotonic, locally observed task timing. Durations are not token throughput
//! or vendor-internal inference measurements; missing observations stay absent.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Instant,
};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Timings {
    pub schema_version: u32,
    pub complete: bool,
    pub total_seconds: f64,
    pub queue_seconds: f64,
    pub active_seconds: Option<f64>,
    pub preparation_seconds: Option<f64>,
    pub runtime_wait_seconds: Option<f64>,
    pub model_load_seconds: Option<f64>,
    pub model_reused: Option<bool>,
    pub model_requests_seconds: Option<f64>,
    pub model_requests: u64,
    /// Request-relative latency to the first nonempty text callback, including
    /// buffered JSON responses. Not TTFT; tool-only responses have no value.
    pub first_text_seconds: Option<f64>,
    pub first_text_request: Option<u64>,
    pub tool_batches_seconds: Option<f64>,
    /// Completion hooks and final evidence refresh, including failed retries.
    pub final_checks_seconds: Option<f64>,
    pub check_process_seconds: Option<f64>,
}

#[derive(Clone, Copy)]
pub enum Section {
    Preparation,
    RuntimeWait,
    ModelLoad,
    ModelRequest,
    Tools,
    FinalChecks,
}
impl Section {
    fn value(self, timings: &mut Timings) -> &mut Option<f64> {
        match self {
            Self::Preparation => &mut timings.preparation_seconds,
            Self::RuntimeWait => &mut timings.runtime_wait_seconds,
            Self::ModelLoad => &mut timings.model_load_seconds,
            Self::ModelRequest => &mut timings.model_requests_seconds,
            Self::Tools => &mut timings.tool_batches_seconds,
            Self::FinalChecks => &mut timings.final_checks_seconds,
        }
    }
}
struct State {
    accepted: Instant,
    admitted: Option<Instant>,
    values: Timings,
    next: u64,
    open: BTreeMap<u64, (Section, Instant)>,
}
#[derive(Clone)]
pub struct Clock(Arc<Mutex<State>>);
impl Default for Clock {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(State {
            accepted: Instant::now(),
            admitted: None,
            values: Timings {
                schema_version: 1,
                ..Default::default()
            },
            next: 0,
            open: BTreeMap::new(),
        })))
    }
}
impl Clock {
    pub fn local_ready(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.values.model_reused = Some(state.values.model_load_seconds.is_none());
    }
    pub fn record_check(&self, receipt: &serde_json::Value) {
        if receipt["kind"] == "configured_check" && receipt["provenance"] == "locally_observed" {
            if let Some(seconds) = receipt["process_seconds"]
                .as_f64()
                .filter(|n| n.is_finite() && *n >= 0.0)
            {
                let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
                *state.values.check_process_seconds.get_or_insert(0.0) += seconds;
            }
        }
    }
    pub fn admit(&self) {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .admitted
            .get_or_insert_with(Instant::now);
    }
    pub fn span(&self, section: Section) -> Span {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let start = Instant::now();
        let id = state.next;
        state.next += 1;
        section.value(&mut state.values).get_or_insert(0.0);
        if matches!(section, Section::ModelRequest) {
            state.values.model_requests += 1;
        }
        let request = state.values.model_requests;
        state.open.insert(id, (section, start));
        Span {
            clock: self.clone(),
            id,
            start,
            request,
            first_text: Mutex::new(None),
        }
    }
    pub fn snapshot(&self, complete: bool) -> Timings {
        self.snapshot_at(Instant::now(), complete)
    }
    fn snapshot_at(&self, now: Instant, complete: bool) -> Timings {
        let state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let mut result = state.values.clone();
        result.complete = complete;
        result.total_seconds = now.duration_since(state.accepted).as_secs_f64();
        result.queue_seconds = state
            .admitted
            .unwrap_or(now)
            .duration_since(state.accepted)
            .as_secs_f64();
        result.active_seconds = state
            .admitted
            .map(|at| now.duration_since(at).as_secs_f64());
        for (section, start) in state.open.values() {
            *section.value(&mut result).get_or_insert(0.0) +=
                now.duration_since(*start).as_secs_f64();
        }
        result
    }
}
pub struct Span {
    clock: Clock,
    id: u64,
    start: Instant,
    request: u64,
    first_text: Mutex<Option<f64>>,
}
impl Span {
    pub fn first_text_seconds(&self) -> Option<f64> {
        *self.first_text.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn elapsed_seconds(&self) -> f64 {
        self.start.elapsed().as_secs_f64()
    }
    pub fn text(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        let mut first = self.first_text.lock().unwrap_or_else(|e| e.into_inner());
        if first.is_some() {
            return;
        }
        let seconds = self.start.elapsed().as_secs_f64();
        *first = Some(seconds);
        let mut state = self.clock.0.lock().unwrap_or_else(|e| e.into_inner());
        if state.values.first_text_seconds.is_none() {
            state.values.first_text_seconds = Some(seconds);
            state.values.first_text_request = Some(self.request);
        }
    }
}
impl Drop for Span {
    fn drop(&mut self) {
        let mut state = self.clock.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((section, start)) = state.open.remove(&self.id) {
            *section.value(&mut state.values).get_or_insert(0.0) += start.elapsed().as_secs_f64();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    #[test]
    fn only_measured_configured_checks_contribute_process_time() {
        let clock = Clock::default();
        clock.record_check(&serde_json::json!({"kind":"configured_check","provenance":"vendor_reported","process_seconds":5}));
        clock.record_check(&serde_json::json!({"kind":"command","provenance":"locally_observed","process_seconds":5}));
        clock.record_check(&serde_json::json!({"kind":"configured_check","provenance":"locally_observed","process_seconds":null}));
        assert!(clock.snapshot(false).check_process_seconds.is_none());
        clock.record_check(&serde_json::json!({"kind":"configured_check","provenance":"locally_observed","process_seconds":0.25}));
        assert_eq!(clock.snapshot(false).check_process_seconds, Some(0.25));
    }
    #[test]
    fn queue_cancellation_has_no_invented_execution_measurements() {
        let clock = Clock::default();
        let accepted = clock.0.lock().unwrap().accepted;
        let result = clock.snapshot_at(accepted + Duration::from_secs(3), true);
        assert_eq!(result.total_seconds, 3.0);
        assert_eq!(result.queue_seconds, 3.0);
        assert!(result.active_seconds.is_none());
        assert!(result.model_load_seconds.is_none());
        assert!(result.first_text_seconds.is_none());
        assert_eq!(result.model_requests, 0);
    }
    #[test]
    fn failed_and_tool_only_requests_remain_counted_without_fabricating_first_text() {
        let clock = Clock::default();
        clock.admit();
        {
            let _failed_request = clock.span(Section::ModelRequest);
        }
        {
            let request = clock.span(Section::ModelRequest);
            request.text("");
        }
        assert!(clock.snapshot(false).first_text_seconds.is_none());
        {
            let request = clock.span(Section::ModelRequest);
            request.text("answer");
        }
        let result = clock.snapshot(true);
        assert_eq!(result.model_requests, 3);
        assert_eq!(result.first_text_request, Some(3));
        assert!(result.first_text_seconds.unwrap() <= result.model_requests_seconds.unwrap());
        assert!(clock.0.lock().unwrap().open.is_empty());
        assert!(
            result.total_seconds >= result.queue_seconds + result.model_requests_seconds.unwrap()
        );
    }
}
