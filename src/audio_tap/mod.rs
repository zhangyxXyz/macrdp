//! Optional Core Audio process tap. All HAL objects are private to this process;
//! their lifetime controls source muting without changing system volume/mute.
use anyhow::{bail, Result};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn macrdp_tap_open(rate: *mut f64, error: *mut i32) -> *mut c_void;
    fn macrdp_tap_read(handle: *mut c_void, left: *mut f32, right: *mut f32, capacity: u32) -> i32;
    fn macrdp_tap_format_unchanged(handle: *mut c_void) -> bool;
    fn macrdp_tap_close(handle: *mut c_void) -> i32;
}

pub struct Tap {
    handle: NonNull<c_void>,
    pub rate: f64,
    checked_at: Instant,
}
impl Tap {
    pub fn open() -> Result<Self> {
        let mut rate = 0.0;
        let mut error = 0;
        let handle = unsafe { macrdp_tap_open(&mut rate, &mut error) };
        let Some(handle) = NonNull::new(handle) else {
            bail!("Core Audio remote-only capture failed (OSStatus {error}); requires macOS 14.2+ and system audio recording permission for macrdp. Local output was not changed.");
        };
        tracing::info!(
            rate,
            "remote-only audio: private process tap started; system volume unchanged"
        );
        Ok(Self {
            handle,
            rate,
            checked_at: Instant::now(),
        })
    }
    pub async fn next(&mut self) -> Result<(Vec<f32>, Vec<f32>)> {
        let mut left = vec![0.0; 2048];
        let mut right = vec![0.0; 2048];
        loop {
            if self.checked_at.elapsed() >= Duration::from_secs(1) {
                self.checked_at = Instant::now();
                if !unsafe { macrdp_tap_format_unchanged(self.handle.as_ptr()) } {
                    bail!(
                        "Core Audio tap format changed or device disappeared; rebuilding capture"
                    );
                }
            }
            let n = unsafe {
                macrdp_tap_read(
                    self.handle.as_ptr(),
                    left.as_mut_ptr(),
                    right.as_mut_ptr(),
                    2048,
                )
            };
            if n < 0 {
                bail!("Core Audio tap supplied an unsupported buffer layout");
            }
            if n > 0 {
                left.truncate(n as usize);
                right.truncate(n as usize);
                return Ok((left, right));
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }
}
impl Drop for Tap {
    fn drop(&mut self) {
        let status = unsafe { macrdp_tap_close(self.handle.as_ptr()) };
        if status != 0 {
            tracing::error!(
                status,
                "Core Audio tap cleanup failed; restart macrdp to release private audio objects"
            );
        } else {
            tracing::info!(
                "remote-only audio: private process tap released; local playback restored"
            );
        }
    }
}
