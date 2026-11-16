//! Virtual displays exist only for authenticated capture leases. Releasing the
//! last lease waits for serialized physical restoration before destroying it.
use crate::{
    display_transition::Coordinator,
    virtual_display::{PrimaryOverride, VirtualDisplay},
};
use anyhow::{Context, Result};
use std::ops::{Deref, DerefMut};
use std::sync::{
    atomic::{AtomicU32, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};
static ACTIVE: AtomicU32 = AtomicU32::new(0);
static READY: AtomicU32 = AtomicU32::new(0);
pub fn mark_ready(id: u32) {
    if ACTIVE.load(Ordering::Acquire) == id {
        READY.store(id, Ordering::Release);
    }
}
static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn enable() {
    ENABLED.store(true, Ordering::Release);
}
pub async fn audio_display(
    fallback: Option<u32>,
    is_current: impl Fn() -> bool,
) -> Result<Option<u32>> {
    anyhow::ensure!(
        is_current(),
        "audio session retired before display readiness"
    );
    if fallback.is_some() || !ENABLED.load(Ordering::Acquire) {
        return Ok(fallback);
    }
    let id = tokio::time::timeout(std::time::Duration::from_secs(30), async {
        loop {
            anyhow::ensure!(
                is_current(),
                "audio session retired while waiting for display"
            );
            let id = READY.load(Ordering::Acquire);
            if id != 0 {
                return Ok::<u32, anyhow::Error>(id);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    })
    .await
    .context("session virtual display was not available for audio within 30 seconds")??;
    Ok(Some(id))
}
pub fn active_display_id() -> Option<u32> {
    match ACTIVE.load(Ordering::Acquire) {
        0 => None,
        id => Some(id),
    }
}

#[derive(Default)]
struct Lifecycle {
    users: usize,
    generation: u64,
    cleanup_not_before: Option<Instant>,
}
impl Lifecycle {
    fn enter(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.users += 1;
    }
    fn leave(&mut self) -> Option<u64> {
        self.users = self.users.saturating_sub(1);
        (self.users == 0).then_some(self.generation)
    }
    fn leave_after(&mut self, now: Instant, grace: Duration) -> Option<(u64, Instant)> {
        // A capture may finish while an older lock keepalive remains. Record
        // its grace even when it is NOT the final user; the eventual zero-grace
        // keepalive drop must not erase that newer capture's reactivation window.
        let requested = now + grace;
        let deadline = self
            .cleanup_not_before
            .map_or(requested, |pending| pending.max(requested));
        self.cleanup_not_before = Some(deadline);
        self.leave().map(|generation| (generation, deadline))
    }
    fn may_release(&self, generation: u64) -> bool {
        self.users == 0 && self.generation == generation
    }
}
#[derive(Clone, Copy)]
pub struct Options {
    pub width: u32,
    pub height: u32,
    pub hidpi: bool,
    pub scale: u32,
}
pub type SharedDisplay = Arc<Mutex<ManagedDisplay>>;
pub struct ManagedDisplay {
    active: Option<VirtualDisplay>,
    lifecycle: Lifecycle,
    options: Options,
    coordinator: Coordinator,
    idle_safe: Arc<dyn Fn() -> bool + Send + Sync>,
    primary: Option<Arc<Mutex<Option<PrimaryOverride>>>>,
}
impl ManagedDisplay {
    pub fn new(
        options: Options,
        coordinator: Coordinator,
        idle_safe: impl Fn() -> bool + Send + Sync + 'static,
        primary: Option<Arc<Mutex<Option<PrimaryOverride>>>>,
    ) -> SharedDisplay {
        Arc::new(Mutex::new(Self {
            active: None,
            lifecycle: Lifecycle::default(),
            options,
            coordinator,
            idle_safe: Arc::new(idle_safe),
            primary,
        }))
    }
    pub fn scale_if_active(&self) -> Option<u32> {
        self.active.as_ref().map(VirtualDisplay::ui_scale)
    }
    pub fn id_if_active(&self) -> Option<u32> {
        self.active.as_ref().map(VirtualDisplay::display_id)
    }
    /// Keep the already-existing surface alive through disconnect locking.
    /// Never creates a display for an idle server or changes SessionTracker.
    pub fn retain_active(shared: &SharedDisplay) -> Option<Lease> {
        let mut s = shared.lock().unwrap();
        s.active.as_ref()?;
        s.lifecycle.enter();
        Some(Lease {
            shared: shared.clone(),
            cleanup_runtime: tokio::runtime::Handle::try_current().ok(),
            cleanup_grace: std::time::Duration::ZERO,
        })
    }
    pub async fn acquire(shared: &SharedDisplay) -> Result<Lease> {
        let coordinator = shared.lock().unwrap().coordinator.clone();
        let state = shared.clone();
        let lease = coordinator
            .run(move |_| {
                let mut s = state.lock().unwrap();
                if s.active.is_none() {
                    let o = s.options;
                    let vd = VirtualDisplay::new(o.width, o.height, 60, o.hidpi, o.scale)
                        .context("creating session virtual display")?;
                    if let Some(primary) = &s.primary {
                        *primary.lock().unwrap() = PrimaryOverride::install(vd.display_id())?;
                    }
                    let id = vd.display_id();
                    let origin = vd.origin_pts();
                    let points = vd.size_pts();
                    s.active = Some(vd);
                    ACTIVE.store(id, Ordering::Release);
                    crate::switcher_hud::set_display_id(id);
                    tracing::info!(
                        display_id = id,
                        ?origin,
                        ?points,
                        "virtual display activated for authenticated capture"
                    );
                }
                s.lifecycle.enter();
                Ok::<_, anyhow::Error>(Lease {
                    shared: state.clone(),
                    cleanup_runtime: tokio::runtime::Handle::try_current().ok(),
                    cleanup_grace: std::time::Duration::from_millis(2500),
                })
            })
            .await?
            .context("display creation cancelled during shutdown")??;
        Ok(lease)
    }
}
impl Deref for ManagedDisplay {
    type Target = VirtualDisplay;
    fn deref(&self) -> &VirtualDisplay {
        self.active
            .as_ref()
            .expect("virtual display requires a live capture lease")
    }
}
impl DerefMut for ManagedDisplay {
    fn deref_mut(&mut self) -> &mut VirtualDisplay {
        self.active
            .as_mut()
            .expect("virtual display requires a live capture lease")
    }
}

pub struct Lease {
    shared: SharedDisplay,
    cleanup_runtime: Option<tokio::runtime::Handle>,
    cleanup_grace: std::time::Duration,
}
impl Drop for Lease {
    fn drop(&mut self) {
        let pending = {
            self.shared
                .lock()
                .unwrap()
                .lifecycle
                .leave_after(Instant::now(), self.cleanup_grace)
        };
        let Some((generation, deadline)) = pending else {
            return;
        };
        let shared = self.shared.clone();
        let coordinator = shared.lock().unwrap().coordinator.clone();
        let Some(runtime) = self
            .cleanup_runtime
            .clone()
            .or_else(|| tokio::runtime::Handle::try_current().ok())
        else {
            return;
        };
        runtime.spawn(async move {
            // Resize/reactivation briefly drops capture; preserve the display
            // through the latest capture's grace, regardless of which lease
            // happened to be released last. A completed lock adds no extra grace.
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
            loop {
                let state = shared.clone();
                let result = coordinator
                    .run(move |_| {
                        let mut s = state.lock().unwrap();
                        if !s.lifecycle.may_release(generation) {
                            return true;
                        }
                        // Same coordinator as overlay install/restore: a guard may
                        // not be absent merely because its install is still running.
                        if !(s.idle_safe)() {
                            return false;
                        }
                        if let Some(primary) = &s.primary {
                            drop(primary.lock().unwrap().take());
                        }
                        crate::switcher_hud::hide();
                        READY.store(0, Ordering::Release);
                        ACTIVE.store(0, Ordering::Release);
                        drop(s.active.take());
                        tracing::info!(
                            "virtual display released after last capture and physical restoration"
                        );
                        true
                    })
                    .await;
                if !matches!(result, Ok(Some(false))) {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
        });
    }
}

/// Explicit drop order: stop SCK before the display lease can be released.
pub struct Updates {
    inner: Option<Box<dyn ironrdp_server::RdpServerDisplayUpdates + Send>>,
    lease: Option<Lease>,
}
impl Updates {
    pub fn new(
        inner: Box<dyn ironrdp_server::RdpServerDisplayUpdates + Send>,
        lease: Lease,
    ) -> Self {
        Self {
            inner: Some(inner),
            lease: Some(lease),
        }
    }
}
impl Drop for Updates {
    fn drop(&mut self) {
        drop(self.inner.take());
        drop(self.lease.take());
    }
}
#[async_trait::async_trait]
impl ironrdp_server::RdpServerDisplayUpdates for Updates {
    async fn next_update(
        &mut self,
    ) -> ironrdp_server::ServerResult<Option<ironrdp_server::DisplayUpdate>> {
        self.inner.as_mut().unwrap().next_update().await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn startup_configuration_does_not_create_a_display() {
        let shared = ManagedDisplay::new(
            Options {
                width: 3840,
                height: 2160,
                hidpi: true,
                scale: 2,
            },
            Coordinator::default(),
            || true,
            None,
        );
        let s = shared.lock().unwrap();
        assert!(s.active.is_none());
        assert_eq!(s.scale_if_active(), None);
        assert_eq!(s.lifecycle.users, 0);
    }
    #[test]
    fn reconnect_invalidates_old_cleanup_even_before_capture_enters_tracker() {
        let mut life = Lifecycle::default();
        life.enter();
        let old = life.leave().unwrap();
        assert!(life.may_release(old));
        life.enter();
        assert!(!life.may_release(old));
        let new = life.leave().unwrap();
        assert!(!life.may_release(old));
        assert!(life.may_release(new));
    }
    #[test]
    fn reactivation_keeps_display_until_every_capture_lease_ends() {
        let mut life = Lifecycle::default();
        life.enter();
        life.enter();
        assert!(life.leave().is_none());
        let last = life.leave().unwrap();
        assert!(life.may_release(last));
    }
    #[test]
    fn lock_keepalive_blocks_old_cleanup_and_reconnect_supersedes_it() {
        let mut life = Lifecycle::default();
        life.enter();
        let capture_end = life.leave().unwrap();
        // Watcher retains the already-existing surface while the original
        // capture's disconnect grace is pending.
        life.enter();
        assert!(!life.may_release(capture_end));
        let lock_end = life.leave().unwrap();
        assert!(life.may_release(lock_end));
        life.enter(); // new authenticated capture beats pending cleanup
        assert!(!life.may_release(lock_end));
    }
    #[test]
    fn old_lock_keepalive_cannot_shorten_a_new_captures_grace() {
        let start = Instant::now();
        let grace = Duration::from_millis(2500);
        let mut life = Lifecycle::default();
        life.enter(); // original capture
        let (old, _) = life.leave_after(start, grace).unwrap();
        life.enter(); // pending lock keeps the original display alive
        assert!(!life.may_release(old));
        life.enter(); // a brief reconnect completes during the lock hold
        let latest_disconnect = start + Duration::from_secs(10);
        assert!(life.leave_after(latest_disconnect, grace).is_none());
        let lock_cancelled = latest_disconnect + Duration::from_millis(100);
        let (last, deadline) = life.leave_after(lock_cancelled, Duration::ZERO).unwrap();
        assert!(life.may_release(last));
        assert_eq!(deadline, latest_disconnect + grace);
        assert_eq!(
            deadline.duration_since(lock_cancelled),
            Duration::from_millis(2400)
        );
        // Another reconnect invalidates this cleanup even while its timer runs.
        life.enter();
        assert!(!life.may_release(last));
    }
    #[test]
    fn completed_lock_does_not_add_a_second_grace_period() {
        let start = Instant::now();
        let mut life = Lifecycle::default();
        life.enter();
        life.leave_after(start, Duration::from_millis(2500))
            .unwrap();
        life.enter();
        let lock_finished = start + Duration::from_secs(3);
        let (_, deadline) = life.leave_after(lock_finished, Duration::ZERO).unwrap();
        assert_eq!(deadline, lock_finished);
    }
}
