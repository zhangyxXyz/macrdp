//! Compatibility cleanup for remote file publications. Sparse presenter files
//! are no longer created: File Provider supplies dataless URLs, with a complete-
//! file fallback. Keep the existing cleanup hooks and stale-dir reaper.
#![cfg(target_os = "macos")]
use crate::file_promise::SelfChangeCount;
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc, Mutex, OnceLock},
};
use tracing::{debug, info};
pub(crate) fn cleanup_previous(current_temp_dir: &Arc<Mutex<Option<PathBuf>>>) {
    if let Some(old) = current_temp_dir.lock().unwrap().take() {
        let _ = std::fs::remove_dir_all(old);
    }
}
static SHUTDOWN_STATE: OnceLock<(Arc<Mutex<Option<PathBuf>>>, SelfChangeCount)> = OnceLock::new();

/// Called once by `MacCliprdr::new` to publish the factory's shared
/// temp-dir + self-change-count tracking into the global so the signal
/// handler can find them. Subsequent calls are no-ops (OnceLock).
pub fn register_shutdown_cleanup(
    paste_temp_dir: Arc<Mutex<Option<PathBuf>>>,
    self_change_count: SelfChangeCount,
) {
    let _ = SHUTDOWN_STATE.set((paste_temp_dir, self_change_count));
}

/// Best-effort cleanup invoked from the SIGINT/SIGTERM watcher in
/// `main.rs` immediately before `std::process::exit(0)`. Equivalent to
/// what `MacCliprdrBackend::drop` would do if Drop ran, but Drop doesn't
/// run on signal exit. Safe to call multiple times.
pub fn shutdown_cleanup() {
    if let Some((temp_dir, self_cc)) = SHUTDOWN_STATE.get() {
        cleanup_on_disconnect(temp_dir, self_cc);
    }
}

/// Clear only our own stale remote file URL publication.
pub fn clear_pasteboard_if_stale(self_change_count: &SelfChangeCount) {
    let our_cc = self_change_count.load(Ordering::Relaxed);
    if our_cc < 0 {
        return;
    }
    // Hold the pasteboard guard across the changeCount check AND the clear so
    // it's atomic (no other writer can bump changeCount between them) and race-
    // free against the advertise poller (NSPasteboard is not thread-safe).
    let _pb_guard = crate::clipboard::pasteboard_guard();
    let current_cc = unsafe { crate::clipboard::pasteboard().changeCount() } as i64;
    if current_cc != our_cc {
        // Something else owns the clipboard now — leave it alone.
        return;
    }
    unsafe {
        let pb = crate::clipboard::pasteboard();
        pb.clearContents();
        let new_cc = pb.changeCount() as i64;
        // Record as a self-write so the change-count poller doesn't see
        // the empty pasteboard and bounce it back to Windows as a fresh
        // Mac-side copy.
        self_change_count.store(new_cc, Ordering::Relaxed);
    }
    info!(
        cleared_cc = our_cc,
        "pasteboard cleared: Windows clipboard transitioned but no FileGroupDescriptorW \
         followed (likely a shell-extension archive type — .zip/.gz/.7z/etc)"
    );
}

/// Reclaim completed-file staging and clear only our own publication.
pub fn cleanup_on_disconnect(
    current_temp_dir: &Arc<Mutex<Option<PathBuf>>>,
    self_change_count: &SelfChangeCount,
) {
    cleanup_previous(current_temp_dir);

    // Only clear the pasteboard if its current changeCount still matches
    // what we set the last time we wrote — otherwise some other app (or
    // the user) has overwritten the clipboard since, and we'd be
    // clobbering their content.
    let our_cc = self_change_count.load(Ordering::Relaxed);
    if our_cc < 0 {
        // We never published; nothing to clear.
        return;
    }
    // Atomic check-then-clear under the shared pasteboard guard (NSPasteboard is
    // not thread-safe; this Drop path races the advertise poller during churn).
    let _pb_guard = crate::clipboard::pasteboard_guard();
    let current_cc = unsafe { crate::clipboard::pasteboard().changeCount() } as i64;
    if current_cc == our_cc {
        unsafe {
            // clearContents bumps changeCount; record it as a self-write
            // so the change-count poller doesn't read the now-empty
            // pasteboard and bounce it back to Windows as a fresh copy.
            let pb = crate::clipboard::pasteboard();
            pb.clearContents();
            let new_cc = pb.changeCount() as i64;
            self_change_count.store(new_cc, Ordering::Relaxed);
        }
        info!(
            cleared_cc = our_cc,
            "file clipboard: pasteboard cleared on disconnect (our URLs were stale)"
        );
    } else {
        debug!(
            our_cc,
            current_cc,
            "file clipboard: skipping pasteboard clear (something else owns the clipboard now)"
        );
    }
}

pub fn reap_stale() {
    crate::reaper::for_each_stale(
        &std::env::temp_dir(),
        "macrdp-lazy-paste-",
        false,
        std::process::id() as i32,
        &crate::reaper::process_is_alive,
        &mut |dir| {
            let _ = std::fs::remove_dir_all(dir);
            debug!(stale_dir = ?dir, "lazy paste: reaped temp dir from a dead prior macrdp process");
        },
    );
}
