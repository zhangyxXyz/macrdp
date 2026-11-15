//! Serializes slow physical-display transitions away from Tokio workers.
//!
//! A native CoreGraphics call cannot safely be aborted halfway through. The
//! blocking worker owns the gate until it returns, even if its async caller is
//! cancelled. Shutdown stops queued work, then drains the same gate before
//! restoring installed guards.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Clone, Default)]
pub struct Coordinator {
    gate: Arc<Mutex<()>>,
    stopping: Arc<AtomicBool>,
}

impl Coordinator {
    pub fn is_stopping(&self) -> bool {
        self.stopping.load(Ordering::Acquire)
    }

    pub fn request_shutdown(&self) {
        self.stopping.store(true, Ordering::Release);
    }

    /// None means shutdown cancelled this operation before it started. An
    /// operation already inside CoreGraphics must finish; it receives the
    /// shutdown flag so an arriving guard can be restored on this worker.
    pub async fn run<R, F>(&self, operation: F) -> Result<Option<R>, tokio::task::JoinError>
    where
        R: Send + 'static,
        F: FnOnce(&AtomicBool) -> R + Send + 'static,
    {
        let gate = self.gate.clone().lock_owned().await;
        let stopping = self.stopping.clone();
        tokio::task::spawn_blocking(move || {
            let _gate = gate;
            if stopping.load(Ordering::Acquire) {
                None
            } else {
                Some(operation(&stopping))
            }
        })
        .await
    }

    pub async fn shutdown<F>(&self, cleanup: F) -> Result<(), tokio::task::JoinError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.request_shutdown();
        let gate = self.gate.clone().lock_owned().await;
        tokio::task::spawn_blocking(move || {
            let _gate = gate;
            cleanup();
        })
        .await
    }
}

/// A cancelled restore retains its guard; absence and successful removal do
/// not. Keeping these outcomes distinct prevents a brief reconnect from making
/// the watcher believe a still-detached physical screen was restored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreOutcome {
    Retained,
    Removed { failed: bool },
    Absent,
}

#[derive(Default)]
pub struct RestoreState {
    pending_failure: bool,
}
impl RestoreState {
    pub fn observe(&mut self, outcome: RestoreOutcome) {
        if let RestoreOutcome::Removed { failed } = outcome {
            self.pending_failure = failed;
        }
    }
    pub fn restart_due(&self, connected: bool) -> bool {
        self.pending_failure && !connected
    }
    pub fn pending_failure(&self) -> bool {
        self.pending_failure
    }
    pub fn acknowledge_failure(&mut self) {
        self.pending_failure = false;
    }
    pub fn reconcile(&self, engaged: bool, connected: bool) -> (bool, bool) {
        if self.restart_due(connected) {
            // Even without a guard (a reconnect may have vanished during
            // debounce), unresolved physical restoration needs the offline
            // grace/recovery branch, never an idle (true, true) no-op.
            (false, true)
        } else {
            reconcile(engaged, connected)
        }
    }
}

/// Reconcile physical state with the latest connection state, not the event
/// that initiated a potentially long native operation.
pub fn reconcile(engaged: bool, connected: bool) -> (bool, bool) {
    (!engaged, engaged != connected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Mutex as StdMutex};
    use std::time::Duration;

    #[test]
    fn latest_state_reconciles_install_restore_and_brief_reconnect() {
        // Install completed after its client left: restore must follow.
        assert_eq!(reconcile(true, false), (false, true));
        // Restore completed after a new client arrived: reinstall must follow.
        assert_eq!(reconcile(false, true), (true, true));
        // A restore was skipped for a reconnect which has already left again:
        // the guard is still engaged and cannot be marked restored.
        let retained = RestoreOutcome::Retained;
        assert_eq!(
            reconcile(retained == RestoreOutcome::Retained, false),
            (false, true)
        );
        assert_eq!(reconcile(true, true), (false, false));
    }

    #[test]
    fn old_restore_failure_waits_for_disconnect_and_survives_an_absent_guard() {
        let mut state = RestoreState::default();
        state.observe(RestoreOutcome::Removed { failed: true });
        assert!(!state.restart_due(true), "never kill a new active session");
        state.observe(RestoreOutcome::Absent); // Reinstall may have failed.
        assert!(!state.restart_due(true));
        assert!(state.restart_due(false), "failure must not be forgotten");
        state.observe(RestoreOutcome::Removed { failed: false });
        assert!(
            !state.restart_due(false),
            "a later successful restore clears it"
        );
    }

    #[test]
    fn failed_restore_then_cancelled_reconnect_still_enters_offline_recovery() {
        let mut state = RestoreState::default();
        state.observe(RestoreOutcome::Removed { failed: true });
        // A new client arrived during restore: enter the install/debounce path.
        assert_eq!(state.reconcile(false, true), (true, true));
        // That client left during debounce OR before the worker started.
        // Both production cancellation paths must enter the disconnect branch.
        assert_eq!(state.reconcile(false, false), (false, true));
        state.observe(RestoreOutcome::Absent);
        assert!(state.restart_due(false));
        // Explicitly disabled restart consumes the failure report once, as
        // before; it must not retry/log forever on a 2.5-second loop.
        state.acknowledge_failure();
        assert_eq!(state.reconcile(false, false), (true, false));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocking_transition_leaves_the_single_async_thread_responsive() {
        let coordinator = Coordinator::default();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker = tokio::spawn(async move {
            coordinator
                .run(move |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).is_ok()
                })
                .await
                .unwrap()
        });
        entered_rx.await.unwrap();
        // This timer and release cannot run if the native call holds the only
        // async thread. The worker timeout turns that regression into a failure.
        tokio::time::sleep(Duration::from_millis(5)).await;
        let _ = release_tx.send(());
        assert_eq!(worker.await.unwrap(), Some(true));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_a_waiter_does_not_allow_overlapping_native_operations() {
        let coordinator = Coordinator::default();
        let events = Arc::new(StdMutex::new(Vec::new()));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let first_coordinator = coordinator.clone();
        let first_events = events.clone();
        let first = tokio::spawn(async move {
            first_coordinator
                .run(move |_| {
                    first_events.lock().unwrap().push("install start");
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    first_events.lock().unwrap().push("install finish");
                })
                .await
        });
        entered_rx.await.unwrap();
        first.abort(); // The native worker must retain the serialization gate.
        let second_events = events.clone();
        let second = tokio::spawn(async move {
            coordinator
                .run(move |_| second_events.lock().unwrap().push("restore"))
                .await
                .unwrap()
        });
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(*events.lock().unwrap(), ["install start"]);
        release_tx.send(()).unwrap();
        assert_eq!(second.await.unwrap(), Some(()));
        assert_eq!(
            *events.lock().unwrap(),
            ["install start", "install finish", "restore"]
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn shutdown_drains_late_install_and_rejects_new_work() {
        let coordinator = Coordinator::default();
        let published = Arc::new(AtomicBool::new(false));
        let restored = Arc::new(AtomicBool::new(false));
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let worker_coordinator = coordinator.clone();
        let worker_published = published.clone();
        let worker_restored = restored.clone();
        let worker = tokio::spawn(async move {
            worker_coordinator
                .run(move |stopping| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv_timeout(Duration::from_secs(2)).unwrap();
                    if stopping.load(Ordering::Acquire) {
                        worker_restored.store(true, Ordering::Release);
                    } else {
                        worker_published.store(true, Ordering::Release);
                    }
                })
                .await
                .unwrap()
        });
        entered_rx.await.unwrap();
        coordinator.request_shutdown();
        let cleanup_coordinator = coordinator.clone();
        let cleanup_published = published.clone();
        let cleanup_restored = restored.clone();
        let cleanup = tokio::spawn(async move {
            cleanup_coordinator
                .shutdown(move || {
                    assert!(cleanup_restored.load(Ordering::Acquire));
                    assert!(!cleanup_published.load(Ordering::Acquire));
                })
                .await
                .unwrap();
        });
        release_tx.send(()).unwrap();
        worker.await.unwrap();
        cleanup.await.unwrap();
        assert_eq!(
            coordinator
                .run(|_| panic!("late install must not run"))
                .await
                .unwrap(),
            None::<()>
        );
    }
}
