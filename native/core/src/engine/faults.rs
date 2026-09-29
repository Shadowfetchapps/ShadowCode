//! Deliberate failures inside a job, for the engine's own failure-path tests
//! (the engine fails before the model streams: RUN-02). Only debug builds
//! compile this module; a release package has no way to arm a fault.
use std::sync::Mutex;

/// Where a job fails on purpose. An armed fault fires once, in the next job
/// that reaches it; every point is before the job's first model request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Fault {
    /// Panic right after the job is marked running.
    PanicBeforeStream,
    /// Panic inside a database write, while it holds the connection.
    PanicInStoreWrite,
    /// Panic while holding the job's own live record.
    PanicHoldingJobRecord,
}

#[derive(Default)]
pub(super) struct Faults(Mutex<Vec<Fault>>);

impl Faults {
    pub(super) fn arm(&self, fault: Fault) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(fault);
    }
    /// Disarm and report `fault` when it was armed.
    pub(super) fn take(&self, fault: Fault) -> bool {
        let mut armed = self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match armed.iter().position(|item| *item == fault) {
            Some(index) => {
                armed.remove(index);
                true
            }
            None => false,
        }
    }
}
