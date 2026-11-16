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

/// Topology of displays other than this session's virtual display. Offline is
/// not a restoration failure; online-but-inactive must never be mistaken for it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OtherDisplays {
    None,
    Active,
    Inactive,
}
impl OtherDisplays {
    pub fn classify(active: &[u32], online: &[u32], virtual_id: Option<u32>) -> Self {
        let other = |id: &u32| *id != 0 && Some(*id) != virtual_id;
        if active.iter().any(other) {
            Self::Active
        } else if online.iter().any(other) {
            Self::Inactive
        } else {
            Self::None
        }
    }
    pub fn can_release_virtual(self) -> bool {
        self != Self::Inactive
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
    /// A reconnect can install an empty guard when all previously disabled
    /// screens disappeared from enumeration. Dropping that guard reports no
    /// new failure; it does not prove the earlier failure was repaired.
    pub fn observe_verified(
        &mut self,
        outcome: RestoreOutcome,
        other: Option<OtherDisplays>,
    ) -> RestoreOutcome {
        let outcome = if self.pending_failure
            && outcome == (RestoreOutcome::Removed { failed: false })
            && other != Some(OtherDisplays::Active)
        {
            RestoreOutcome::Removed { failed: true }
        } else {
            outcome
        };
        self.observe(outcome);
        outcome
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DisconnectCompletion {
    Completed,
    Cancelled,
    LockFailed,
}

/// An unresolved failed restore must retain its surface even when CoreGraphics
/// no longer lists the disabled physical screens. A new capture owns its own
/// lease; otherwise only a positively active other display permits release.
pub fn recovery_surface_can_release(connected: bool, other: Option<OtherDisplays>) -> bool {
    connected || matches!(other, Some(OtherDisplays::Active))
}

/// Run irreversible recovery only after the requested lock is confirmed.
/// `acquire` waits for reconnect activity and takes the auto-unlock serial gate,
/// or cancels a superseded disconnect. Its original disconnect/deadline must
/// remain fixed across retries. If a handshake starts during native lock
/// confirmation, release the gate and wait again in this same operation.
/// With locking disabled, `lock` is an explicit successful no-op.
pub fn complete_disconnect<G>(
    mut acquire: impl FnMut() -> Option<G>,
    mut current: impl FnMut() -> bool,
    mut lock: impl FnMut() -> bool,
    recover: impl FnOnce(),
) -> DisconnectCompletion {
    loop {
        let Some(_serial) = acquire() else {
            return DisconnectCompletion::Cancelled;
        };
        if !current() {
            continue;
        }
        if !lock() {
            return DisconnectCompletion::LockFailed;
        }
        if !current() {
            continue;
        }
        recover();
        return DisconnectCompletion::Completed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Mutex as StdMutex};
    use std::time::Duration;

    #[test]
    fn failed_restore_locks_before_committing_recovery() {
        let calls = std::cell::RefCell::new(Vec::new());
        assert_eq!(
            complete_disconnect(
                || Some(()),
                || {
                    calls.borrow_mut().push("current");
                    true
                },
                || {
                    calls.borrow_mut().push("lock confirmed");
                    true
                },
                || calls.borrow_mut().push("cleanup and exit"),
            ),
            DisconnectCompletion::Completed,
        );
        assert_eq!(
            *calls.borrow(),
            ["current", "lock confirmed", "current", "cleanup and exit"]
        );
    }

    #[test]
    fn unconfirmed_lock_never_restarts_or_cleans_up() {
        assert_eq!(
            complete_disconnect(
                || Some(()),
                || true,
                || false,
                || panic!("must preserve running services")
            ),
            DisconnectCompletion::LockFailed,
        );
    }

    #[test]
    fn reconnect_before_or_during_lock_cancels_recovery() {
        assert_eq!(
            complete_disconnect(
                || None::<()>,
                || false,
                || panic!("stale disconnect must not lock"),
                || panic!("must not restart")
            ),
            DisconnectCompletion::Cancelled,
        );
        let epoch = std::cell::Cell::new(7);
        assert_eq!(
            complete_disconnect(
                || (epoch.get() == 7).then_some(()),
                || epoch.get() == 7,
                || {
                    epoch.set(8);
                    true
                },
                || panic!("must not kill the newer session")
            ),
            DisconnectCompletion::Cancelled,
        );
        // Preserve failed restoration so the new session's disconnect owns
        // recovery, even if it finishes entirely between watcher polls.
        let mut restore = RestoreState::default();
        restore.observe(RestoreOutcome::Removed { failed: true });
        assert_eq!(restore.reconcile(false, true), (true, true));
        assert_eq!(restore.reconcile(false, false), (false, true));
    }

    #[test]
    fn failed_restoration_keeps_the_only_surface_until_real_progress() {
        // Disabled external screens can disappear from BOTH enumeration lists.
        // That is safe for an ordinary idle headless session, but not evidence
        // that an unresolved failed restoration may release its last surface.
        assert!(!recovery_surface_can_release(
            false,
            Some(OtherDisplays::None)
        ));
        assert!(!recovery_surface_can_release(
            false,
            Some(OtherDisplays::Inactive)
        ));
        assert!(!recovery_surface_can_release(false, None)); // enumeration failed
        assert!(recovery_surface_can_release(
            false,
            Some(OtherDisplays::Active)
        ));
        assert!(recovery_surface_can_release(true, None)); // capture owns a lease
    }

    #[test]
    fn handshake_during_lock_releases_serial_and_keeps_the_original_hold_cap() {
        use crate::lock_activity::{self, LockDecision};
        use std::cell::Cell;
        let serial = StdMutex::new(());
        let now_ms = Cell::new(1_u64);
        let auth_ms = Cell::new(None::<u64>);
        let disconnect_ms = 1;
        let due_ms = 1;
        let locks = Cell::new(0);
        let acquisitions = Cell::new(0);
        let recovered = Cell::new(false);
        let decision = || {
            lock_activity::decide(
                false, // no SessionTracker epoch/count change during handshake
                None,
                auth_ms
                    .get()
                    .filter(|at| *at > disconnect_ms)
                    .map(|at| Duration::from_millis(now_ms.get() - at)),
                Duration::from_millis(now_ms.get() - due_ms),
            )
        };
        assert_eq!(
            complete_disconnect(
                || {
                    assert!(
                        serial.try_lock().is_ok(),
                        "hold must release auto-unlock's gate"
                    );
                    if acquisitions.get() == 1 {
                        assert_eq!(
                            decision(),
                            LockDecision::Hold,
                            "keep the original disconnect timestamp"
                        );
                        // Even another fresh authentication cannot reset this
                        // disconnect's already-running 30-second hold budget.
                        now_ms.set(due_ms + lock_activity::MAX_HOLD.as_millis() as u64);
                        auth_ms.set(Some(now_ms.get() - 1));
                    }
                    assert_eq!(decision(), LockDecision::Lock);
                    acquisitions.set(acquisitions.get() + 1);
                    Some(serial.lock().unwrap())
                },
                || decision() == LockDecision::Lock,
                || {
                    if locks.get() == 0 {
                        now_ms.set(2_001);
                        auth_ms.set(Some(2_001)); // arrived DURING lock confirmation
                    }
                    locks.set(locks.get() + 1);
                    true
                },
                || recovered.set(true),
            ),
            DisconnectCompletion::Completed,
        );
        assert_eq!(acquisitions.get(), 2);
        assert_eq!(locks.get(), 2);
        assert!(recovered.get());
    }

    #[test]
    fn handshake_that_finishes_after_lock_confirmation_cancels_old_recovery() {
        use std::cell::Cell;
        let serial = StdMutex::new(());
        let handshaking = Cell::new(false);
        let acquisitions = Cell::new(0);
        assert_eq!(
            complete_disconnect(
                || {
                    assert!(serial.try_lock().is_ok());
                    acquisitions.set(acquisitions.get() + 1);
                    if handshaking.get() {
                        // The original wait now observes the live session and
                        // cancels; it never creates a new disconnect deadline.
                        return None;
                    }
                    Some(serial.lock().unwrap())
                },
                || !handshaking.get(),
                || {
                    handshaking.set(true);
                    true
                },
                || panic!("must not restart over the new connection"),
            ),
            DisconnectCompletion::Cancelled,
        );
        assert_eq!(acquisitions.get(), 2);
    }

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
    fn an_empty_reconnect_guard_cannot_clear_an_unresolved_restore_failure() {
        let mut state = RestoreState::default();
        state.observe(RestoreOutcome::Removed { failed: true });
        for topology in [
            Some(OtherDisplays::None),
            Some(OtherDisplays::Inactive),
            None,
        ] {
            assert_eq!(
                state.observe_verified(RestoreOutcome::Removed { failed: false }, topology),
                RestoreOutcome::Removed { failed: true },
            );
            assert!(state.restart_due(false));
            assert!(!state.restart_due(true));
        }
        assert_eq!(
            state.observe_verified(
                RestoreOutcome::Removed { failed: false },
                Some(OtherDisplays::Active)
            ),
            RestoreOutcome::Removed { failed: false },
        );
        assert!(!state.pending_failure());
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
    #[test]
    fn offline_and_failed_restoration_are_different() {
        use OtherDisplays::*;
        assert_eq!(OtherDisplays::classify(&[42], &[42], Some(42)), None);
        assert_eq!(OtherDisplays::classify(&[], &[], Option::None), None);
        assert!(None.can_release_virtual());
        assert_eq!(OtherDisplays::classify(&[42], &[1, 42], Some(42)), Inactive);
        assert!(!Inactive.can_release_virtual());
        assert_eq!(
            OtherDisplays::classify(&[1, 42], &[1, 42], Some(42)),
            Active
        );
        assert!(Active.can_release_virtual());
        // Plugging in, detaching, then unplugging the only physical monitor.
        let states = [
            (&[1, 42][..], &[1, 42][..]),
            (&[42][..], &[1, 42][..]),
            (&[42][..], &[42][..]),
        ];
        let allowed: Vec<_> = states
            .iter()
            .map(|(a, o)| OtherDisplays::classify(a, o, Some(42)).can_release_virtual())
            .collect();
        assert_eq!(allowed, [true, false, true]);
    }
}
