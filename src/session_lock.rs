//! Opt-in authenticated capture preparation and immediate session locking.
//! No password leaves the existing in-memory auto-unlock closure. Private
//! login.framework symbols are resolved at runtime, never linked or assumed.
use anyhow::Result;
use std::sync::Arc;
use std::time::Duration;

#[cfg(target_os = "macos")]
static SCREENS_ASLEEP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "macos")]
pub fn observe_display_sleep() {
    crate::runloop_thread::submit(|| unsafe {
        use objc2_app_kit::{
            NSWorkspace, NSWorkspaceScreensDidSleepNotification,
            NSWorkspaceScreensDidWakeNotification,
        };
        use objc2_foundation::NSNotification;
        use std::ptr::NonNull;
        use std::sync::atomic::Ordering;

        let workspace = NSWorkspace::sharedWorkspace();
        let center = workspace.notificationCenter();
        for (name, asleep) in [
            (NSWorkspaceScreensDidSleepNotification, true),
            (NSWorkspaceScreensDidWakeNotification, false),
        ] {
            let block = block2::RcBlock::new(move |_: NonNull<NSNotification>| {
                SCREENS_ASLEEP.store(asleep, Ordering::SeqCst);
            });
            let token = center.addObserverForName_object_queue_usingBlock(
                Some(name),
                Some(workspace.as_ref()),
                None,
                &block,
            );
            // The workspace observer has the server's lifetime.
            std::mem::forget(token);
        }
    });
}

#[cfg(target_os = "macos")]
pub fn display_asleep(display_id: Option<u32>) -> bool {
    use core_graphics::display::CGDisplay;
    use std::sync::atomic::Ordering;
    SCREENS_ASLEEP.load(Ordering::SeqCst)
        || display_id
            .map_or_else(CGDisplay::main, CGDisplay::new)
            .is_asleep()
}

/// Wake before acquiring a virtual display or waiting on the display
/// coordinator. An in-flight CoreGraphics restore can itself need this wake.
/// The power request never submits a password or holds displays awake for
/// the lifetime of the connection.
#[cfg(target_os = "macos")]
pub async fn wake_for_capture(timeout: Duration, virtual_configured: bool) -> Result<bool> {
    use core_graphics::display::CGDisplay;
    wake_for_surface(
        timeout,
        virtual_configured,
        || {
            display_asleep(None)
                || CGDisplay::active_displays().is_ok_and(|active| active.is_empty())
        },
        || {
            if virtual_configured {
                let id = crate::display_session::active_display_id();
                if crate::virtual_display::other_displays(id)?
                    == crate::display_transition::OtherDisplays::None
                {
                    let drawable = id.is_some_and(|id| {
                        let display = CGDisplay::new(id);
                        display.is_active() && !display.is_asleep()
                    });
                    return Ok(WakeSurface::Virtual { drawable });
                }
            }
            Ok(WakeSurface::Other)
        },
        async || tokio::task::spawn_blocking(declare_display_activity).await?,
    )
    .await
}

#[cfg(not(target_os = "macos"))]
pub async fn wake_for_capture(_: Duration, _: bool) -> Result<bool> {
    Ok(false)
}

#[cfg(target_os = "macos")]
fn declare_display_activity() -> Result<()> {
    use core_foundation::{base::TCFType, string::CFString};
    #[link(name = "IOKit", kind = "framework")]
    extern "C" {
        fn IOPMAssertionDeclareUserActivity(
            name: core_foundation::string::CFStringRef,
            user_type: libc::c_uint,
            assertion: *mut u32,
        ) -> libc::c_int;
    }
    const IOPM_USER_ACTIVE_LOCAL: libc::c_uint = 0;
    // Reuse the activity ID across reconnects. Power management expires it;
    // releasing it here could undo the request before the display wakes.
    static ASSERTION: std::sync::Mutex<u32> = std::sync::Mutex::new(0);
    let mut assertion = ASSERTION.lock().unwrap();
    let name = CFString::new("macrdp authenticated display wake");
    let status = unsafe {
        IOPMAssertionDeclareUserActivity(
            name.as_concrete_TypeRef(),
            IOPM_USER_ACTIVE_LOCAL,
            &mut *assertion,
        )
    };
    anyhow::ensure!(
        status == 0,
        "display wake request failed (IOReturn {status:#x})"
    );
    Ok(())
}

/// Blocking display coordinator only. This is invoked after a native
/// configuration operation actually fails with WindowServer's sleep error.
/// An intentional lock + Esc must still be able to end a live capture.
#[cfg(target_os = "macos")]
pub fn wake_after_configuration_error(id: u32, restoring: bool) -> bool {
    if !display_asleep(Some(id))
        || (!restoring && crate::virtual_display::screen_authentication_is_required() == Some(true))
    {
        return false;
    }
    if let Err(error) = declare_display_activity() {
        tracing::warn!(%error, "could not wake displays for reconfiguration");
        return false;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while display_asleep(Some(id)) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(100));
    }
    true
}

#[cfg(any(target_os = "macos", test))]
enum WakeSurface {
    Other,
    Virtual { drawable: bool },
}

/// Returns true when the caller must acquire a virtual login surface before
/// running auto-unlock. A CG query error cannot silently become "no displays".
#[cfg(any(target_os = "macos", test))]
async fn wake_for_surface(
    timeout: Duration,
    virtual_configured: bool,
    mut asleep: impl FnMut() -> bool,
    mut surface: impl FnMut() -> Result<WakeSurface>,
    mut request_wake: impl AsyncFnMut() -> Result<()>,
) -> Result<bool> {
    use anyhow::Context;
    let mut requested = false;
    let result = tokio::time::timeout(timeout, async {
        loop {
            let sleeping = asleep();
            if sleeping && !requested {
                // A previous CG restore may itself need a wake. Signal before
                // enumerating displays or queuing for the coordinator.
                tracing::info!("display asleep at authenticated capture startup; requesting wake");
                request_wake().await?;
                requested = true;
            }
            match surface()? {
                WakeSurface::Virtual { drawable } => {
                    if !requested && !drawable {
                        // After a restart we may have missed the sleep notification.
                        // A Mac with no registered display has no main ID to query.
                        request_wake().await?;
                    }
                    tracing::debug!("preparing the virtual login surface without waiting for a physical display");
                    return Ok(true);
                }
                WakeSurface::Other if !sleeping => {
                    if requested {
                        tracing::info!("display woke before authenticated capture preparation");
                    }
                    return Ok(false);
                }
                WakeSurface::Other => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
    })
    .await;
    if result.is_err() && requested && virtual_configured {
        tracing::info!(
            timeout_secs = timeout.as_secs(),
            "display wake unconfirmed; checking the authenticated virtual login surface"
        );
        return Ok(true);
    }
    result.with_context(|| {
        format!(
            "display wake exceeded {} seconds before capture preparation",
            timeout.as_secs()
        )
    })?
}

/// No physical desktop effect unless explicitly requested, PAM validated and
/// paired with a supported headless display policy.
pub fn auto_unlock_enabled(requested: bool, skip_auth: bool, headless: bool) -> bool {
    requested && !skip_auth && headless
}

pub struct CapturePreparation {
    operation: Arc<dyn Fn() -> Result<bool> + Send + Sync>,
    serial: Arc<tokio::sync::Mutex<()>>,
}
impl CapturePreparation {
    pub fn new(operation: impl Fn() -> Result<bool> + Send + Sync + 'static) -> Self {
        Self {
            operation: Arc::new(operation),
            serial: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    /// Called from display.updates(), after credential/ARC validation. Blocking
    /// keyboard/lock polling never occupies a tokio worker. No detached task is
    /// launched by on_accept/on_authenticated (a preemption probe is not served).
    pub async fn run(&self) -> Result<bool> {
        self.run_with_guard(()).await.map(|(unlocked, ())| unlocked)
    }

    /// Keep an authenticated preparation's display lease alive until password
    /// work actually finishes, including when the awaiting connection is cancelled.
    /// On success ownership returns to the caller for the following capture.
    pub async fn run_with_guard<G: Send + 'static>(&self, guard: G) -> Result<(bool, G)> {
        let serial = self.serial.clone().lock_owned().await;
        let operation = self.operation.clone();
        tokio::task::spawn_blocking(move || {
            // Cancellation of the awaiting connection must not release the
            // guard while its blocking password operation is still executing.
            let _serial = serial;
            Ok((operation()?, guard))
        })
        .await?
    }
}

/// A Mac with no other display needs a WindowServer surface before input can
/// wake the lock screen. ScreenCaptureKit enumeration is deliberately not used
/// here: it can hide the desktop while locked, which would invert the dependency.
#[cfg(target_os = "macos")]
pub async fn wait_for_unlock_surface(shared: &crate::display_session::SharedDisplay) -> Result<()> {
    use core_graphics::display::CGDisplay;
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let active = CGDisplay::active_displays().unwrap_or_default();
            let id = shared.lock().unwrap().resolve_listed(&active);
            if let Some(id) = id {
                let bounds = CGDisplay::new(id).bounds();
                if !CGDisplay::new(id).is_asleep()
                    && unlock_surface_ready(&active, id, bounds.size.width, bounds.size.height)
                {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "WindowServer did not activate the owned virtual display within 8 seconds before unlock"
        )
    })
}
#[cfg(not(target_os = "macos"))]
pub async fn wait_for_unlock_surface(_: &crate::display_session::SharedDisplay) -> Result<()> {
    Ok(())
}

#[cfg(any(target_os = "macos", test))]
fn unlock_surface_ready(active: &[u32], id: u32, width: f64, height: f64) -> bool {
    id != 0
        && active.contains(&id)
        && width.is_finite()
        && height.is_finite()
        && width > 0.0
        && height > 0.0
}

/// Only after a confirmed unlock: WindowServer/SCK can lag the lock flag.
/// Each enumeration and the entire settling period have hard bounds.
#[cfg(target_os = "macos")]
pub async fn wait_for_display(target: Option<u32>) -> Result<()> {
    use screencapturekit::async_api::AsyncSCShareableContent;
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(content) = AsyncSCShareableContent::get().await {
                if display_available(
                    &content
                        .displays()
                        .iter()
                        .map(|d| d.display_id())
                        .collect::<Vec<_>>(),
                    target,
                ) {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "ScreenCaptureKit did not expose the requested display within 8 seconds after unlock"
        )
    })
}
#[cfg(not(target_os = "macos"))]
pub async fn wait_for_display(_: Option<u32>) -> Result<()> {
    Ok(())
}

fn display_available(displays: &[u32], target: Option<u32>) -> bool {
    match target {
        Some(id) => displays.contains(&id),
        None => !displays.is_empty(),
    }
}

/// A successful request is not evidence of a locked desktop. Never silently
/// fall back to a screen saver, and never change the user's password-delay policy.
fn request_and_confirm(
    mut state: impl FnMut() -> Option<bool>,
    request: impl FnOnce() -> Result<()>,
    mut wait: impl FnMut(),
) -> Result<()> {
    // Always request an immediate lock: the screen saver can already be
    // active while macOS still allows returning without a password.
    request()?;
    for attempt in 0..=20 {
        if state() == Some(true) {
            return Ok(());
        }
        if attempt != 20 {
            wait();
        }
    }
    anyhow::bail!("immediate lock request was accepted but the system lock state was not confirmed within 2 seconds")
}

#[cfg(target_os = "macos")]
pub fn lock_now() -> Result<()> {
    request_and_confirm(
        crate::virtual_display::screen_authentication_is_required,
        || {
            // Signature and runtime lookup follow Chromium base/mac/login_util.cc
            // (OSStatus, no arguments) and Hammerspoon's caffeinate implementation.
            type Lock = unsafe extern "C" fn() -> i32;
            static FUNCTION: std::sync::OnceLock<std::result::Result<Lock, String>> =
                std::sync::OnceLock::new();
            let lock = FUNCTION
                .get_or_init(|| unsafe {
                    let handle = libc::dlopen(
                        c"/System/Library/PrivateFrameworks/login.framework/Versions/Current/login"
                            .as_ptr(),
                        libc::RTLD_LAZY | libc::RTLD_LOCAL,
                    );
                    if handle.is_null() {
                        return Err("login.framework is unavailable".into());
                    }
                    let symbol = libc::dlsym(handle, c"SACLockScreenImmediate".as_ptr());
                    if symbol.is_null() {
                        libc::dlclose(handle);
                        return Err("SACLockScreenImmediate is unavailable on this macOS".into());
                    }
                    // Keep the framework loaded while its cached function can be called.
                    Ok(std::mem::transmute::<*mut libc::c_void, Lock>(symbol))
                })
                .as_ref()
                .map_err(|error| anyhow::anyhow!("{error}"))?;
            let status = unsafe { lock() };
            anyhow::ensure!(
                status == 0,
                "SACLockScreenImmediate returned OSStatus {status}"
            );
            Ok(())
        },
        || std::thread::sleep(std::time::Duration::from_millis(100)),
    )
}
#[cfg(not(target_os = "macos"))]
pub fn lock_now() -> Result<()> {
    anyhow::bail!("immediate session lock is only supported on macOS")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[tokio::test]
    async fn awake_display_needs_no_wake_request() {
        assert!(!wake_for_surface(
            Duration::from_secs(3),
            false,
            || false,
            || Ok(WakeSurface::Other),
            async || panic!("already awake")
        )
        .await
        .unwrap());
    }

    #[tokio::test(start_paused = true)]
    async fn sleeping_display_must_wake_before_capture_can_prepare() {
        let started = tokio::time::Instant::now();
        let requests = AtomicUsize::new(0);
        assert!(!wake_for_surface(
            Duration::from_secs(3),
            false,
            || started.elapsed() < Duration::from_millis(300),
            || Ok(WakeSurface::Other),
            async || {
                requests.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap());
        assert_eq!(requests.load(Ordering::SeqCst), 1);
        assert!(started.elapsed() >= Duration::from_millis(300));
    }

    #[tokio::test(start_paused = true)]
    async fn no_monitor_bootstraps_even_if_sleep_notification_is_missing_or_stale() {
        for (asleep, drawable, expected_requests) in [
            (true, false, 1),
            (false, false, 1),
            (true, true, 1),
            (false, true, 0),
        ] {
            let started = tokio::time::Instant::now();
            let requests = AtomicUsize::new(0);
            assert!(wake_for_surface(
                Duration::from_secs(1),
                true,
                || asleep,
                || {
                    if asleep {
                        assert_eq!(requests.load(Ordering::SeqCst), 1);
                    }
                    Ok(WakeSurface::Virtual { drawable })
                },
                async || {
                    requests.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                }
            )
            .await
            .unwrap());
            assert_eq!(requests.load(Ordering::SeqCst), expected_requests);
            assert_eq!(started.elapsed(), Duration::ZERO);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn display_wake_uses_configured_deadline_and_propagates_errors() {
        for seconds in [1, 3, 10] {
            let started = tokio::time::Instant::now();
            let timeout = Duration::from_secs(seconds);
            let error = wake_for_surface(
                timeout,
                false,
                || true,
                || Ok(WakeSurface::Other),
                async || Ok(()),
            )
            .await
            .unwrap_err();
            assert!(error.to_string().contains(&format!("{seconds} seconds")));
            assert_eq!(started.elapsed(), timeout);
        }
        let error = wake_for_surface(
            Duration::from_secs(3),
            false,
            || true,
            || Ok(WakeSurface::Other),
            async || anyhow::bail!("IOKit failed"),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "IOKit failed");
        let error = wake_for_surface(
            Duration::from_secs(3),
            false,
            || false,
            || anyhow::bail!("CG enumeration failed"),
            async || panic!("unknown topology"),
        )
        .await
        .unwrap_err();
        assert_eq!(error.to_string(), "CG enumeration failed");
    }

    #[tokio::test(start_paused = true)]
    async fn virtual_fallback_requires_a_completed_wake_request() {
        let timeout = Duration::from_secs(2);
        assert!(wake_for_surface(
            timeout,
            true,
            || true,
            || Ok(WakeSurface::Other),
            async || Ok(())
        )
        .await
        .unwrap());
        assert!(wake_for_surface(
            timeout,
            true,
            || true,
            || Ok(WakeSurface::Other),
            async || std::future::pending().await
        )
        .await
        .is_err());
        assert!(wake_for_surface(
            timeout,
            true,
            || true,
            || Ok(WakeSurface::Other),
            async || anyhow::bail!("IOKit rejected wake")
        )
        .await
        .is_err());
    }

    #[test]
    fn unlock_requires_opt_in_authentication_and_supported_display_policy() {
        for requested in [false, true] {
            for skip_auth in [false, true] {
                for headless in [false, true] {
                    assert_eq!(
                        auto_unlock_enabled(requested, skip_auth, headless),
                        requested && !skip_auth && headless
                    );
                }
            }
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn password_work_does_not_run_on_the_runtime_thread() {
        let runtime_thread = std::thread::current().id();
        let prep = CapturePreparation::new(move || {
            assert_ne!(std::thread::current().id(), runtime_thread);
            Ok(false)
        });
        assert!(!prep.run().await.unwrap());
    }
    #[test]
    fn requested_display_must_actually_exist() {
        assert!(!display_available(&[], None));
        assert!(!display_available(&[], Some(37)));
        assert!(!display_available(&[38], Some(37)));
        assert!(display_available(&[38, 37], Some(37)));
        assert!(display_available(&[38], None));
    }
    #[test]
    fn unlock_surface_requires_the_target_active_with_valid_bounds() {
        assert!(unlock_surface_ready(&[37], 37, 1920.0, 1080.0));
        assert!(!unlock_surface_ready(&[], 37, 1920.0, 1080.0));
        assert!(!unlock_surface_ready(&[38], 37, 1920.0, 1080.0));
        assert!(!unlock_surface_ready(&[0], 0, 1920.0, 1080.0));
        for bad in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            assert!(!unlock_surface_ready(&[37], 37, bad, 1080.0));
            assert!(!unlock_surface_ready(&[37], 37, 1920.0, bad));
        }
    }
    #[test]
    fn existing_lock_state_still_gets_an_immediate_request() {
        let requested = AtomicUsize::new(0);
        assert!(request_and_confirm(
            || {
                assert_eq!(requested.load(Ordering::SeqCst), 1);
                Some(true)
            },
            || {
                requested.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
            || panic!("no wait")
        )
        .is_ok());
        assert_eq!(requested.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn existing_lock_state_does_not_hide_a_failed_immediate_request() {
        assert!(request_and_confirm(
            || Some(true),
            || anyhow::bail!("request failed"),
            || panic!("no wait after failed request")
        )
        .is_err());
    }
    #[test]
    fn successful_request_without_locked_state_is_failure() {
        let waits = AtomicUsize::new(0);
        assert!(request_and_confirm(
            || Some(false),
            || Ok(()),
            || {
                waits.fetch_add(1, Ordering::SeqCst);
            }
        )
        .is_err());
        assert_eq!(waits.load(Ordering::SeqCst), 20);
        assert!(request_and_confirm(|| None, || Ok(()), || {}).is_err());
    }
    #[test]
    fn request_failure_is_not_hidden_by_screen_saver_fallback() {
        assert!(request_and_confirm(
            || Some(false),
            || anyhow::bail!("missing API"),
            || panic!("no wait")
        )
        .is_err());
    }
    #[test]
    fn waits_for_real_confirmation() {
        let calls = AtomicUsize::new(0);
        assert!(request_and_confirm(
            || Some(calls.fetch_add(1, Ordering::SeqCst) >= 3),
            || Ok(()),
            || {}
        )
        .is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 4);
    }
    #[tokio::test]
    async fn preparation_finishes_before_capture_and_failure_stops_it() {
        let called = Arc::new(AtomicUsize::new(0));
        let c = called.clone();
        let prep = CapturePreparation::new(move || {
            c.fetch_add(1, Ordering::SeqCst);
            Ok(true)
        });
        assert!(prep.run().await.unwrap());
        assert_eq!(called.load(Ordering::SeqCst), 1);
        let failed = CapturePreparation::new(|| anyhow::bail!("unsafe lock state"));
        assert!(failed.run().await.is_err());
    }
    #[tokio::test]
    async fn cancelled_waiter_does_not_release_an_active_password_operation() {
        let calls = Arc::new(AtomicUsize::new(0));
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (count, gate) = (calls.clone(), release.clone());
        let prep = Arc::new(CapturePreparation::new(move || {
            let call = count.fetch_add(1, Ordering::SeqCst);
            if call == 0 {
                let (mutex, condition) = &*gate;
                let mut done = mutex.lock().unwrap();
                while !*done {
                    done = condition.wait(done).unwrap();
                }
            }
            Ok(false)
        }));
        let first = {
            let prep = prep.clone();
            tokio::spawn(async move { prep.run().await })
        };
        while calls.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        first.abort();
        let _ = first.await;
        let mut second = {
            let prep = prep.clone();
            tokio::spawn(async move { prep.run().await })
        };
        let queued = tokio::time::timeout(std::time::Duration::from_millis(20), &mut second).await;
        let before_release = calls.load(Ordering::SeqCst);
        // Release even when a regression let the second operation run early;
        // otherwise a failing assertion would strand the blocking worker.
        *release.0.lock().unwrap() = true;
        release.1.notify_one();
        assert!(queued.is_err());
        assert_eq!(before_release, 1);
        assert!(!second.await.unwrap().unwrap());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
    #[tokio::test]
    async fn cancelled_preparation_retains_its_display_guard_until_worker_finishes() {
        use std::sync::atomic::AtomicBool;
        struct DisplayGuard {
            finished: Arc<AtomicBool>,
            dropped: Option<tokio::sync::oneshot::Sender<bool>>,
        }
        impl Drop for DisplayGuard {
            fn drop(&mut self) {
                let _ = self
                    .dropped
                    .take()
                    .unwrap()
                    .send(self.finished.load(Ordering::SeqCst));
            }
        }
        let finished = Arc::new(AtomicBool::new(false));
        let started = Arc::new(tokio::sync::Notify::new());
        let release = Arc::new((std::sync::Mutex::new(false), std::sync::Condvar::new()));
        let (worker_finished, worker_started, gate) =
            (finished.clone(), started.clone(), release.clone());
        let prep = CapturePreparation::new(move || {
            worker_started.notify_one();
            let (mutex, condition) = &*gate;
            let mut done = mutex.lock().unwrap();
            while !*done {
                done = condition.wait(done).unwrap();
            }
            worker_finished.store(true, Ordering::SeqCst);
            Ok(true)
        });
        let (dropped, mut drop_result) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            prep.run_with_guard(DisplayGuard {
                finished,
                dropped: Some(dropped),
            })
            .await
        });
        let entered =
            tokio::time::timeout(std::time::Duration::from_secs(2), started.notified()).await;
        task.abort();
        let _ = task.await;
        let before_release = drop_result.try_recv();
        // Always unblock the worker before assertions, including a failing
        // regression, so tokio runtime shutdown cannot hang on this test.
        *release.0.lock().unwrap() = true;
        release.1.notify_one();
        assert!(entered.is_ok());
        assert!(matches!(
            before_release,
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), drop_result)
                .await
                .unwrap()
                .unwrap()
        );
    }

    #[tokio::test]
    async fn successful_preparation_returns_display_ownership_to_capture() {
        let display = Arc::new(());
        let prep = CapturePreparation::new(|| Ok(true));
        let (unlocked, lease) = prep.run_with_guard(display.clone()).await.unwrap();
        assert!(unlocked);
        assert_eq!(Arc::strong_count(&display), 2);
        drop(lease);
        assert_eq!(Arc::strong_count(&display), 1);
        let failed = CapturePreparation::new(|| anyhow::bail!("unsafe lock state"));
        assert!(failed.run_with_guard(display.clone()).await.is_err());
        assert_eq!(Arc::strong_count(&display), 1);
    }
    #[tokio::test]
    async fn concurrent_preparations_do_not_submit_passwords_together() {
        let active = Arc::new(AtomicUsize::new(0));
        let count = active.clone();
        let prep = Arc::new(CapturePreparation::new(move || {
            assert_eq!(count.fetch_add(1, Ordering::SeqCst), 0);
            std::thread::sleep(std::time::Duration::from_millis(10));
            assert_eq!(count.fetch_sub(1, Ordering::SeqCst), 1);
            Ok(false)
        }));
        let (a, b) = tokio::join!(prep.run(), prep.run());
        assert!(!a.unwrap());
        assert!(!b.unwrap());
    }
}
