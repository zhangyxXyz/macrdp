//! Virtual displays exist only for authenticated capture leases. Releasing the
//! last lease waits for serialized physical restoration before destroying it.
use crate::{
    display_transition::Coordinator,
    virtual_display::{PrimaryOverride, VirtualDisplay},
};
use anyhow::{Context, Result};
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
/// Aliases last exactly as long as this display lease. A recycled numeric
/// ID in a later session must never redirect input or physical restoration.
#[derive(Default)]
struct ActiveDisplayIds {
    id: Option<u32>,
    aliases: Vec<u32>,
    ready: bool,
}
impl ActiveDisplayIds {
    fn publish(&mut self, id: u32) {
        if let Some(previous) = self.id.replace(id) {
            if previous != id && !self.aliases.contains(&previous) {
                self.aliases.push(previous);
            }
        }
    }
    fn resolve(&self, saved: u32) -> u32 {
        self.id
            .filter(|_| self.aliases.contains(&saved))
            .unwrap_or(saved)
    }
}
static IDS: Mutex<ActiveDisplayIds> = Mutex::new(ActiveDisplayIds {
    id: None,
    aliases: Vec::new(),
    ready: false,
});
pub fn current_id(saved: u32) -> u32 {
    IDS.lock().unwrap().resolve(saved)
}
fn publish(id: u32) {
    let mut ids = IDS.lock().unwrap();
    if ids.id == Some(id) {
        return;
    }
    if let Some(old) = ids.id {
        tracing::info!(old, new = id, "virtual display re-registered");
    }
    ids.publish(id);
    crate::input::invalidate_display_bounds();
    crate::switcher_hud::set_display_id(id);
}

#[cfg(target_os = "macos")]
pub fn audio_ready() -> bool {
    IDS.lock().unwrap().ready
}
pub fn active_display_id() -> Option<u32> {
    IDS.lock().unwrap().id
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
    fn may_replace(&self, physical_restored: impl FnOnce() -> bool) -> bool {
        self.users == 0 && physical_restored()
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
pub type WeakDisplay = std::sync::Weak<Mutex<ManagedDisplay>>;
pub struct ManagedDisplay {
    active: Option<VirtualDisplay>,
    lifecycle: Lifecycle,
    recreate_unlisted: bool,
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
            recreate_unlisted: false,
            options,
            coordinator,
            idle_safe: Arc::new(idle_safe),
            primary,
        }))
    }
    pub fn scale_if_active(&self) -> Option<u32> {
        self.active
            .as_ref()
            .filter(|vd| vd.has_handle())
            .map(VirtualDisplay::ui_scale)
    }
    pub fn id_if_active(&self) -> Option<u32> {
        self.active
            .as_ref()
            .filter(|vd| vd.has_handle())
            .map(VirtualDisplay::display_id)
    }
    #[cfg(target_os = "macos")]
    pub fn resolve_listed(&mut self, ids: &[u32]) -> Option<u32> {
        let id = self.active.as_mut()?.resolve_listed(ids)?;
        publish(id);
        Some(id)
    }
    #[cfg(target_os = "macos")]
    pub fn resolve_ready_audio(&mut self, ids: &[u32]) -> Option<u32> {
        // Audio may have been awaiting SCK while the last video lease was
        // released or replaced. Recheck readiness under the display lock.
        if !audio_ready() {
            return None;
        }
        self.resolve_listed(ids)
    }
    #[cfg(target_os = "macos")]
    pub fn mark_unlisted(&mut self) {
        self.recreate_unlisted = true;
    }
    pub fn mark_ready(&mut self) {
        self.recreate_unlisted = false;
        let mut ids = IDS.lock().unwrap();
        if self.id_if_active().is_some() && self.id_if_active() == ids.id {
            ids.ready = true;
        }
    }
    /// Keep the already-existing surface alive through disconnect locking.
    /// Never creates a display for an idle server or changes SessionTracker.
    pub fn retain_active(shared: &SharedDisplay) -> Option<Lease> {
        let mut s = shared.lock().unwrap();
        s.id_if_active()?;
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
                if s.active.is_some() {
                    let can_replace = s.lifecycle.may_replace(|| (s.idle_safe)());
                    let force = s.recreate_unlisted && can_replace;
                    if force || !s.active.as_mut().unwrap().refresh_online()? {
                        anyhow::ensure!(can_replace, "virtual display unavailable while capture or physical restoration still owns it");
                        if let Some(primary) = &s.primary {
                            drop(primary.lock().unwrap().take());
                        }
                        // Clear input, audio and HUD state before dropping the live
                        // handle. A failed replacement leaves no accessible display.
                        *IDS.lock().unwrap() = ActiveDisplayIds::default();
                        crate::input::invalidate_display_bounds();
                        crate::switcher_hud::set_display_id(0);
                        s.active.as_mut().unwrap().recreate()?;
                        let id = s.active.as_ref().unwrap().display_id();
                        if let Some(primary) = s.primary.clone() {
                            match PrimaryOverride::install(id) {
                                Ok(guard) => *primary.lock().unwrap() = guard,
                                Err(error) => {
                                    drop(s.active.take());
                                    return Err(error);
                                }
                            }
                        }
                    }
                    publish(s.active.as_ref().unwrap().display_id());
                } else {
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
                    s.recreate_unlisted = false;
                    publish(id);
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
                        *IDS.lock().unwrap() = ActiveDisplayIds::default();
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
    fn timed_out_sck_cannot_replace_a_capture_or_pending_physical_restore() {
        let mut lifecycle = Lifecycle::default();
        lifecycle.enter();
        assert!(!lifecycle.may_replace(|| panic!("active capture or lock keepalive")));
        lifecycle.leave();
        assert!(!lifecycle.may_replace(|| false));
        assert!(lifecycle.may_replace(|| true));
    }
    #[test]
    fn aliases_follow_re_registration_only_within_the_same_lease() {
        let mut ids = ActiveDisplayIds::default();
        ids.publish(22);
        ids.ready = true;
        ids.publish(23);
        ids.publish(24);
        assert_eq!(ids.resolve(22), 24);
        assert_eq!(ids.resolve(23), 24);
        assert_eq!(ids.resolve(4), 4);
        assert!(ids.ready);
        ids.publish(22); // WindowServer reused the original ID
        assert_eq!(ids.resolve(23), 22);
        ids = ActiveDisplayIds::default();
        ids.publish(55);
        assert_eq!(ids.resolve(22), 22);
        assert!(!ids.ready);
    }
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
