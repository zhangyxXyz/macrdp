//! Opt-in authenticated capture preparation and immediate session locking.
//! No password leaves the existing in-memory auto-unlock closure. Private
//! login.framework symbols are resolved at runtime, never linked or assumed.
use anyhow::Result;
use std::sync::Arc;

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
pub async fn wait_for_unlock_surface(id: u32) -> Result<()> {
    use core_graphics::display::CGDisplay;
    use std::time::Duration;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let active = CGDisplay::active_displays().unwrap_or_default();
            let bounds = CGDisplay::new(id).bounds();
            if unlock_surface_ready(&active, id, bounds.size.width, bounds.size.height) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "WindowServer did not activate virtual display {id} within 8 seconds before unlock"
        )
    })
}
#[cfg(not(target_os = "macos"))]
pub async fn wait_for_unlock_surface(_: u32) -> Result<()> {
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
