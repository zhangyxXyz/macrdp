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
        let serial = self.serial.clone().lock_owned().await;
        let operation = self.operation.clone();
        tokio::task::spawn_blocking(move || {
            // Cancellation of the awaiting connection must not release the
            // guard while its blocking password operation is still executing.
            let _serial = serial;
            operation()
        })
        .await?
    }
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
    if state() == Some(true) {
        return Ok(());
    }
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
        crate::virtual_display::screen_is_locked,
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
    fn already_locked_does_not_issue_another_request() {
        assert!(request_and_confirm(
            || Some(true),
            || panic!("already locked"),
            || panic!("no wait")
        )
        .is_ok());
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
