//! Cancellation scope for the account check owned by a login operation.
//!
//! The scope controls only IO exchange. A probe retains its child outside that
//! future, then explicitly reaps it before returning or publishing an outcome.
//! Task-local scope is intentionally not inherited by independently spawned tasks.
use anyhow::{bail, Context, Result};
use std::{
    future::Future,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{process::Child, time::Instant};
use tokio_util::sync::CancellationToken;

tokio::task_local! {
    static CONTROL: ProbeControl;
}

/// Shared only by a login session's cancellation path and its final cache
/// publication. Neither side awaits while holding this gate. The catalog may
/// acquire it after its state lock; cancellation must not acquire catalog state.
#[derive(Clone, Default)]
pub(crate) struct PublicationGate(Arc<Mutex<()>>);

impl PublicationGate {
    /// Acknowledging cancellation follows any commit already holding the gate.
    /// Recover poison here so even a failed publication cannot prevent Stop.
    pub(crate) fn cancel(&self, cancel: &CancellationToken) {
        let _guard = self.0.lock().unwrap_or_else(|error| error.into_inner());
        cancel.cancel();
    }
}

#[derive(Clone)]
pub(crate) struct ProbeControl {
    cancel: CancellationToken,
    deadline: Instant,
    publication: PublicationGate,
}

pub(crate) fn current() -> Option<ProbeControl> {
    CONTROL.try_with(Clone::clone).ok()
}

#[cfg(test)]
pub(crate) async fn scope<F: Future>(
    cancel: CancellationToken,
    deadline: Instant,
    future: F,
) -> F::Output {
    scope_with_gate(cancel, deadline, PublicationGate::default(), future).await
}

pub(crate) async fn scope_with_gate<F: Future>(
    cancel: CancellationToken,
    deadline: Instant,
    publication: PublicationGate,
    future: F,
) -> F::Output {
    CONTROL
        .scope(
            ProbeControl {
                cancel,
                deadline,
                publication,
            },
            future,
        )
        .await
}

impl ProbeControl {
    /// One synchronous publication decision. An accepted cancellation that
    /// acquired the gate first prevents this commit; a commit that acquired it
    /// first finishes before cancellation is acknowledged. Deadline eligibility
    /// is checked at this point, not before waiting for the catalog state lock.
    pub(crate) fn publish<T>(&self, commit: impl FnOnce() -> T) -> Result<T> {
        let _guard = self
            .publication
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("Account publication gate poisoned"))?;
        self.check()?;
        Ok(commit())
    }

    pub(crate) fn is_stopped(&self) -> bool {
        self.cancel.is_cancelled() || Instant::now() >= self.deadline
    }

    pub(crate) fn check(&self) -> Result<()> {
        if self.cancel.is_cancelled() {
            bail!("Account check cancelled");
        }
        if Instant::now() >= self.deadline {
            bail!("Account check exceeded the original sign-in deadline");
        }
        Ok(())
    }

    /// The caller must retain the child outside `future` and reap it after
    /// this returns. Cancellation never detaches a cleanup task.
    pub(crate) async fn run<T, F>(&self, timeout: Duration, future: F) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        self.check()?;
        let deadline = self.deadline.min(Instant::now() + timeout);
        tokio::select! {
            biased;
            _ = self.cancel.cancelled() => bail!("Account check cancelled"),
            _ = tokio::time::sleep_until(deadline) => bail!("Account check timed out"),
            result = future => result,
        }
    }
}

/// Stop and reap only this exact owned child. No process-group signalling:
/// independently opened browsers and helpers are not owned by this probe.
/// Do not put the wait under the cancelled IO deadline. The login supervisor
/// retains its tracking token if an OS wait takes longer than engine shutdown.
pub(super) async fn reap(child: &mut Child) -> Result<()> {
    if child
        .try_wait()
        .context("Could not observe account-check process")?
        .is_none()
    {
        // Even if the process exits between observation and kill, wait must
        // still run; a kill error alone cannot establish that it was reaped.
        let _ = child.start_kill();
        child
            .wait()
            .await
            .context("Could not reap account-check process")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    #[tokio::test]
    async fn cancelled_or_expired_scope_never_polls_new_probe_io() {
        for expired in [false, true] {
            let cancel = CancellationToken::new();
            if !expired {
                cancel.cancel();
            }
            let deadline = Instant::now()
                + if expired {
                    Duration::ZERO
                } else {
                    Duration::from_secs(5)
                };
            let polled = Arc::new(AtomicBool::new(false));
            scope(cancel, deadline, async {
                let control = current().expect("scope installed");
                assert!(control.is_stopped());
                let result = control
                    .run(Duration::from_secs(8), async {
                        polled.store(true, Ordering::SeqCst);
                        Ok(())
                    })
                    .await;
                assert!(result.is_err());
            })
            .await;
            assert!(!polled.load(Ordering::SeqCst));
        }
        assert!(
            current().is_none(),
            "scope cannot leak to later ordinary probes"
        );
    }
}
