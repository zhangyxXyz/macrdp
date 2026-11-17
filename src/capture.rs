//! Screen capture for the RDP display layer.
//!
//! macOS uses ScreenCaptureKit via the `screencapturekit` crate. Other targets
//! get a static-rectangle stub so the protocol layer still builds and can be
//! exercised on Linux CI.

use std::num::{NonZeroU16, NonZeroUsize};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use bytes::Bytes;
use ironrdp_server::{
    BitmapUpdate, DesktopSize, DisplayUpdate, PixelFormat, RdpServerDisplay,
    RdpServerDisplayUpdates, ServerError, ServerErrorExt as _, ServerResult,
};

/// The server's display traits use its own error type; capture code uses anyhow.
/// Keeps the whole context chain in the message (the server logs it with `{:#}`).
fn display_error(error: anyhow::Error) -> ServerError {
    ServerError::reason("display capture", format!("{error:#}"))
}
use tokio::sync::Notify;

use std::sync::atomic::AtomicU64;
use std::time::{Duration, Instant};

use crate::cursor::CursorState;

/// Tuning for the H.264 on-demand-keyframe feature (`--keyframe-on-change`).
/// Built from CLI flags in `main.rs`; all percentages are of the full frame.
#[derive(Clone, Copy, Debug)]
pub struct KeyframeOnChange {
    /// Master switch (`--keyframe-on-change` / `--no-keyframe-on-change`).
    pub enabled: bool,
    /// Dirty-area threshold at or above which an immediate keyframe is forced.
    /// A change this large (window raised to front, scroll, app launch) renders
    /// as a big P-frame that some clients resolve cleanly only on the next
    /// periodic IDR; an on-demand IDR lands it at once. Typing/caret changes are
    /// a few percent and stay below this. Fired only on the rising edge (see
    /// `kf_armed`), so this is "a large change *began*", not "is ongoing".
    pub change_pct: u64,
    /// Lowered threshold used briefly after a mouse click. A click often drives a
    /// moderate change (dropdown, dialog, button repaint) below the normal bar;
    /// the IDR still only fires if a real change follows, so no-op clicks cost
    /// nothing.
    pub click_pct: u64,
    /// How long after a click the lowered `click_pct` threshold applies.
    pub click_window: Duration,
}

/// Shared click→keyframe hint. `input.rs` records a mouse-down timestamp; the
/// H.264 capture path lowers its keyframe dirty-area threshold for a short
/// window afterward (see [`KEYFRAME_CHANGE_PCT_POST_CLICK`]). Cheap to clone
/// (one `Arc`); the timestamp is a relaxed atomic since exact ordering doesn't
/// matter for a heuristic.
#[derive(Clone)]
pub struct ClickSignal {
    inner: Arc<ClickInner>,
}

struct ClickInner {
    epoch: Instant,
    /// Milliseconds since `epoch` of the last click; 0 means "never clicked".
    last_click_ms: AtomicU64,
}

impl ClickSignal {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(ClickInner {
                epoch: Instant::now(),
                last_click_ms: AtomicU64::new(0),
            }),
        }
    }

    /// Record a click "now". Called from the input handler on mouse-button down.
    pub fn record_click(&self) {
        let ms = self.inner.epoch.elapsed().as_millis() as u64;
        // max(1) so a click at t≈0 isn't mistaken for "never".
        self.inner.last_click_ms.store(ms.max(1), Ordering::Relaxed);
    }

    /// True if the last click was within `window` of now.
    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    pub fn within(&self, window: Duration) -> bool {
        let last = self.inner.last_click_ms.load(Ordering::Relaxed);
        if last == 0 {
            return false;
        }
        let now = self.inner.epoch.elapsed().as_millis() as u64;
        now.saturating_sub(last) <= window.as_millis() as u64
    }
}

impl Default for ClickSignal {
    fn default() -> Self {
        Self::new()
    }
}

fn pack_size(width: u16, height: u16) -> u32 {
    (u32::from(width) << 16) | u32::from(height)
}

fn unpack_size(packed: u32) -> (u16, u16) {
    ((packed >> 16) as u16, (packed & 0xFFFF) as u16)
}

/// Debounce window for a client-driven live resize (MS-RDPEDISP monitor-layout
/// PDU, sent by the client when its window is resized mid-session). A drag can
/// emit several layout PDUs a second; applying one is a full RDP core
/// deactivation-reactivation (fresh SCK stream, resized framebuffer), so we
/// wait for the drag to settle rather than resizing on every tick.
pub const RESIZE_DEBOUNCE: Duration = Duration::from_millis(400);

/// A client-driven live-resize request (MS-RDPEDISP), debounced.
/// `CaptureDisplay::request_layout` calls [`PendingResize::request`] on every
/// monitor-layout PDU the client sends; the capture loop calls
/// [`PendingResize::take_settled`] each iteration and only acts once
/// [`RESIZE_DEBOUNCE`] has passed since the *last* PDU, so a multi-tick drag
/// produces one resize, not one per tick. The size and receive time are
/// published together under a short mutex so they cannot be read out of step.
#[derive(Clone)]
pub struct PendingResize {
    inner: Arc<PendingResizeInner>,
}

struct PendingResizeInner {
    // Size and timestamp are one publication: two independent relaxed atomics
    // can let the reader consume a new size against the previous timestamp.
    latest: std::sync::Mutex<Option<(Instant, (u16, u16))>>,
}
impl PendingResize {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(PendingResizeInner {
                latest: std::sync::Mutex::new(None),
            }),
        }
    }
    pub fn request(&self, width: u16, height: u16) {
        *self.inner.latest.lock().unwrap_or_else(|p| p.into_inner()) =
            Some((Instant::now(), (width, height)));
    }
    pub fn clear(&self) {
        *self.inner.latest.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
    pub fn has_pending(&self) -> bool {
        self.inner
            .latest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_some()
    }
    pub fn take_settled(&self, debounce: Duration) -> Option<(u16, u16)> {
        let mut latest = self.inner.latest.lock().unwrap_or_else(|p| p.into_inner());
        if latest
            .as_ref()
            .is_some_and(|(when, _)| when.elapsed() >= debounce)
        {
            latest.take().map(|(_, size)| size)
        } else {
            None
        }
    }
}

/// Connection-scoped transient display state. Reactivation deliberately does
/// not begin a new session; only the server's fresh-connection hook does.
pub struct DisplayLifecycle {
    epoch: AtomicU64,
    sequence: AtomicU64,
    layout_generation: AtomicU64,
    active: AtomicBool,
    pending: PendingResize,
    suppress: Arc<AtomicBool>,
}
impl DisplayLifecycle {
    pub fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
            sequence: AtomicU64::new(0),
            layout_generation: AtomicU64::new(0),
            active: AtomicBool::new(false),
            pending: PendingResize::new(),
            suppress: Arc::new(AtomicBool::new(false)),
        }
    }
    pub fn pending_resize(&self) -> PendingResize {
        self.pending.clone()
    }
    pub fn suppress_next_adopt(&self) -> Arc<AtomicBool> {
        self.suppress.clone()
    }
    pub fn begin(&self, scale: Option<u32>) {
        self.active.store(false, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
        self.sequence.store(0, Ordering::SeqCst);
        self.layout_generation.store(0, Ordering::SeqCst);
        self.pending.clear();
        self.suppress.store(false, Ordering::SeqCst);
        self.active.store(true, Ordering::SeqCst);
        if let Some(stats) = crate::stats::global() {
            stats.connected.store(true, Ordering::Relaxed);
            stats.update_display(|d| {
                *d = Default::default();
                d.session_generation = self.epoch.load(Ordering::SeqCst);
                d.requested_scale = scale;
                d.phase = "applying";
            });
        }
    }
    pub fn end(&self) {
        self.active.store(false, Ordering::SeqCst);
        self.pending.clear();
        self.suppress.store(false, Ordering::SeqCst);
        if let Some(stats) = crate::stats::global() {
            stats.connected.store(false, Ordering::Relaxed);
            stats.update_display(|d| {
                let generation = d.session_generation;
                *d = Default::default();
                d.session_generation = generation;
            });
        }
    }
    fn request(&self) -> (u64, u64) {
        let seq = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
        let token = (self.epoch.load(Ordering::SeqCst), seq);
        if let Some(stats) = crate::stats::global() {
            stats.update_display(|d| {
                d.session_generation = token.0;
                d.request_sequence = token.1;
            });
        }
        token
    }
    fn advance_layout(&self) -> (u64, u64) {
        self.layout_generation.fetch_add(1, Ordering::SeqCst);
        let token = self.token();
        if let Some(stats) = crate::stats::global() {
            stats.update_display(|d| d.layout_generation = token.1);
        }
        token
    }
    fn token(&self) -> (u64, u64) {
        (
            self.epoch.load(Ordering::SeqCst),
            self.layout_generation.load(Ordering::SeqCst),
        )
    }
    fn is_current(&self, token: (u64, u64)) -> bool {
        self.active.load(Ordering::SeqCst) && self.token() == token
    }
}
impl Default for DisplayLifecycle {
    fn default() -> Self {
        Self::new()
    }
}

impl Default for PendingResize {
    fn default() -> Self {
        Self::new()
    }
}

/// Live session desktop size, shared by every component that must agree on
/// it: `CaptureDisplay` (SCK capture size + `RdpServerDisplay::size`),
/// `MacInputHandler` (mouse-coordinate scaling), and the H.264 `Gfx`
/// pipeline (EGFX surface + encoder dimensions). Starts at the size resolved
/// in `main.rs`; mutated only by `request_initial_size` when the
/// client-resolution auto-adopt is active. Packed into one `AtomicU32`
/// (width << 16 | height) so readers on the per-event input path get a
/// coherent pair without a lock.
#[derive(Clone)]
pub struct SharedDesktopSize {
    packed: Arc<AtomicU32>,
    /// Input keeps the previous negotiated size until the matching display
    /// mode has settled. Publishing a new wire size alone is not a mapping.
    input_packed: Arc<AtomicU32>,
    /// True when capture is letterboxing/pillarboxing (preserving the Mac's
    /// aspect ratio inside a differently-shaped client frame) rather than
    /// stretching to fill. `input.rs` reads this so mouse coords map into the
    /// centered content sub-rect instead of the whole frame.
    letterbox: Arc<AtomicBool>,
}

impl SharedDesktopSize {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            packed: Arc::new(AtomicU32::new(pack_size(width, height))),
            input_packed: Arc::new(AtomicU32::new(pack_size(width, height))),
            letterbox: Arc::new(AtomicBool::new(false)),
        }
    }

    pub fn get(&self) -> (u16, u16) {
        unpack_size(self.packed.load(Ordering::Relaxed))
    }

    pub fn set(&self, width: u16, height: u16) {
        self.packed
            .store(pack_size(width, height), Ordering::Relaxed);
    }

    pub fn get_for_input(&self) -> (u16, u16) {
        unpack_size(self.input_packed.load(Ordering::Acquire))
    }

    pub fn commit_input_size(&self, width: u16, height: u16) {
        self.input_packed
            .store(pack_size(width, height), Ordering::Release);
    }

    pub fn set_letterbox(&self, on: bool) {
        self.letterbox.store(on, Ordering::Relaxed);
    }

    pub fn letterbox(&self) -> bool {
        self.letterbox.load(Ordering::Relaxed)
    }
}

/// Live RDP-session counter. ironrdp_server calls `RdpServerDisplay::updates()`
/// once per accepted client connection and drops the returned stream when the
/// connection ends, so wrapping that stream is the natural place to count
/// "how many clients are currently consuming our frames." Used by the
/// `--detach-primary` session-transition watcher in `main.rs` to disable the
/// physical displays only while at least one client is connected, and re-
/// attach them as soon as the last client disconnects.
#[derive(Clone, Default)]
pub struct SessionTracker {
    /// Incremented on every capture entry, including reconnects entirely between watcher polls.
    pub connection_epoch: Arc<AtomicU64>,
    pub count: Arc<AtomicUsize>,
    /// Notified whenever `count` changes. `tokio::sync::Notify` is
    /// edge-triggered; we wake one waiter per state transition, which
    /// is what the watchdog needs.
    pub notify: Arc<Notify>,
}

impl SessionTracker {
    fn enter(&self) {
        self.connection_epoch.fetch_add(1, Ordering::SeqCst);
        let prev = self.count.fetch_add(1, Ordering::SeqCst);
        tracing::info!(prev, now = prev + 1, "SessionTracker::enter");
        self.notify.notify_one();
    }
    fn leave(&self) {
        let prev = self.count.fetch_sub(1, Ordering::SeqCst);
        tracing::info!(prev, now = prev.saturating_sub(1), "SessionTracker::leave");
        self.notify.notify_one();
    }
}

/// Wraps an `RdpServerDisplayUpdates` so its lifetime drives a
/// `SessionTracker`. The inner trait is what ironrdp_server polls;
/// our wrapper just forwards `next_update` and bumps the counter on
/// new/drop. Zero overhead per frame — only construction and drop touch
/// the atomic.
struct CountedUpdates {
    // `+ Send` so `next_update`'s async future is Send (ironrdp_server uses
    // it under tokio::select!). The concrete inner types satisfy Send;
    // the trait object would otherwise lose it.
    inner: Box<dyn RdpServerDisplayUpdates + Send>,
    tracker: SessionTracker,
}

impl CountedUpdates {
    fn new(inner: Box<dyn RdpServerDisplayUpdates + Send>, tracker: SessionTracker) -> Self {
        tracker.enter();
        Self { inner, tracker }
    }
}

impl Drop for CountedUpdates {
    fn drop(&mut self) {
        self.tracker.leave();
    }
}

#[async_trait::async_trait]
impl RdpServerDisplayUpdates for CountedUpdates {
    async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
        self.inner.next_update().await
    }
}

pub struct CaptureDisplay {
    /// Opt-in preparation, reached only after RDP authentication/ARC validation.
    pub preparation: Option<crate::session_lock::CapturePreparation>,
    /// Session desktop size — shared with the input handler and the H.264
    /// pipeline so all three stay in sync when `auto_size` adopts the
    /// client's requested resolution.
    pub desktop_size: SharedDesktopSize,
    /// Adopt the resolution the client asks for (its Confirm Active bitmap
    /// capset, e.g. mstsc full-screen at 1920×1080) instead of always
    /// serving the size resolved at startup. Serving the client's exact
    /// resolution means the client never rescales the decoded video —
    /// client-side rescaling on mstsc costs typing latency and, with
    /// `--enable-h264`, audio desync. Enabled by default on the
    /// mirror-primary path when no explicit `--width`/`--height`/`--hidpi`
    /// was given; `--no-client-resolution` opts out.
    ///
    /// The actual size negotiation happens in `ironrdp-acceptor`
    /// (`set_honor_client_desktop_size`, wired via the server builder's
    /// `with_honor_client_desktop_size`): the client's true
    /// request is only visible in its GCC Client Core Data, and the
    /// acceptor commits a size in Demand Active before any server code
    /// runs. This flag's job is the receiving end — adopt the negotiated
    /// size (echoed back through `request_initial_size`) into
    /// `desktop_size` so capture, input scaling, and the H.264 pipeline
    /// all serve it.
    pub auto_size: bool,
    pub live_resize: bool,
    pub lifecycle: Arc<DisplayLifecycle>,
    /// Opt out of aspect-preserving letterbox on the auto-size path: stretch the
    /// Mac screen to fill the client frame (the old default) instead of adding
    /// black bars. Only relevant when `auto_size` is scaling to a non-native
    /// aspect; ignored otherwise.
    pub stretch: bool,
    pub fps: u32,
    /// `Some(CGDirectDisplayID)` captures that specific display (e.g. a
    /// `VirtualDisplay`); `None` captures the first SCK display, which
    /// is the user's primary panel. SCK's enumeration order isn't
    /// formally documented as "main first," but in practice it is —
    /// and we only fall through to it when the caller didn't ask for
    /// anything specific.
    pub display_id: Option<u32>,
    /// Target display's logical size in points — fed through to
    /// `CursorState` for the (currently disabled) position-polling
    /// path. Caller queries it from `CGDisplay::main()` for the
    /// primary path, or from `VirtualDisplay::size_pts()`.
    pub screen_size_pts: (f64, f64),
    /// True when serving a `--virtual-display`. On disconnect the capture
    /// session warps the cursor back onto the primary physical display, so it
    /// isn't left stranded in the (off-panel) virtual-display coordinate region.
    pub warp_cursor_home: bool,
    /// User-facing cursor size multiplier (`--cursor-scale`, default 1.0).
    /// Multiplies the automatic backing→session pointer downscale; lets the
    /// user enlarge the pointer for comfort without affecting hotspot accuracy.
    pub cursor_scale: f64,
    /// Optional session tracker — when `Some`, the returned
    /// `RdpServerDisplayUpdates` is wrapped so its lifetime bumps the
    /// counter that drives the `--detach-primary` session-transition
    /// watcher. `None` disables the wrap entirely (zero overhead).
    pub session_tracker: Option<SessionTracker>,
    /// EGFX/H.264 frame sink (macOS-only, opt-in via `--enable-h264`). When
    /// `Some`, every captured SCK frame is submitted to the H.264 encoder; once
    /// EGFX has negotiated, the legacy BitmapUpdate path is suppressed and the
    /// display is served entirely over EGFX.
    #[cfg(target_os = "macos")]
    pub gfx: Option<crate::h264::Gfx>,
    /// On-demand-keyframe tuning (`--keyframe-on-change` and its threshold
    /// flags). When disabled, the periodic `--keyframe-interval` is the only IDR
    /// driver besides the forced first frame.
    pub keyframe_on_change: KeyframeOnChange,
    /// Shared mouse-click hint, used only when `keyframe_on_change.enabled`. Lets
    /// the H.264 path lower its keyframe threshold briefly after a click — see
    /// [`ClickSignal`].
    pub click_signal: Option<ClickSignal>,
    /// Trailing flush frames re-sent after the last change to drain mstsc's
    /// presentation buffer (`--flush-frames`; EGFX/H.264 path only). 0 disables.
    pub flush_frames: u32,
    /// `--capture-nv12` (experimental): ScreenCaptureKit delivers NV12 that the
    /// encoder takes as-is, instead of BGRA it converts on the CPU. No effect
    /// without H.264.
    pub capture_nv12: bool,
    /// Shared "client minimized" flag from the vendor server's
    /// `SuppressOutput` handler. When set, `next_update` short-circuits
    /// (no SCK pull, no encode, no ship) and waits on a short timer
    /// until the flag clears, at which point the H.264 path forces an
    /// IDR keyframe so the client can resume from a clean reference
    /// frame. `None` disables the gate entirely (single-binary builds
    /// where the server doesn't expose the handle — e.g., the non-macOS
    /// stub).
    pub display_suppressed: Option<Arc<AtomicBool>>,
    /// Client-driven live-resize request in flight (MS-RDPEDISP monitor
    /// layout — the client resized its window mid-session). `request_layout`
    /// writes to it; the capture loop polls + debounces it into an actual
    /// `DisplayUpdate::Resize` (with an EGFX state reset first on the H.264
    /// path — see `Gfx::reset_for_live_resize`). Works on the auto-adopt
    /// path (both codec paths) and, via `virtual_display` below, on the
    /// virtual-display path.
    pub pending_resize: PendingResize,
    /// Operator ceiling for a live client-requested resize (`--max-client-size`),
    /// mirroring the connect-time cap the vendored acceptor applies. `None` =
    /// no cap beyond the protocol max (still enforced by `adopt_client_size`).
    pub max_client_size: Option<DesktopSize>,
    /// Set by the capture loop just before it emits a server-driven
    /// `DisplayUpdate::Resize` (blank recovery's same-size reactivate, or a
    /// settled `pending_resize`); consumed by `request_initial_size` on the
    /// very next call to skip re-adopting the client's reactivation-echo
    /// size. See `request_initial_size`'s doc comment for why this exists.
    pub suppress_next_adopt: Arc<AtomicBool>,
    /// The `--virtual-display` this session serves, when there is one.
    /// Shared with `main.rs`; `updates()` acquires its session lease and re-modes it to
    /// match the negotiated size and independent scale policy before capture.
    pub virtual_display: Option<crate::display_session::SharedDisplay>,
    /// When the virtual display runs HiDPI (`--virtual-display-hidpi`). With
    /// `Auto`, decided per connection from the client's desktop scale factor
    /// in `sync_virtual_display`.
    pub vd_hidpi: crate::virtual_display::HidpiPolicy,
    /// Measured wire pixels per point, in percent, for the pointer. Published
    /// after mode application and read live by the capture
    /// loop so a scale change resends the pointer.
    pub pointer_ui_scale: Arc<std::sync::atomic::AtomicU32>,

    /// The `--capture-primary` blanking guard (shared with main.rs), so a live
    /// re-mode can **re-assert the gamma-black** after it: a display
    /// reconfiguration resets gamma tables, un-blanking the physical panel
    /// (the desktop shows through on resize). `None` unless `--capture-primary`
    /// is active. See `sync_virtual_display`.
    pub captured_primary:
        Option<Arc<std::sync::Mutex<Option<crate::virtual_display::CapturedPrimary>>>>,

    /// The `--shield-primary` blanking guard (shared with main.rs), re-fitted
    /// after a live re-mode for the same reason as `captured_primary` above —
    /// but a weaker one. A shield WINDOW survives a display reconfiguration
    /// (that's the point of the mode), so this is not required for correctness
    /// the way the gamma re-assert is. It is wired anyway because the helper's
    /// own `didChangeScreenParameters` observer cannot be trusted here: this
    /// codebase already established that the private `CGVirtualDisplay
    /// applySettings` re-mode does NOT emit a public display-reconfiguration
    /// event (a `CGDisplayRegisterReconfigurationCallback` never fired across
    /// many resizes — see the removed-callback NOTE in virtual_display/mod.rs),
    /// so the AppKit notification very likely does not fire either. Re-fitting
    /// from the capture path does not depend on any notification. `None` unless
    /// `--shield-primary` is active.
    pub shielded_primary:
        Option<Arc<std::sync::Mutex<Option<crate::virtual_display::ShieldedPrimary>>>>,
}

/// Which of ScreenCaptureKit's displays to mirror when no virtual display is
/// set: the main display, which is the one input maps the mouse onto
/// (`CGDisplay::main()`), falling back to the first. SCK doesn't list the main
/// display first on every multi-monitor Mac, and mirroring another one put the
/// picture on one monitor and the mouse on a different one (#205). Pure, so
/// it's unit-tested on every target.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn mirror_display_index(display_ids: &[u32], main_id: u32) -> Option<usize> {
    if display_ids.is_empty() {
        return None;
    }
    Some(
        display_ids
            .iter()
            .position(|&id| id == main_id)
            .unwrap_or(0),
    )
}

/// Vendor id on every macrdp virtual display descriptor ("macr").
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const VIRTUAL_DISPLAY_VENDOR: u32 = 0x6D616372;

/// One display SCK lists, as the capture reopen sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
struct OnlineDisplay {
    id: u32,
    vendor: u32,
    builtin: bool,
    size_px: (u32, u32),
}

/// The id a lost virtual display came back under: the same id, else the only
/// display with our vendor id, else the only external display of the session's
/// size. `None` while it is not back. Waking can make WindowServer re-register
/// the virtual display under a new id, and the old one never returns.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn pick_reopened_display(old: u32, online: &[OnlineDisplay], size_px: (u32, u32)) -> Option<u32> {
    if online.iter().any(|d| d.id == old) {
        return Some(old);
    }
    let unique = |f: &dyn Fn(&OnlineDisplay) -> bool| {
        let mut it = online.iter().filter(|d| f(d));
        match (it.next(), it.next()) {
            (Some(d), None) => Some(d.id),
            _ => None,
        }
    };
    unique(&|d| d.vendor == VIRTUAL_DISPLAY_VENDOR)
        .or_else(|| unique(&|d| !d.builtin && d.size_px == size_px))
}

#[cfg(any(target_os = "macos", test))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureStop {
    MissingSource,
    Other,
}

#[cfg(any(target_os = "macos", test))]
const CAPTURE_RECOVERY_WINDOW: Duration = Duration::from_secs(3);

#[cfg(any(target_os = "macos", test))]
#[derive(Debug, PartialEq, Eq)]
enum CaptureRecovery<T> {
    Restored(T),
    LockedDisplaySleep,
    TimedOut,
}

/// Only called after a real stream stop. Idle, blank and suppressed samples
/// still use the ordinary capture loop and never start this timer.
#[cfg(any(target_os = "macos", test))]
async fn recover_capture<T, F>(
    mut stop: CaptureStop,
    mut locked_and_asleep: impl FnMut() -> bool,
    mut reopen: impl FnMut() -> F,
) -> CaptureRecovery<T>
where
    F: std::future::Future<Output = Result<Option<T>, CaptureStop>>,
{
    tokio::time::timeout(CAPTURE_RECOVERY_WINDOW, async {
        loop {
            if stop == CaptureStop::MissingSource && locked_and_asleep() {
                return CaptureRecovery::LockedDisplaySleep;
            }
            // Let display handoffs settle. A sleep notification can arrive
            // just after ScreenCaptureKit's stop callback.
            tokio::time::sleep(Duration::from_millis(500)).await;
            if stop == CaptureStop::MissingSource && locked_and_asleep() {
                return CaptureRecovery::LockedDisplaySleep;
            }
            match reopen().await {
                Ok(Some(stream)) => return CaptureRecovery::Restored(stream),
                Ok(None) => {}
                Err(next_stop) => stop = next_stop,
            }
        }
    })
    .await
    .unwrap_or(CaptureRecovery::TimedOut)
}

#[cfg(target_os = "macos")]
pub use macos::observe_display_sleep;

/// Look up the primary display's pixel dimensions via ScreenCaptureKit.
///
/// Returns `None` on non-macOS targets so the caller can fall back to a stub
/// default. On macOS, failures surface as `Err` because they almost always
/// mean Screen Recording permission is missing — that's a setup problem the
/// user needs to see, not silently paper over.
pub async fn primary_display_size() -> Result<Option<(u16, u16)>> {
    #[cfg(target_os = "macos")]
    {
        use anyhow::{anyhow, Context};
        use screencapturekit::async_api::AsyncSCShareableContent;

        let content = AsyncSCShareableContent::get().await.map_err(|e| {
            anyhow!("AsyncSCShareableContent::get failed (Screen Recording permission?): {e:?}")
        })?;
        let displays = content.displays();
        let ids: Vec<u32> = displays.iter().map(|d| d.display_id()).collect();
        let main_id = core_graphics::display::CGDisplay::main().id;
        let display = mirror_display_index(&ids, main_id)
            .map(|i| &displays[i])
            .context("no displays available")?;
        let w = u16::try_from(display.width()).context("display width > u16")?;
        let h = u16::try_from(display.height()).context("display height > u16")?;
        Ok(Some((w, h)))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(None)
    }
}

/// Decide whether the auto-size path should adopt the client's requested
/// desktop size. Pure (no platform deps, no shared state) so it's unit-tested
/// on every target.
///
/// With `auto_size` on, the vendored acceptor has already negotiated the
/// client's requested size (from its Client Core Data) in Demand Active, and
/// the `client` size here is the client's Confirm Active echo of that. We adopt
/// it so capture, input scaling, and the H.264 pipeline all serve the
/// negotiated resolution. The `200..=8192` band is the protocol-legal desktop
/// range (MS-RDPBCGR); an echo outside it is garbage we refuse to adopt, and a
/// no-op echo (already current) needs no change.
///
/// Returns `Some(adopted)` to switch, or `None` to keep the current size.
fn adopt_client_size(
    auto_size: bool,
    current: (u16, u16),
    client: DesktopSize,
) -> Option<DesktopSize> {
    if auto_size
        && (client.width, client.height) != current
        && (200..=8192).contains(&client.width)
        && (200..=8192).contains(&client.height)
    {
        Some(client)
    } else {
        None
    }
}

/// Clamp a client-requested size to `--max-client-size`, per dimension.
///
/// The acceptor clamps the size it negotiates in Demand Active, but
/// `request_initial_size` receives the client's Confirm Active echo, which a
/// client can fill with its own requested size instead (mstsc asking for
/// 3840x2160 was observed doing so, issue #211). The echo is client-controlled,
/// so the ceiling has to be applied here too, before anything is sized from it.
fn cap_client_size(client: DesktopSize, max: Option<DesktopSize>) -> DesktopSize {
    match max {
        Some(max) => DesktopSize {
            width: client.width.min(max.width),
            height: client.height.min(max.height),
        },
        None => client,
    }
}

fn pointer_factor(scale_percent: u32) -> f64 {
    f64::from(scale_percent) / 100.0
}

fn aspect_preserving_scale(
    actual: &crate::virtual_display::DisplayMeasurement,
    wire: (u16, u16),
) -> Option<f64> {
    actual
        .points_width
        .zip(actual.points_height)
        .map(|(w, h)| (f64::from(wire.0) / w).min(f64::from(wire.1) / h) * 100.0)
}

fn mode_fallback(
    actual: &crate::virtual_display::DisplayMeasurement,
    wire: (u16, u16),
    ui_scale: u32,
) -> Option<String> {
    let mut reasons = Vec::new();
    if actual
        .scale_percent
        .is_some_and(|s| (s - f64::from(ui_scale * 100)).abs() > 1.0)
    {
        reasons.push("macOS selected a different display scale (density fallback)");
    }
    if actual
        .backing_width
        .zip(actual.backing_height)
        .is_some_and(|(w, h)| u32::from(wire.0) > w || u32::from(wire.1) > h)
    {
        reasons.push("wire resolution exceeds actual backing pixels; capture is upscaled");
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// Resolution and scale decisions are independent. A fixed desktop or a
/// disabled live-resolution policy may still re-mode at the existing size.
fn layout_target(
    auto_size: bool,
    live_resize: bool,
    applied_scale: Option<u32>,
    desired_scale: u32,
    current: (u16, u16),
    requested: DesktopSize,
) -> Option<DesktopSize> {
    adopt_client_size(auto_size && live_resize, current, requested).or_else(|| {
        applied_scale
            .is_some_and(|applied| applied != desired_scale)
            .then_some(DesktopSize {
                width: current.0,
                height: current.1,
            })
    })
}

#[async_trait::async_trait]
impl RdpServerDisplay for CaptureDisplay {
    async fn size(&mut self) -> DesktopSize {
        let (width, height) = self.desktop_size.get();
        DesktopSize { width, height }
    }

    async fn request_initial_size(&mut self, client_size: DesktopSize) -> DesktopSize {
        let (width, height) = self.desktop_size.get();

        // This call also fires on every deactivation-reactivation, not just
        // the initial connect — including one WE just triggered (blank
        // recovery's same-size reactivate, or a client-driven live resize
        // via `pending_resize`). Empirically (live Windows App test,
        // 2026-07-09), the client's Confirm Active bitmap capset during such
        // a reactivation does NOT echo the size we just told it to use in
        // Demand Active — it reports some other value (looked like its
        // original connect-time size). Treating that as a fresh client
        // request re-adopts the wrong size and immediately undoes the
        // resize we just applied. So: when we ourselves armed this
        // reactivation, trust `desktop_size` (already set by the capture
        // loop before it emitted `DisplayUpdate::Resize`) and skip the
        // adopt entirely for this one call.
        if self.suppress_next_adopt.swap(false, Ordering::Relaxed) {
            tracing::debug!(
                width,
                height,
                echoed_client_w = client_size.width,
                echoed_client_h = client_size.height,
                "reactivation we triggered ourselves — keeping the server-driven \
                 size, not re-adopting the client's capset echo"
            );
            return DesktopSize { width, height };
        }

        self.lifecycle.request();
        // Confirm Active can echo the server's Demand Active dimensions.
        // It is negotiation input, not a reliable original client request:
        // leave requested dimensions unknown until a Monitor Layout arrives.

        let client_size = cap_client_size(client_size, self.max_client_size);
        if let Some(adopted) = adopt_client_size(self.auto_size, (width, height), client_size) {
            tracing::info!(
                client_w = adopted.width,
                client_h = adopted.height,
                prev_w = width,
                prev_h = height,
                "serving client-requested desktop resolution \
                 (--no-client-resolution disables)"
            );
            self.desktop_size.set(adopted.width, adopted.height);
            return adopted;
        }
        DesktopSize { width, height }
    }

    /// Handle a client-driven live resize (MS-RDPEDISP monitor layout — the
    /// client resized its window). The connect-time auto-adopt path only
    /// negotiates a size once, in `request_initial_size`; this is the
    /// live-in-session counterpart, wired through the vendored server's
    /// already-unconditional `DisplayControlServer` DVC (it decodes the PDU
    /// and calls this on every layout change — the default trait impl is a
    /// no-op debug log, which is why this used to silently do nothing).
    ///
    /// Resolution follows the same policy as `request_initial_size`. A fixed
    /// virtual display can still follow the client's scale independently of
    /// its fixed wire resolution. `live_resize` only controls resolution.
    /// Debounced by the capture loop via `pending_resize` so a window drag
    /// (many PDUs/sec) produces one resize, not one per tick. Both codec
    /// paths ride the same core deactivation-reactivation; on the EGFX
    /// (`--enable-h264`) path the capture loop additionally resets the
    /// per-connection surface/encoder state first so the post-reactivation
    /// rebuild starts from scratch at the new size — see
    /// `Gfx::reset_for_live_resize`'s doc comment for why (a channel-level
    /// surface swap without the core reactivation was tried first and was
    /// visually broken on a real client).
    fn request_layout(&mut self, layout: ironrdp_displaycontrol::pdu::DisplayControlMonitorLayout) {
        let Some(monitor) = layout
            .monitors()
            .iter()
            .find(|m| m.is_primary())
            .or_else(|| layout.monitors().first())
        else {
            tracing::debug!("client sent a monitor layout with no monitors — ignoring");
            return;
        };

        let (w, h) = monitor.dimensions();
        let (Ok(width), Ok(height)) = (u16::try_from(w), u16::try_from(h)) else {
            tracing::warn!(
                w,
                h,
                "client-requested monitor size out of range — ignoring"
            );
            return;
        };

        self.lifecycle.request();
        if let Some(stats) = crate::stats::global() {
            stats.update_display(|d| {
                d.requested_width = Some(width);
                d.requested_height = Some(height);
                d.request_source = Some("monitor_layout");
                if let Some(scale) = monitor.desktop_scale_factor() {
                    d.requested_scale = Some(scale);
                }
            });
        }
        // Scale is independent of resolution policy, including when live
        // resolution updates are disabled. Publish pointer scale only after
        // the actual mode is known, at the same boundary as input mapping.
        self.vd_hidpi
            .observe_client_scale(monitor.desktop_scale_factor());
        // Compare against the applied mode, not the preceding PDU. Repeated
        // 200% layouts during debounce must retain an unapplied 100→200 change.
        let applied_scale = self.virtual_display.as_ref().and_then(|vd| {
            vd.lock()
                .expect("virtual display mutex poisoned")
                .scale_if_active()
        });
        self.pending_resize.clear();
        let requested = cap_client_size(DesktopSize { width, height }, self.max_client_size);
        let (cur_w, cur_h) = self.desktop_size.get();
        let Some(adopted) = layout_target(
            self.auto_size,
            self.live_resize,
            applied_scale,
            self.vd_hidpi.ui_scale(),
            (cur_w, cur_h),
            requested,
        ) else {
            if let Some(stats) = crate::stats::global() {
                stats.update_display(|d| d.finish_unchanged_request((cur_w, cur_h)));
            }
            return;
        };
        if let Some(stats) = crate::stats::global() {
            stats.update_display(|d| d.phase = "applying");
        }

        tracing::info!(
            client_w = adopted.width,
            client_h = adopted.height,
            prev_w = cur_w,
            prev_h = cur_h,
            desktop_scale_factor = ?monitor.desktop_scale_factor(),
            ui_scale = self.vd_hidpi.ui_scale(),
            "client resized its window — resizing the session (debounced)"
        );
        self.pending_resize.request(adopted.width, adopted.height);
    }

    async fn updates(&mut self) -> ServerResult<Box<dyn RdpServerDisplayUpdates>> {
        // Authenticated callers only. A fully headless locked session needs a
        // display for loginwindow before password input, but still starts no
        // screen capture/input-size commit until preparation succeeds.
        let result = async {
            let bootstrap = self.preparation.is_some()
                && self.virtual_display.is_some()
                && crate::virtual_display::other_displays(
                    crate::display_session::active_display_id(),
                )? == crate::display_transition::OtherDisplays::None;
            let early_lease = if bootstrap {
                let vd = self
                    .virtual_display
                    .as_ref()
                    .expect("virtual display bootstrap");
                let lease = crate::display_session::ManagedDisplay::acquire(vd).await?;
                let id = vd
                    .lock()
                    .unwrap()
                    .id_if_active()
                    .context("bootstrap display missing")?;
                crate::session_lock::wait_for_unlock_surface(id).await?;
                Some(lease)
            } else {
                None
            };
            // The blocking password worker owns an early lease until it ends,
            // even if this connection stops awaiting its result.
            let (unlocked, mut lease) = if let Some(preparation) = &self.preparation {
                if early_lease.is_some() {
                    preparation.run_with_guard(early_lease).await?
                } else {
                    (preparation.run().await?, None)
                }
            } else {
                (false, early_lease)
            };
            if let Some(vd) = &self.virtual_display {
                if lease.is_none() {
                    lease = Some(crate::display_session::ManagedDisplay::acquire(vd).await?);
                }
                let vd = vd.lock().unwrap();
                self.display_id = vd.id_if_active();
                self.screen_size_pts = vd.size_pts();
            }
            // The sync block keeps the display mutex out of every await point.
            let (width, height) = self.sync_virtual_display();
            self.desktop_size.commit_input_size(width, height);
            crate::input::invalidate_display_bounds();
            if unlocked || lease.is_some() {
                crate::session_lock::wait_for_display(self.display_id).await?;
            }
            if lease.is_some() {
                if let Some(id) = self.display_id {
                    crate::display_session::mark_ready(id);
                }
            }
            let inner = self.build_updates(width, height).await?;
            Ok(match lease {
                Some(lease) => Box::new(crate::display_session::Updates::new(inner, lease))
                    as Box<dyn RdpServerDisplayUpdates>,
                None => inner,
            })
        }
        .await;
        if let Some(stats) = crate::stats::global() {
            stats.update_display(|d| {
                if let Err(error) = &result {
                    d.fallback_reason = Some(format!("capture failed: {error:#}"));
                }
                d.phase = if d.fallback_reason.is_some() {
                    "degraded"
                } else {
                    "active"
                };
            });
        }
        result.map_err(display_error)
    }
}

impl CaptureDisplay {
    /// How much to enlarge the pointer on the HiDPI virtual display, whose
    /// cursor SkyLight returns as the 1× image (measured: a 28×40 arrow whose
    /// NSCursor has 28×40 points, at both UI scales).
    ///
    /// The factor is the session's pixels per point: with "Optimize for
    /// Retina displays" on, Windows App draws session pixels (pointer
    /// included) 1:1, so a 2x session needs a 2x pointer and a 1x session a
    /// 1x one. With `Auto` it comes from `pointer_ui_scale` (percent),
    /// published from the measured mode; `On` uses session width over points.
    /// The pointer-scale cell for the capture loop to follow, with the
    /// `--cursor-scale` multiplier it applies on top. `Some` only for `Auto`
    /// on the HiDPI virtual display, where the pointer scale changes
    /// mid-capture.
    fn live_pointer_scale(&self) -> Option<(Arc<std::sync::atomic::AtomicU32>, f64)> {
        (self.virtual_display.is_some()
            && matches!(self.vd_hidpi, crate::virtual_display::HidpiPolicy::Auto(_)))
        .then(|| (self.pointer_ui_scale.clone(), self.cursor_scale))
    }

    fn pointer_scale(&self, _width: u16) -> f64 {
        if self.virtual_display.is_some() {
            // A density fallback can make points larger than the wire, even
            // though the display is no longer classified as Retina. It still
            // needs the measured fractional pointer factor (for example 0.5).
            pointer_factor(self.pointer_ui_scale.load(Ordering::Acquire))
        } else {
            1.0
        }
    }

    /// Keep the virtual display's mode in sync with the session size,
    /// returning the (possibly corrected) size to serve.
    /// [`RdpServerDisplay::updates`] runs at connect AND after every
    /// deactivation-reactivation — including the one a live client-driven
    /// resize just triggered (which is the only thing that changes
    /// `desktop_size` on the vd path, since auto-adopt is off there) — so
    /// re-moding here means the fresh capture stream opens against a display
    /// that is already the right size (a 1:1 native capture, no SCK scaling,
    /// no letterbox). On failure the session must still come up, so fall
    /// back to scaling the actual mode into the already-negotiated wire size.
    /// No-op without a virtual display (mirror-primary).
    fn publish_mode(
        &self,
        width: u16,
        height: u16,
        mut actual: crate::virtual_display::DisplayMeasurement,
        fallback: Option<String>,
    ) {
        let fitting_scale = aspect_preserving_scale(&actual, (width, height));
        if self.auto_size && !self.stretch {
            actual.scale_percent = fitting_scale;
        }
        // A stretched fallback has no single content scale. Keep the pointer
        // undistorted and fitting within both axes while telemetry stays null.
        if let Some(scale) = actual.scale_percent.or(fitting_scale) {
            self.pointer_ui_scale
                .store(scale.round().max(1.0) as u32, Ordering::Release);
        }
        if let Some(stats) = crate::stats::global() {
            stats.width.store(u32::from(width), Ordering::Relaxed);
            stats.height.store(u32::from(height), Ordering::Relaxed);
            stats.update_display(|d| {
                d.wire_width = Some(width);
                d.wire_height = Some(height);
                d.actual = actual;
                d.fallback_reason = fallback;
                d.phase = "applying";
            });
        }
    }

    fn sync_virtual_display(&mut self) -> (u16, u16) {
        let (width, height) = self.desktop_size.get();
        let Some(vd) = self.virtual_display.clone() else {
            self.publish_mode(width, height, Default::default(), None);
            return (width, height);
        };
        let mut vd = vd.lock().expect("virtual display mutex poisoned");
        let ui_scale = self.vd_hidpi.ui_scale();
        // Compare in pixels: on a HiDPI vd the points are the session size
        // divided by the UI scale.
        if vd.mode_px() == (u32::from(width), u32::from(height)) && vd.ui_scale() == ui_scale {
            let actual = vd.measurement(width, height);
            let fallback = mode_fallback(&actual, (width, height), ui_scale);
            self.publish_mode(width, height, actual, fallback);
            return (width, height);
        }
        // Invalidate old delayed sweeps at the mode-application boundary.
        // A duplicate/no-op PDU must not cancel a necessary post-mode sweep.
        self.lifecycle.advance_layout();
        match vd.resize(u32::from(width), u32::from(height), ui_scale) {
            Ok(()) => {
                let actual = vd.measurement(width, height);
                let fallback = mode_fallback(&actual, (width, height), ui_scale);
                self.publish_mode(width, height, actual, fallback);
                self.screen_size_pts = vd.size_pts();
                let vd_id = vd.display_id();
                tracing::info!(
                    width,
                    height,
                    ui_scale,
                    display_id = vd_id,
                    "virtual display re-moded to the client-requested session size"
                );
                // TWO post-re-mode fixes for the headless modes, with a
                // deliberate split on WHERE each runs — learned the hard way (each
                // has the OPPOSITE timing need):
                //   1. Re-anchor the vd at (0,0) as the system MAIN display,
                //      SYNCHRONOUSLY here on the capture path. A re-mode drifts the
                //      vd off (0,0) (back to its creation origin), moving the menu
                //      bar + Dock onto the blanked physical panel — they vanish
                //      over RDP. The re-anchor must be IMMEDIATE: done even a few
                //      hundred ms late (off-thread), the Dock has already settled
                //      on the physical panel and doesn't re-follow the main-display
                //      change. It's a fast CG config, so on-path is fine.
                //   2. Gather windows stranded off the re-moded display back onto
                //      it (the Ctrl+Alt+G sweep) — automatic because a resize is
                //      when the user expects their windows to follow. But the
                //      re-anchor is an arrangement change that triggers a
                //      WindowServer RELAYOUT, so the gather must run AFTER that
                //      settles (off-thread, ~0.7 s) and read the FINAL (0,0)
                //      bounds; too soon and it sweeps windows in a coordinate space
                //      that's about to shift → they land off-screen.
                // Headless-only (session_tracker is Some exactly for
                // --capture-primary/--detach-primary, where the vd is the only
                // visible display); skipped for plain --virtual-display, where a
                // window off the vd may be intentionally on the physical panel.
                #[cfg(target_os = "macos")]
                if self.session_tracker.is_some() {
                    // --shield-primary in keep-physical-main mode deliberately
                    // does NOT make the vd the system main display (so the lock
                    // screen stays on the visible physical panel). Re-anchoring
                    // it as main here would undo that on every resize, so skip it.
                    let skip_reanchor = self.shielded_primary.is_some()
                        && crate::virtual_display::shield_keeps_physical_main();
                    if !skip_reanchor {
                        if let Err(e) = vd.reanchor_as_main() {
                            tracing::warn!(error = ?e, "re-anchoring the vd as main after re-mode failed");
                        }
                    }
                    // 0. Re-assert the --capture-primary gamma blanking. The
                    //    re-mode (applySettings) and the re-anchor above are
                    //    display reconfigurations, and macOS RESETS the physical
                    //    panels' gamma tables on a reconfiguration — so the
                    //    all-black blanking installed at connect is lost and the
                    //    desktop shows through on resize. This immediate re-assert
                    //    (after the re-anchor, whose own reconfiguration would
                    //    otherwise undo an earlier one) covers the common case; the
                    //    LATE resets (capture re-engage / a final relayout after the
                    //    session re-enters, ~1 s in — the "quick blink after the
                    //    resize" a polling burst couldn't beat) are handled by the
                    //    post-sweep re-blanks in the thread below. (An event-driven
                    //    re-blank was tried and REMOVED: the private
                    //    CGVirtualDisplay applySettings resets gamma without
                    //    emitting a public reconfiguration event, so the callback
                    //    never fired — see the NOTE in virtual_display/mod.rs.)
                    //    No-op for --detach-primary (slot is None; that mode
                    //    disables the panel, not gamma).
                    if let Some(cap) = self.captured_primary.as_ref() {
                        if let Some(g) = cap.lock().expect("captured_primary poisoned").as_ref() {
                            let failed = g.reassert_blanking();
                            tracing::info!(
                                failed,
                                "re-asserted capture-primary blanking after re-mode"
                            );
                        }
                    }
                    // Same hook for --shield-primary: re-fit the shield windows to
                    // the panels' new frames. Cheap and idempotent (SHOW
                    // reconciles rather than stacking).
                    if let Some(sh) = self.shielded_primary.as_ref() {
                        if let Some(g) = sh.lock().expect("shielded_primary poisoned").as_ref() {
                            let failed = g.reassert_blanking();
                            tracing::info!(failed, "re-fitted shield windows after re-mode");
                        }
                    }
                    let captured_primary = self.captured_primary.clone();
                    let shielded_primary = self.shielded_primary.clone();
                    let lifecycle = self.lifecycle.clone();
                    let generation = lifecycle.token();
                    std::thread::spawn(move || {
                        // Re-blank after each window-gather sweep. DIAGNOSED from
                        // the callback trace: the vd re-mode resets the physical
                        // gamma WITHOUT emitting a CGDisplay reconfiguration event
                        // (it's the private CGVirtualDisplay applySettings), so an
                        // event-driven re-blank never fires — AND the sync
                        // re-assert on the capture path (before this thread) is
                        // undone ~700 ms later by the gather's own WindowServer
                        // relayout, which re-resets the gamma. That's the "quick
                        // blink a little less than a second after the resize". So
                        // the re-assert has to run AFTER each sweep, here.
                        let reblank = || {
                            if !lifecycle.is_current(generation) {
                                return;
                            }
                            if let Some(cap) = captured_primary.as_ref() {
                                if let Some(g) =
                                    cap.lock().expect("captured_primary poisoned").as_ref()
                                {
                                    g.reassert_blanking();
                                }
                            }
                            if let Some(sh) = shielded_primary.as_ref() {
                                if let Some(g) =
                                    sh.lock().expect("shielded_primary poisoned").as_ref()
                                {
                                    g.reassert_blanking();
                                }
                            }
                        };
                        // TWO sweeps. The re-anchor's WindowServer relayout can
                        // still be finishing well after the first sweep (it fires
                        // during the reactivation churn, so it's slow) and would
                        // re-strand windows the first sweep just placed — the
                        // "apps flicker then disappear" symptom. A second sweep
                        // after the relayout has fully settled sticks. Robust to
                        // however long the relayout takes; a no-op second sweep is
                        // cheap when the first already stuck.
                        std::thread::sleep(std::time::Duration::from_millis(700));
                        if !lifecycle.is_current(generation) {
                            return;
                        }
                        let first = crate::input::gather_windows_onto_display(vd_id);
                        reblank();
                        std::thread::sleep(std::time::Duration::from_millis(1000));
                        if !lifecycle.is_current(generation) {
                            return;
                        }
                        let second = crate::input::gather_windows_onto_display(vd_id);
                        reblank();
                        // One more after the relayout has fully settled, in case
                        // the second sweep's relayout also trails.
                        std::thread::sleep(std::time::Duration::from_millis(400));
                        reblank();
                        if first > 0 || second > 0 {
                            tracing::info!(
                                first,
                                second,
                                display_id = vd_id,
                                "gathered stranded windows after live resize (two sweeps)"
                            );
                        }
                    });
                }
                (width, height)
            }
            Err(e) => {
                // Keep serving the size the client negotiated: by the time this
                // runs the client has already accepted it in Demand Active, so
                // switching to the display's old size would build the video
                // surface at a size the client isn't showing (a client at
                // 1728×1084 then sees the top-left quarter of a 3456×2170
                // surface). Capture scales from whatever mode the display is
                // in, and the next `updates()` retries the re-mode: a failed
                // `resize` clears `mode_px`, so the retry happens even when
                // the next client asks for the size that last succeeded.
                // Input maps onto the display's current points.
                self.screen_size_pts = vd.size_pts();
                self.publish_mode(
                    width,
                    height,
                    vd.measurement(width, height),
                    Some(format!("mode change failed: {e:#}; capture is scaled")),
                );
                tracing::warn!(
                    error = ?e,
                    requested_w = width,
                    requested_h = height,
                    "virtual display live resize failed — serving the requested size, \
                     scaled from the display's current mode"
                );
                (width, height)
            }
        }
    }

    /// The body of [`RdpServerDisplay::updates`], parameterized on the final
    /// session size so the virtual-display sync above decides it first.
    async fn build_updates(
        &mut self,
        width: u16,
        height: u16,
    ) -> Result<Box<dyn RdpServerDisplayUpdates + Send>> {
        #[cfg(target_os = "macos")]
        let inner: Box<dyn RdpServerDisplayUpdates + Send> = Box::new(
            macos::ScreenCaptureUpdates::start(
                width,
                height,
                self.fps,
                self.display_id,
                self.screen_size_pts,
                self.cursor_scale * self.pointer_scale(width),
                self.warp_cursor_home,
                self.gfx.clone(),
                self.keyframe_on_change,
                self.click_signal.clone(),
                self.flush_frames,
                self.capture_nv12,
                self.display_suppressed.clone(),
                self.auto_size,
                self.stretch,
                self.desktop_size.clone(),
                self.pending_resize.clone(),
                self.suppress_next_adopt.clone(),
                self.live_pointer_scale(),
            )
            .await?,
        );
        #[cfg(not(target_os = "macos"))]
        let inner: Box<dyn RdpServerDisplayUpdates + Send> =
            Box::new(stub::StubUpdates::new(width, height)?);
        Ok(match self.session_tracker.clone() {
            Some(tracker) => Box::new(CountedUpdates::new(inner, tracker)),
            None => inner,
        })
    }
}

/// Max pixels per emitted legacy `BitmapUpdate`. The bitmap encoder packs a
/// whole rect into one update PDU (one RLE rectangle per ~`65535/(w*4)`
/// rows); mstsc renders a single ~1280×720 (≈0.9 MP) update but DROPS a
/// 1920×1080 (≈2.1 MP) one — a per-update size/rectangle-count limit — so
/// the big initial full-frame paint of a virtual display never shows
/// (only the small ticking-clock dirty-rects render). Splitting tall rects
/// into strips at or below this proven-good size keeps every update
/// renderable. FreeRDP accepts either; it just sees more, smaller updates.
///
/// Pure rect math (no platform deps) — lives at file scope so it is
/// unit-tested on every target; `mod macos` reaches it via `use super::*`.
const MAX_BITMAP_UPDATE_PIXELS: u32 = 1280 * 720;

/// Decide the capture scaling mode from the configured session size vs the
/// display's `native` (points) and `backing` (HiDPI pixels) sizes. Pure (no
/// platform deps) so it's unit-tested on every target; complements the input
/// side ([`crate::input`]'s `map_client_to_display`, which consumes the
/// resulting letterbox flag). Returns `(force_full_frame, letterbox)`:
///
/// - `force_full_frame`: SCK is scaling — the configured size matches neither
///   native points NOR backing pixels — so its dirty-rects arrive in source
///   coords and misalign; we must send whole frames. A 1:1 native OR backing
///   capture (the `--hidpi` path) keeps dirty-rects valid, so it's `false`.
/// - `letterbox`: preserve the Mac's aspect ratio with black bars instead of
///   stretching to fill — only when scaling on the auto-size path and `--stretch`
///   is not set. No scaling ⇒ nothing to letterbox.
fn capture_scaling_mode(
    native: (u16, u16),
    backing: (u16, u16),
    configured: (u16, u16),
    auto_size: bool,
    stretch: bool,
) -> (bool, bool) {
    let force_full_frame = !(native == configured || backing == configured);
    let letterbox = force_full_frame && auto_size && !stretch;
    (force_full_frame, letterbox)
}

/// Whether SCK needs a 1/fps minimum frame interval. On a display refreshing
/// no faster than `fps` (0.5 Hz slack so 59.94 counts as 60) the cap saves no
/// frames but makes SCK hold each one back to its pacing slot, adding latency.
/// An unknown refresh rate (0) keeps the cap.
fn sck_needs_frame_cap(fps: u32, display_hz: f64) -> bool {
    !(display_hz > 0.0 && display_hz <= f64::from(fps) + 0.5)
}

/// Split a rect into horizontal strips each ≤ [`MAX_BITMAP_UPDATE_PIXELS`].
/// Returns the rect unchanged when it already fits.
fn split_strips(x: u16, y: u16, w: u16, h: u16) -> Vec<(u16, u16, u16, u16)> {
    if w == 0 || h == 0 {
        return Vec::new();
    }
    if u32::from(w) * u32::from(h) <= MAX_BITMAP_UPDATE_PIXELS {
        return vec![(x, y, w, h)];
    }
    let strip_rows =
        u16::try_from((MAX_BITMAP_UPDATE_PIXELS / u32::from(w)).max(1)).unwrap_or(u16::MAX);
    let mut out = Vec::new();
    let mut row = 0u16;
    while row < h {
        let sh = strip_rows.min(h - row);
        out.push((x, y + row, w, sh));
        row += sh;
    }
    out
}

#[cfg(target_os = "macos")]
mod macos {
    use super::*;

    use anyhow::{anyhow, Context};
    use screencapturekit::async_api::AsyncSCShareableContent;
    use screencapturekit::cm::CMSampleBuffer;
    use screencapturekit::cv::{CVPixelBuffer, CVPixelBufferLockFlags};
    use screencapturekit::error::SCStreamErrorCode;
    use screencapturekit::prelude::{
        CMTime, PixelFormat as SckPixelFormat, SCContentFilter, SCDisplay, SCError, SCStream,
        SCStreamConfiguration, SCStreamOutputType,
    };
    use screencapturekit::stream::delegate_trait::ErrorHandler;

    /// SCK's surface-pool depth: its default (3) plus one for the frame the
    /// flush burst keeps retained (`ScreenCaptureUpdates::last_frame`), so
    /// holding it doesn't shrink the headroom SCK had before.
    const SCK_QUEUE_DEPTH: u32 = 4;

    /// SCK's surface-pool depth in NV12 mode (its maximum). There the encoder
    /// holds capture surfaces too — VideoToolbox retains each frame until it is
    /// encoded — on top of the async buffer, the frame being handled, and the
    /// retained flush frame.
    const SCK_QUEUE_DEPTH_NV12: u32 = 8;

    /// How long the shared SuppressOutput flag must remain `true` before
    /// the gate engages. A real minimize lasts seconds-to-minutes and
    /// trips easily; backpressure flaps under heavy local CPU/IO last
    /// tens of ms and are filtered out. 1 second is comfortably between.
    const SUPPRESS_DEBOUNCE: Duration = Duration::from_secs(1);
    /// How long after a capture starts a pre-EGFX frame is kept for
    /// re-sending over H.264. The pipeline is normally ready ~100-250 ms
    /// after the first frame; this only bounds the copy for a client that
    /// never opens the graphics channel.
    const EGFX_PENDING_FRAME_WINDOW: Duration = Duration::from_secs(5);

    /// Samples kept for a consumer that falls behind; the oldest is dropped.
    const SAMPLE_BUFFER: usize = 4;

    static SCREENS_ASLEEP: AtomicBool = AtomicBool::new(false);

    pub fn observe_display_sleep() {
        crate::runloop_thread::submit(|| unsafe {
            use objc2_app_kit::{
                NSWorkspace, NSWorkspaceScreensDidSleepNotification,
                NSWorkspaceScreensDidWakeNotification,
            };
            use objc2_foundation::NSNotification;
            use std::ptr::NonNull;

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

    #[derive(Default)]
    struct SampleQueue {
        samples: std::collections::VecDeque<CMSampleBuffer>,
        stopped: Option<CaptureStop>,
    }

    #[derive(Default)]
    struct Samples {
        queue: std::sync::Mutex<SampleQueue>,
        ready: tokio::sync::Notify,
    }

    /// An SCK display capture whose `next()` reports the delegate's stop reason.
    /// `AsyncSCStream` has no delegate, so a stream SCK stops (its display went
    /// away, as on display sleep) would leave `next()` pending forever.
    struct DisplayStream {
        stream: Option<SCStream>,
        samples: Arc<Samples>,
    }

    impl Samples {
        async fn next(&self) -> Result<CMSampleBuffer, CaptureStop> {
            loop {
                {
                    let mut q = self.queue.lock().unwrap();
                    if let Some(reason) = q.stopped {
                        return Err(reason);
                    }
                    if let Some(sample) = q.samples.pop_front() {
                        return Ok(sample);
                    }
                }
                self.ready.notified().await;
            }
        }
    }

    impl DisplayStream {
        fn start(display: &SCDisplay, config: &SCStreamConfiguration) -> Result<Self, SCError> {
            let filter = SCContentFilter::create()
                .with_display(display)
                .with_excluding_windows(&[])
                .build();
            let samples = Arc::new(Samples::default());
            let on_stop = Arc::clone(&samples);
            let mut stream = SCStream::new_with_delegate(
                &filter,
                config,
                ErrorHandler::new(move |error| {
                    tracing::warn!(%error, "SCK stopped the capture");
                    let reason =
                        if error.stream_error_code() == Some(SCStreamErrorCode::NoCaptureSource) {
                            CaptureStop::MissingSource
                        } else {
                            CaptureStop::Other
                        };
                    on_stop.queue.lock().unwrap().stopped.get_or_insert(reason);
                    on_stop.ready.notify_one();
                }),
            );
            let on_sample = Arc::clone(&samples);
            stream.add_output_handler(
                move |sample, _| {
                    let mut q = on_sample.queue.lock().unwrap();
                    if q.stopped.is_some() {
                        return;
                    }
                    if q.samples.len() >= SAMPLE_BUFFER {
                        q.samples.pop_front();
                    }
                    q.samples.push_back(sample);
                    drop(q);
                    on_sample.ready.notify_one();
                },
                SCStreamOutputType::Screen,
            );
            stream.start_capture()?;
            Ok(Self {
                stream: Some(stream),
                samples,
            })
        }

        fn take_live_stream(&mut self) -> Option<SCStream> {
            let stream = self.stream.take();
            if self.stop_reason().is_some() {
                return None;
            }
            stream
        }

        fn stop_reason(&self) -> Option<CaptureStop> {
            self.samples.queue.lock().unwrap().stopped
        }

        async fn next(&self) -> Result<CMSampleBuffer, CaptureStop> {
            self.samples.next().await
        }
    }

    impl Drop for DisplayStream {
        fn drop(&mut self) {
            // A replacement whose start/first frame outlasts the recovery
            // window is disposed independently of the connection's teardown.
            if let Some(stream) = self.take_live_stream() {
                tokio::task::spawn_blocking(move || {
                    let _ = stream.stop_capture();
                });
            }
        }
    }

    #[cfg(test)]
    #[tokio::test(start_paused = true)]
    async fn an_empty_sample_queue_is_idle_until_the_delegate_stops_capture() {
        let samples = Samples::default();
        assert!(
            tokio::time::timeout(CAPTURE_RECOVERY_WINDOW * 2, samples.next())
                .await
                .is_err()
        );
        samples.queue.lock().unwrap().stopped = Some(CaptureStop::MissingSource);
        samples.ready.notify_one();
        assert!(matches!(
            samples.next().await,
            Err(CaptureStop::MissingSource)
        ));
    }

    pub struct ScreenCaptureUpdates {
        stream: DisplayStream,
        /// What `stream` captures, kept to reopen it after SCK stops it.
        display_id: u32,
        /// Whether `display_id` is the virtual display, which can come back under a new id.
        follows_virtual_display: bool,
        size_px: (u32, u32),
        stream_config: SCStreamConfiguration,
        pending: std::collections::VecDeque<DisplayUpdate>,
        // Force a full-frame seed on the first sample so the client's
        // bitmap cache starts in a known-good state; SCK's dirty rects
        // for frame 0 may not cover everything.
        seeded: bool,
        // When the configured desktop size differs from the Mac's native
        // display, SCK scales the output internally but emits dirty rects
        // in *source* coordinates — they don't line up with the output
        // buffer. RemoteFx (mstsc) renders the resulting mis-positioned
        // tile updates as a black canvas. Force a full-frame BitmapUpdate
        // every tick in that case; the upstream encoder's framebuffer diff
        // then keeps the actual bandwidth reasonable.
        force_full_frame: bool,
        cursor: CursorState,
        /// Last time the cursor shape was polled (`None` = never → poll
        /// immediately so the client gets its initial pointer). Each poll is a
        /// synchronous `SLSGetGlobalCursorData` WindowServer IPC sitting inline
        /// on the capture/encode path, and shape changes are human-timescale —
        /// so polling is throttled to ~15 Hz instead of once per 60 fps frame.
        /// Time-based, NOT identity-gated, so animated cursors (beachball /
        /// watch) still animate — just at ~15 fps (the identity-gate variant
        /// froze them entirely; see the cursor-animation feedback note).
        last_cursor_poll: Option<Instant>,
        /// EGFX/H.264 frame sink; `None` unless `--enable-h264`.
        gfx: Option<crate::h264::Gfx>,
        /// On-demand-keyframe tuning (`--keyframe-on-change`). When disabled, no
        /// dirty-area work is done for keyframe decisions.
        keyframe_on_change: KeyframeOnChange,
        /// Rising-edge state for `keyframe_on_change`: true means "ready to fire
        /// on the next large change". Cleared when one fires; re-armed once the
        /// dirty area subsides below half the threshold (hysteresis). Stops
        /// sustained churn (e.g. video) from forcing an IDR every frame.
        kf_armed: bool,
        /// Mouse-click hint for the post-click keyframe-threshold drop. Only
        /// consulted when `keyframe_on_change.enabled`.
        click_signal: Option<ClickSignal>,
        /// Interval between SCK frames (1/fps). Doubles as the flush-burst
        /// timeout: when SCK goes idle we wait at most this long before
        /// re-submitting the last frame to drain mstsc's presentation buffer.
        frame_interval: Duration,
        /// How many trailing flush frames to re-send after each change
        /// (`--flush-frames`). Each is a tiny skip-P-frame; mstsc needs ≥2 to
        /// display a frame, default 4 gives margin. 0 disables the burst.
        flush_frames: u32,
        /// Trailing flush re-submits remaining after the last real change
        /// (EGFX/H.264 path only). SCK stops delivering frames on a static
        /// screen, so the last change before a pause (e.g. the final keystroke)
        /// would otherwise sit in mstsc's ~2-frame AVC420 presentation buffer
        /// until the next on-screen change or the periodic keyframe — that's
        /// the "typing follows the keyframe" lag. We re-submit the last frame
        /// `flush_frames` times to push it through within a couple of frame
        /// intervals. Stays 0 (no-op) on the legacy bitmap path.
        flush_remaining: u32,
        /// Last frame submitted to EGFX, re-encoded as cheap skip-P-frames
        /// during a flush burst. Held as a retained SCK pixel buffer rather
        /// than a byte copy: copying the whole frame after every submit was
        /// ~20% of macrdp's CPU at a large virtual display, and the flush
        /// path only reads it on the few idle-edge ticks. Holding it pins one
        /// surface of SCK's pool, which is why the stream's queue depth is
        /// one above SCK's default (see `SCK_QUEUE_DEPTH`).
        last_frame: Option<CVPixelBuffer>,
        /// A frame captured while the EGFX pipeline was still being negotiated
        /// is waiting in `last_frame` to be re-sent over H.264 once it comes up
        /// (see `promote_frame_to_egfx`).
        egfx_pending_frame: bool,
        /// When this capture started; bounds how long a frame is held for
        /// `egfx_pending_frame` (a client that never opens the graphics
        /// channel would otherwise have every legacy frame retained), and the
        /// legacy hold of `Gfx::hold_legacy` is measured from it.
        started_at: Instant,
        /// Capturing NV12 for the encoder (`--capture-nv12`). Frames that still
        /// ship as legacy bitmaps (before EGFX is ready, or a client without
        /// AVC) are converted back to BGRA on the CPU.
        nv12: bool,
        /// BGRA converted from an NV12 frame for the legacy bitmap path.
        /// Reused across frames; stays empty unless that path runs.
        nv12_bgra: Vec<u8>,
        /// Shared "client minimized" flag — see [`super::CaptureDisplay::display_suppressed`].
        /// `None` disables the gate.
        display_suppressed: Option<Arc<AtomicBool>>,
        /// Last observed value of `display_suppressed`, used to detect the
        /// false→true→false transition. On the un-suppress edge we force an
        /// IDR keyframe so the client resumes from a fresh reference frame
        /// (the P-frames we would have sent during the suppress depend on a
        /// state mstsc no longer has).
        was_suppressed: bool,
        /// Local "first observed suppress=true" timestamp for debouncing.
        /// `None` while the shared flag reads `false`; set to `Some(Instant::now())`
        /// the first iteration we read `true`. The suppress gate only fires
        /// after `elapsed >= SUPPRESS_DEBOUNCE` — so a real multi-second
        /// minimize trips it normally, but transient flaps under wire
        /// pressure (mstsc backs off for tens of ms when the encoded backlog
        /// is too large, then resumes) don't. Without this, those flaps
        /// stop/start the video pipeline rapidly under heavy local CPU/IO
        /// (cargo build) and the audio mute thrashes — both audible as
        /// stutter even though no real minimize happened.
        suppressed_since: Option<Instant>,
        /// True once the EGFX/H.264 encoder has accepted at least one
        /// frame for this session (i.e., `gfx.submit_bgra` returned
        /// `Ok(true)`). The suppress gate is a no-op until this is set —
        /// mstsc's normal connect handshake includes a
        /// `SuppressOutput { None }` *before* its display surface is
        /// fully initialized, and stopping the first EGFX frame from
        /// going through leaves mstsc with nothing to display + a
        /// half-initialized surface that doesn't recover when we
        /// un-suppress. FreeRDP doesn't do this. Tracking
        /// "we've delivered the first frame" gives us a clean
        /// "client is fully wired up" signal that's stricter than just
        /// "connected" but cheap to maintain.
        first_egfx_frame_sent: bool,
        /// `last_frame` holds a frame captured while EGFX was still being
        /// negotiated (legacy hold) and not yet sent anywhere. SCK sends
        /// nothing more on a static screen, so the loop submits this frame to
        /// EGFX once it is ready, or sends it as a legacy update when the hold
        /// ends without EGFX.
        held_frame: bool,
        /// When capturing a virtual display, the RDP session drives the cursor
        /// into that display's slot of the global coordinate space (to the
        /// right of / beyond the physical panels). On disconnect the cursor is
        /// stranded there — off every physical screen — until the virtual
        /// display is removed. When set, `Drop` warps the cursor back onto the
        /// primary physical display so the local Mac stays usable. Only set on
        /// the `--virtual-display` path.
        warp_cursor_home: bool,
        /// Client-driven live-resize request (window drag), debounced — see
        /// `super::CaptureDisplay::pending_resize`. Polled once per loop
        /// iteration; once settled, changes `desktop_size` and returns a
        /// `DisplayUpdate::Resize` — the same core-reactivation route as the
        /// blank-recovery case above, but actually changing the size. On the
        /// EGFX path the per-connection surface/encoder state is reset first
        /// (`Gfx::reset_for_live_resize`) so the post-reactivation rebuild
        /// starts from scratch at the new size.
        pending_resize: PendingResize,
        /// The shared session size, so a settled `pending_resize` can be
        /// written back into it before emitting `DisplayUpdate::Resize` —
        /// input scaling and the H.264 pipeline (via `Gfx::desktop_size`) read
        /// this cell directly, so it must be current before the reactivation
        /// round-trips back through `request_initial_size`.
        desktop_size: SharedDesktopSize,
        /// See `CaptureDisplay::suppress_next_adopt`. Set right before this
        /// loop returns a server-driven `DisplayUpdate::Resize`, so the
        /// reactivation's `request_initial_size` call doesn't re-adopt
        /// whatever size the client's Confirm Active echoes.
        suppress_next_adopt: Arc<AtomicBool>,
        /// See `CaptureDisplay::live_pointer_scale`, plus the scale last
        /// applied to `cursor`.
        live_pointer_scale: Option<(Arc<std::sync::atomic::AtomicU32>, f64)>,
        applied_pointer_scale: u32,
    }

    impl ScreenCaptureUpdates {
        /// Once H.264 is up, re-send the frame captured before it was, and
        /// drop the legacy bitmap strips still queued for that frame.
        ///
        /// The first frame of a connection is usually captured ~100 ms before
        /// the EGFX channel is ready, so it goes out as legacy bitmaps: up to
        /// eight 1280x720 strips at 3456x2168. Over a WAN those strips took
        /// 38-68 s to send, and the H.264 stream waited behind them, so the
        /// desktop painted strip by strip. The client draws the H.264 surface
        /// over the whole desktop anyway, so the unsent strips are dropped.
        fn promote_frame_to_egfx(&mut self) {
            if !self.egfx_pending_frame {
                return;
            }
            let Some(gfx) = self.gfx.as_ref() else {
                self.egfx_pending_frame = false;
                return;
            };
            if !gfx.pipeline_ready() {
                if !gfx.pipeline_pending() {
                    self.egfx_pending_frame = false; // declined, or the client left
                }
                return;
            }
            let queued = self.pending.len();
            self.pending
                .retain(|update| !matches!(update, DisplayUpdate::Bitmap(_)));
            let Some(pb) = self.last_frame.as_ref() else {
                self.egfx_pending_frame = false;
                return;
            };
            match submit_to_gfx(gfx, pb, self.nv12, true, false) {
                Ok(true) => {
                    self.egfx_pending_frame = false;
                    self.seeded = true;
                    self.first_egfx_frame_sent = true;
                    self.flush_remaining = self.flush_frames;
                    tracing::info!(
                        dropped_bitmap_updates = queued - self.pending.len(),
                        "EGFX ready — re-sent the pre-EGFX frame over H.264"
                    );
                }
                Ok(false) => {}
                Err(e) => {
                    self.egfx_pending_frame = false;
                    tracing::warn!(error = ?e, "EGFX submit_bgra failed");
                }
            }
        }
    }

    impl ScreenCaptureUpdates {
        /// The SCK stream sent no frame for one interval while a frame is held
        /// for the EGFX negotiation (`held_frame`): submit it to EGFX if EGFX
        /// is ready now, keep holding it while the hold lasts, or send it as a
        /// full legacy picture once the hold ends without EGFX.
        fn send_held_frame(&mut self) {
            let (Some(gfx), Some(pb)) = (self.gfx.as_ref(), self.last_frame.as_ref()) else {
                self.held_frame = false;
                return;
            };
            // `submit_to_gfx` hands the buffer over as NV12 or locks it as BGRA,
            // matching the capture format (`--capture-nv12`).
            match submit_to_gfx(gfx, pb, self.nv12, true, false) {
                Ok(true) => {
                    self.held_frame = false;
                    self.seeded = true;
                    self.first_egfx_frame_sent = true;
                    self.flush_remaining = self.flush_frames;
                    tracing::debug!("EGFX ready — submitted the frame held during the negotiation");
                }
                Ok(false) if gfx.hold_legacy(self.started_at) => {}
                Ok(false) => {
                    // Hold over without EGFX: send the held frame as a full
                    // legacy picture (converted back to BGRA in NV12 mode).
                    self.held_frame = false;
                    let guard = match pb.lock(CVPixelBufferLockFlags::READ_ONLY) {
                        Ok(g) => g,
                        Err(e) => {
                            tracing::warn!(error = ?e, "held frame: CVPixelBuffer::lock failed");
                            return;
                        }
                    };
                    let (Ok(w), Ok(h)) =
                        (u16::try_from(guard.width()), u16::try_from(guard.height()))
                    else {
                        return;
                    };
                    let (src, stride): (&[u8], usize) = if self.nv12 {
                        if let Err(e) = nv12_to_bgra(&guard, &mut self.nv12_bgra) {
                            tracing::warn!(error = ?e, "held frame: NV12 to BGRA failed");
                            return;
                        }
                        (&self.nv12_bgra, usize::from(w) * 4)
                    } else {
                        (guard.as_slice(), guard.bytes_per_row())
                    };
                    for (sx, sy, sw, sh) in split_strips(0, 0, w, h) {
                        if let Some(update) = rect_update(src, stride, sx, sy, sw, sh) {
                            self.pending.push_back(update);
                        }
                    }
                    self.seeded = true;
                    // EGFX may still come up after the hold: keep the frame
                    // (already in `last_frame`) for `promote_frame_to_egfx`.
                    if gfx.pipeline_pending()
                        && self.started_at.elapsed() < EGFX_PENDING_FRAME_WINDOW
                    {
                        self.egfx_pending_frame = true;
                    }
                    tracing::debug!("legacy hold over — sent the held frame as legacy");
                }
                Err(e) => {
                    self.held_frame = false;
                    tracing::warn!(error = ?e, "EGFX held-frame submit failed");
                }
            }
        }

        #[allow(clippy::too_many_arguments)]
        pub async fn start(
            width: u16,
            height: u16,
            fps: u32,
            target_display_id: Option<u32>,
            screen_size_pts: (f64, f64),
            cursor_scale_multiplier: f64,
            warp_cursor_home: bool,
            gfx: Option<crate::h264::Gfx>,
            keyframe_on_change: KeyframeOnChange,
            click_signal: Option<ClickSignal>,
            flush_frames: u32,
            capture_nv12: bool,
            display_suppressed: Option<Arc<AtomicBool>>,
            auto_size: bool,
            stretch: bool,
            desktop_size: SharedDesktopSize,
            pending_resize: PendingResize,
            suppress_next_adopt: Arc<AtomicBool>,
            live_pointer_scale: Option<(Arc<std::sync::atomic::AtomicU32>, f64)>,
        ) -> Result<Self> {
            let content = AsyncSCShareableContent::get()
                .await
                .map_err(|e| anyhow!("AsyncSCShareableContent::get failed (likely Screen Recording permission denied): {e:?}"))?;

            let displays = content.displays();
            let display = match target_display_id {
                Some(id) => displays
                    .iter()
                    .find(|d| d.display_id() == id)
                    .with_context(|| {
                        format!(
                            "SCK enumeration has no display with id={id} — the \
                             virtual display didn't register, or the WindowServer \
                             hasn't picked it up yet. SCK sees: [{}]",
                            displays
                                .iter()
                                .map(|d| format!(
                                    "id={} {}x{}",
                                    d.display_id(),
                                    d.width(),
                                    d.height()
                                ))
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })?,
                None => {
                    // Mirror mode: follow the main display, as input does.
                    let ids: Vec<u32> = displays.iter().map(|d| d.display_id()).collect();
                    let main_id = core_graphics::display::CGDisplay::main().id;
                    let index = super::mirror_display_index(&ids, main_id)
                        .context("no displays available")?;
                    if ids.len() > 1 {
                        tracing::info!(
                            mirrored = ids[index],
                            main = main_id,
                            sck_displays = ?ids,
                            "multiple displays: mirroring the main display"
                        );
                    }
                    &displays[index]
                }
            };

            let native_w = u16::try_from(display.width()).context("display width > u16")?;
            let native_h = u16::try_from(display.height()).context("display height > u16")?;
            // Backing (Retina) pixel size of this display. Capturing at EITHER
            // the logical point size (native_w/h) or the backing pixel size
            // (the `--hidpi` path) is a 1:1 native capture — SCK delivers the
            // panel's own pixels and its dirty-rects line up with our
            // framebuffer. Only a size matching neither (explicit
            // --width/--height that asks SCK to scale) forces full frames,
            // because then dirty-rects arrive in source coords and misalign.
            let mode = core_graphics::display::CGDisplay::new(display.display_id()).display_mode();
            let (backing_w, backing_h) =
                if crate::virtual_display::is_hidpi_virtual_display(display.display_id()) {
                    // `CGDisplayCopyDisplayMode` returns nothing for a virtual
                    // display, so the HiDPI vd's 2× backing is derived here.
                    (native_w.saturating_mul(2), native_h.saturating_mul(2))
                } else {
                    mode.as_ref()
                        .and_then(|m| {
                            Some((
                                u16::try_from(m.pixel_width()).ok()?,
                                u16::try_from(m.pixel_height()).ok()?,
                            ))
                        })
                        .unwrap_or((native_w, native_h))
                };
            // See `capture_scaling_mode`: full frames when SCK scales (configured
            // size matches neither native points nor backing pixels), and
            // letterbox vs stretch on the auto-size path.
            let (force_full_frame, letterbox) = capture_scaling_mode(
                (native_w, native_h),
                (backing_w, backing_h),
                (width, height),
                auto_size,
                stretch,
            );
            desktop_size.set_letterbox(letterbox);
            if force_full_frame {
                tracing::warn!(
                    requested_w = width,
                    requested_h = height,
                    native_w,
                    native_h,
                    backing_w,
                    backing_h,
                    aspect_mode = if letterbox { "letterbox" } else { "stretch" },
                    "configured size != native points or backing; SCK dirty rects are in \
                     source coords and would misalign — sending full frames every tick \
                     (higher bandwidth)"
                );
            }

            let nv12 = gfx.is_some() && capture_nv12;
            if nv12 {
                tracing::info!(
                    "capture: ScreenCaptureKit delivers full-range BT.709 NV12 to the \
                     encoder (--capture-nv12, experimental)"
                );
            }
            let display_hz = mode.map_or(0.0, |m| m.refresh_rate());
            let frame_cap = sck_needs_frame_cap(fps, display_hz);
            tracing::debug!(display_hz, fps, frame_cap, "SCK frame cap");
            let min_frame_interval = if frame_cap {
                CMTime::new(1, fps as i32)
            } else {
                CMTime::new(0, 1)
            };

            let config = SCStreamConfiguration::new()
                .with_width(u32::from(width))
                .with_height(u32::from(height))
                .with_pixel_format(if nv12 {
                    SckPixelFormat::YCbCr_420f
                } else {
                    SckPixelFormat::BGRA
                })
                .with_minimum_frame_interval(&min_frame_interval)
                // Hide the macOS cursor in the captured framebuffer; we forward
                // it separately as RGBAPointer so the client renders its own
                // (real-shape) cursor without doubling up.
                .with_shows_cursor(false)
                // Aspect handling on a scaling (non-native) capture:
                //   * letterbox=false (default for explicit --width/--height, or
                //     auto-size + --stretch): stretch the source to fill the
                //     output exactly — no black bars, but distorts on aspect
                //     mismatch.
                //   * letterbox=true (auto-size default): preserve the Mac's
                //     aspect ratio, padding with black bars. input.rs maps mouse
                //     coords into the centered content sub-rect to keep clicks
                //     aligned (the stretch path needs no such correction, which
                //     is why fill used to be the only mode).
                //
                // Apple replaced `scalesToFit` with `preservesAspectRatio`
                // (inverse semantics) in macOS 14; on 14+ the newer property is
                // what takes effect. `scales_to_fit(true)` stays set so the
                // source scales to the output either way; `preserves_aspect_ratio`
                // is the knob that picks letterbox vs stretch.
                .with_scales_to_fit(true)
                .with_preserves_aspect_ratio(letterbox)
                .with_queue_depth(if nv12 {
                    SCK_QUEUE_DEPTH_NV12
                } else {
                    SCK_QUEUE_DEPTH
                });
            // The encoder's NV12 is BT.709 (the session is tagged 709), and the
            // BGRA path converts with that matrix too, so ask SCK for the same.
            if nv12 {
                set_color_matrix_709(&config);
                tracing::debug!(color_matrix = ?config.color_matrix(), "capture: NV12 colour matrix");
            }

            let stream = DisplayStream::start(display, &config)
                .map_err(|e| anyhow!("SCStream::start_capture failed: {e:?}"))?;

            // Cursor scale = session framebuffer px ÷ display backing px.
            // SkyLight hands the cursor back at the display's backing pixels;
            // the client draws the pointer 1:1 against our framebuffer, so on
            // a Retina panel running at logical points (default, no --hidpi)
            // the raw cursor is 2× oversized with a 2×-off hotspot. This ratio
            // is 1.0 when they match (1× displays, and --hidpi on Retina).
            // The pointer is forwarded at its native macOS size — the raw
            // SkyLight backing-pixel bitmap — which matches the cursor on the
            // Mac's own screen and keeps the hotspot exact, on 1× panels,
            // Retina, and `--hidpi` alike. `--cursor-scale` (default 1.0) is a
            // pure comfort multiplier on top: some clients draw the pointer at
            // native pixels while upscaling the desktop image to their window,
            // which can make a native-size pointer look small. Clamped to a
            // sane band; the hotspot stays accurate at any value.
            // On the HiDPI virtual display the multiplier already includes
            // the pointer enlargement (`CaptureDisplay::pointer_scale`).
            let cursor_scale = cursor_scale_multiplier.clamp(0.1, 8.0);
            tracing::debug!(
                native_w,
                native_h,
                backing_w,
                backing_h,
                session_w = width,
                session_h = height,
                cursor_scale,
                "cursor pointer scaling"
            );
            let cursor = CursorState::new(width, height, screen_size_pts, cursor_scale)?;
            let frame_interval = Duration::from_secs_f64(1.0 / f64::from(fps.max(1)));

            // Reset the cross-connection suppress flag — the `Arc<AtomicBool>`
            // is owned by the server and survives disconnect/reconnect, so a
            // value left set `true` at the previous session's teardown would
            // freeze the new session the moment the gate arms (first EGFX
            // frame ships). Fresh state per connection is the only safe
            // default; the gate then trips correctly when the new client's
            // own SuppressOutput PDU lands.
            if let Some(flag) = display_suppressed.as_ref() {
                flag.store(false, Ordering::Relaxed);
            }

            Ok(Self {
                stream,
                display_id: display.display_id(),
                follows_virtual_display: target_display_id.is_some(),
                size_px: (u32::from(width), u32::from(height)),
                stream_config: config,
                pending: std::collections::VecDeque::new(),
                seeded: false,
                force_full_frame,
                cursor,
                last_cursor_poll: None,
                gfx,
                keyframe_on_change,
                kf_armed: true,
                click_signal,
                frame_interval,
                flush_frames,
                flush_remaining: 0,
                last_frame: None,
                egfx_pending_frame: false,
                started_at: Instant::now(),
                nv12,
                nv12_bgra: Vec::new(),
                display_suppressed,
                was_suppressed: false,
                suppressed_since: None,
                first_egfx_frame_sent: false,
                held_frame: false,
                warp_cursor_home,
                pending_resize,
                desktop_size,
                suppress_next_adopt,
                applied_pointer_scale: live_pointer_scale
                    .as_ref()
                    .map_or(0, |(cell, _)| cell.load(Ordering::Relaxed)),
                live_pointer_scale,
            })
        }

        async fn try_reopen_stream(
            display_id: u32,
            follows_virtual_display: bool,
            size_px: (u32, u32),
            config: SCStreamConfiguration,
        ) -> Result<Option<(u32, DisplayStream, CMSampleBuffer)>, CaptureStop> {
            let Ok(content) = AsyncSCShareableContent::get().await else {
                return Ok(None);
            };
            let displays = content.displays();
            let id = if follows_virtual_display {
                let online: Vec<_> = displays
                    .iter()
                    .map(|d| online_display(d.display_id()))
                    .collect();
                let Some(id) = super::pick_reopened_display(display_id, &online, size_px) else {
                    return Ok(None);
                };
                id
            } else {
                display_id
            };
            let Some(display) = displays.into_iter().find(|d| d.display_id() == id) else {
                return Ok(None);
            };

            // The binding's start_capture is synchronous. Keep the deadline
            // effective even if its native completion stalls. This worker
            // owns only the candidate stream, never the session's display lease.
            let stream =
                match tokio::task::spawn_blocking(move || DisplayStream::start(&display, &config))
                    .await
                {
                    Ok(Ok(stream)) => stream,
                    Ok(Err(error)) => {
                        tracing::debug!(?error, "reopening the SCK capture failed");
                        return Ok(None);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "SCK capture restart task failed");
                        return Ok(None);
                    }
                };
            // A successful start callback does not establish usable capture.
            loop {
                let sample = stream.next().await?;
                if sample
                    .frame_status()
                    .is_none_or(|status| status.has_content())
                    && sample.image_buffer().is_some()
                {
                    return Ok(Some((id, stream, sample)));
                }
            }
        }

        async fn reopen_stream(&mut self, stop: CaptureStop) -> Option<CMSampleBuffer> {
            let started = Instant::now();
            let display_id = self.display_id;
            let result = recover_capture(
                stop,
                || {
                    (SCREENS_ASLEEP.load(Ordering::SeqCst)
                        || core_graphics::display::CGDisplay::new(display_id).is_asleep())
                        && crate::virtual_display::screen_authentication_is_required() == Some(true)
                },
                || {
                    Self::try_reopen_stream(
                        display_id,
                        self.follows_virtual_display,
                        self.size_px,
                        self.stream_config.clone(),
                    )
                },
            )
            .await;
            let elapsed_ms = started.elapsed().as_millis() as u64;
            match result {
                CaptureRecovery::Restored((id, stream, first_frame)) => {
                    if id != self.display_id {
                        tracing::warn!(
                            old = self.display_id,
                            new = id,
                            "virtual display came back under a new id"
                        );
                        self.display_id = id;
                    }
                    self.stream = stream;
                    self.seeded = false;
                    self.last_frame = None;
                    self.held_frame = false;
                    self.egfx_pending_frame = false;
                    self.flush_remaining = 0;
                    tracing::info!(display_id = id, elapsed_ms, "SCK capture reopened");
                    Some(first_frame)
                }
                CaptureRecovery::LockedDisplaySleep => {
                    tracing::info!(
                        display_id,
                        elapsed_ms,
                        "locked display fell asleep and SCK lost its source; disconnecting RDP"
                    );
                    None
                }
                CaptureRecovery::TimedOut => {
                    tracing::warn!(
                        display_id,
                        elapsed_ms,
                        ?stop,
                        "SCK capture recovery expired; disconnecting RDP"
                    );
                    None
                }
            }
        }
    }

    fn online_display(id: u32) -> super::OnlineDisplay {
        extern "C" {
            fn CGDisplayVendorNumber(display: u32) -> u32;
        }
        let d = core_graphics::display::CGDisplay::new(id);
        let size_px = d.display_mode().map_or((0, 0), |m| {
            (m.pixel_width() as u32, m.pixel_height() as u32)
        });
        super::OnlineDisplay {
            id,
            vendor: unsafe { CGDisplayVendorNumber(id) },
            builtin: d.is_builtin(),
            size_px,
        }
    }

    /// Set `SCStreamConfiguration.colorMatrix` to BT.709.
    ///
    /// Not `with_color_matrix`: the crate stores a temporary CFString built
    /// from its `&str`, but the property doesn't retain its value (Apple
    /// expects one of the CoreGraphics constants), so `SCStream` later copies
    /// a freed string and the process traps in `CFStringCreateCopy`. Hand it
    /// the real, never-freed constant instead.
    fn set_color_matrix_709(config: &SCStreamConfiguration) {
        /// Opaque `struct __CFString`, so the argument carries the
        /// `^{__CFString=}` type encoding objc2 checks against the method.
        #[repr(C)]
        struct CFStringOpaque {
            _private: [u8; 0],
        }
        // SAFETY: matches CoreFoundation's declaration of `CFStringRef`.
        unsafe impl objc2::encode::RefEncode for CFStringOpaque {
            const ENCODING_REF: objc2::encode::Encoding = objc2::encode::Encoding::Pointer(
                &objc2::encode::Encoding::Struct("__CFString", &[]),
            );
        }
        #[link(name = "CoreGraphics", kind = "framework")]
        extern "C" {
            static kCGDisplayStreamYCbCrMatrix_ITU_R_709_2: *const CFStringOpaque;
        }
        // SAFETY: `SCStreamConfiguration` is `#[repr(transparent)]` over the
        // retained `SCStreamConfiguration *`, so this reads that pointer; it
        // stays valid while `config` is borrowed. `setColorMatrix:` takes a
        // CFStringRef, and the constant lives for the process.
        unsafe {
            let obj =
                *(config as *const SCStreamConfiguration as *const *mut objc2::runtime::AnyObject);
            let matrix = kCGDisplayStreamYCbCrMatrix_ITU_R_709_2;
            let () = objc2::msg_send![obj, setColorMatrix: matrix];
        }
    }

    /// The colour matrix must survive the configuration copy `SCStream` makes
    /// (`-[SCStreamConfiguration copyWithZone:]`). The crate's own setter left
    /// a dangling string there and the copy trapped, killing the server on
    /// connect.
    #[cfg(test)]
    #[test]
    fn color_matrix_survives_the_configuration_copy() {
        let config = SCStreamConfiguration::new().with_pixel_format(SckPixelFormat::YCbCr_420f);
        set_color_matrix_709(&config);
        assert_eq!(config.color_matrix().as_deref(), Some("ITU_R_709_2"));
        objc2::rc::autoreleasepool(|_| unsafe {
            let obj =
                *(&config as *const SCStreamConfiguration as *const *mut objc2::runtime::AnyObject);
            let copy: *mut objc2::runtime::AnyObject = objc2::msg_send![obj, copy];
            assert!(!copy.is_null());
            let () = objc2::msg_send![copy, release];
        });
    }

    /// Hand one captured frame to the H.264 pipeline: the buffer itself in NV12
    /// mode, its locked BGRA bytes otherwise. Same return contract as
    /// [`crate::h264::Gfx::submit_bgra`].
    fn submit_to_gfx(
        gfx: &crate::h264::Gfx,
        pixel_buffer: &CVPixelBuffer,
        nv12: bool,
        request_keyframe: bool,
        flush: bool,
    ) -> Result<bool> {
        if nv12 {
            return gfx.submit_nv12(pixel_buffer, request_keyframe, flush);
        }
        let guard = pixel_buffer
            .lock(CVPixelBufferLockFlags::READ_ONLY)
            .map_err(|e| anyhow!("CVPixelBuffer::lock OSStatus {e}"))?;
        gfx.submit_bgra(
            guard.as_slice(),
            guard.bytes_per_row(),
            request_keyframe,
            flush,
        )
    }

    /// Convert a locked NV12 capture frame to tightly packed BGRA in `out`.
    fn nv12_to_bgra(
        guard: &screencapturekit::cv::CVPixelBufferLockGuard<'_>,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let y = guard
            .plane_data(0)
            .ok_or_else(|| anyhow!("NV12 frame has no luma plane"))?;
        let cbcr = guard
            .plane_data(1)
            .ok_or_else(|| anyhow!("NV12 frame has no chroma plane"))?;
        crate::videotoolbox::nv12_full_range_to_bgra(
            y,
            guard.bytes_per_row_of_plane(0),
            cbcr,
            guard.bytes_per_row_of_plane(1),
            guard.width(),
            guard.height(),
            out,
        )
    }

    /// Spike diagnostic, once per session: the luma range of the first NV12
    /// frame. A desktop with true black and white reaches 0 and 255 when the
    /// buffer really is full range; 16..235 would mean video range.
    fn log_nv12_luma_range(pixel_buffer: &CVPixelBuffer) {
        let Ok(guard) = pixel_buffer.lock(CVPixelBufferLockFlags::READ_ONLY) else {
            return;
        };
        let Some(y) = guard.plane_data(0) else {
            return;
        };
        let (min, max) = y
            .iter()
            .step_by(61)
            .fold((u8::MAX, u8::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
        tracing::info!(
            pixel_format = format_args!("{:#010x}", pixel_buffer.pixel_format()),
            luma_min = min,
            luma_max = max,
            "capture: first NV12 frame"
        );
    }

    /// Build a `BitmapUpdate` for a sub-rectangle of the captured frame by
    /// copying the rect's pixels into a tightly-packed buffer.
    fn rect_update(
        src: &[u8],
        src_stride: usize,
        x: u16,
        y: u16,
        w: u16,
        h: u16,
    ) -> Option<DisplayUpdate> {
        let (Some(width), Some(height)) = (NonZeroU16::new(w), NonZeroU16::new(h)) else {
            return None;
        };
        let row_bytes = usize::from(w) * 4;
        let stride = NonZeroUsize::new(row_bytes)?;
        let mut data = Vec::with_capacity(row_bytes * usize::from(h));
        for row in 0..usize::from(h) {
            let src_off = (usize::from(y) + row) * src_stride + usize::from(x) * 4;
            data.extend_from_slice(&src[src_off..src_off + row_bytes]);
        }
        Some(DisplayUpdate::Bitmap(BitmapUpdate {
            x,
            y,
            width,
            height,
            format: PixelFormat::BgrA32,
            data: Bytes::from(data),
            stride,
        }))
    }

    impl Drop for ScreenCaptureUpdates {
        fn drop(&mut self) {
            if let Some(stream) = self.stream.take_live_stream() {
                let _ = stream.stop_capture();
            }
            // On a virtual-display session the cursor was last posted into the
            // virtual display's region of the global coordinate space, which is
            // off every physical panel. The virtual display outlives this
            // per-connection object (the server keeps running for the next
            // client), so without intervention the local cursor is stranded and
            // invisible after disconnect. Warp it back to the center of the
            // primary physical display so the Mac stays usable. Best-effort.
            if self.warp_cursor_home {
                use core_graphics::display::CGDisplay;
                use core_graphics::geometry::CGPoint;
                let b = CGDisplay::main().bounds();
                let center = CGPoint::new(
                    b.origin.x + b.size.width / 2.0,
                    b.origin.y + b.size.height / 2.0,
                );
                match CGDisplay::warp_mouse_cursor_position(center) {
                    Ok(()) => tracing::debug!(
                        x = center.x,
                        y = center.y,
                        "warped cursor to primary on disconnect"
                    ),
                    Err(e) => tracing::warn!(?e, "failed to warp cursor home on disconnect"),
                }
            }
        }
    }

    impl ScreenCaptureUpdates {
        /// The capture loop. Kept on anyhow; the trait impl below converts at the boundary.
        async fn capture_next_update(&mut self) -> Result<Option<DisplayUpdate>> {
            loop {
                // EXPERIMENTAL blank-recovery: if the H.264 blank detector armed
                // a bare core reactivation (BlankAction::Reactivate), emit a
                // no-op DisplayUpdate::Resize to the same size. The vendored
                // server turns that into Server Deactivate All → new Demand
                // Active while preserving the static channels, so the EGFX DVC
                // and its surface survive (no resize_with_monitors/DeleteSurface)
                // — the one untested lever for the mstsc reconnect-blank. A
                // forced IDR was already armed on the ctx.
                if let Some(gfx) = self.gfx.as_ref() {
                    // Manual A/V resync hotkey (Ctrl+Alt+Shift+R, set in
                    // input.rs via crate::RESYNC_VIDEO): force a clean IDR
                    // keyframe to repaint a stale/idle-blanked mstsc presentation.
                    // Deliberately lighter than the full core reactivation
                    // (gfx.request_reactivation), which on the headless
                    // virtual-display path cascades into a visible session
                    // re-cycle — see Gfx::force_keyframe.
                    if crate::RESYNC_VIDEO.swap(false, Ordering::Relaxed) {
                        gfx.force_keyframe();
                    }
                    if let Some((w, h)) = gfx.take_reactivate_request() {
                        // See `CaptureDisplay::suppress_next_adopt`: without
                        // this, the reactivation's `request_initial_size`
                        // call could re-adopt whatever size the client's
                        // Confirm Active happens to echo, undoing this
                        // same-size reactivate. Same-size made the bug
                        // invisible here (see the pending_resize case below,
                        // where it was caught live), but the exposure is
                        // identical — set it unconditionally for safety.
                        self.suppress_next_adopt.store(true, Ordering::Relaxed);
                        return Ok(Some(DisplayUpdate::Resize(DesktopSize {
                            width: w,
                            height: h,
                        })));
                    }
                }

                // Client-driven live resize (window drag), debounced. Both
                // codec paths ride the SAME core deactivation-reactivation
                // (the vendored server's `DisplayUpdate::Resize` machinery):
                // after the reactivation, `client_loop` re-runs `updates()` and
                // a fresh `ScreenCaptureUpdates` is built at the new size, so
                // capture-side state rebuilds exactly like a fresh connect.
                //
                // The EGFX difference is one extra step: reset the per-
                // connection surface/encoder state FIRST (`reset_for_live_
                // resize`), so the first post-reactivation frame re-runs
                // `setup_locked` from scratch — RESET_GRAPHICS at the new
                // size, fresh surface, fresh VideoToolbox encoder, IDR —
                // exactly the sequence a brand-new connection gets, and the
                // resize response MS-RDPEDISP expects (deactivation-
                // reactivation + graphics reset, what real RDS servers send).
                // A channel-level surface swap withOUT the core reactivation
                // was tried first and was visually broken (blinking) on
                // Windows App for macOS despite clean wire mechanics.
                //
                // Write the new size into the shared `desktop_size` before
                // emitting (so input scaling sees it immediately) and arm
                // `suppress_next_adopt` — LIVE-VERIFIED 2026-07-09 against
                // Windows App for macOS that without it, the reactivation's
                // `request_initial_size` call re-adopts a DIFFERENT size from
                // the client's Confirm Active bitmap capset (its original
                // connect-time size, not the size we just told it via Demand
                // Active, and not the size it just told us via the
                // MonitorLayout PDU) — silently snapping the resize back
                // before the next frame ships.
                if let Some((w, h)) = self.pending_resize.take_settled(RESIZE_DEBOUNCE) {
                    self.desktop_size.set(w, h);
                    self.suppress_next_adopt.store(true, Ordering::Relaxed);
                    if let Some(gfx) = self.gfx.as_ref() {
                        gfx.reset_for_live_resize();
                    }
                    tracing::info!(w, h, "applying debounced client-driven resize");
                    return Ok(Some(DisplayUpdate::Resize(DesktopSize {
                        width: w,
                        height: h,
                    })));
                }

                // Poll cursor at loop top — preempts queued bitmap rects so
                // pointer shape updates keep up instead of batching behind the
                // bitmap drain of the previous SCK sample. Throttled to
                // ~15 Hz: each poll is a synchronous WindowServer IPC on the
                // capture/encode critical path, and 60 Hz buys nothing over
                // 15 Hz for shape changes (see the `last_cursor_poll` field
                // note — time-based, so animated cursors keep animating).
                const CURSOR_POLL_INTERVAL: Duration = Duration::from_millis(66);
                let poll_due = self
                    .last_cursor_poll
                    .is_none_or(|t| t.elapsed() >= CURSOR_POLL_INTERVAL);
                if poll_due {
                    self.last_cursor_poll = Some(Instant::now());
                    if let Some((cell, multiplier)) = self.live_pointer_scale.as_ref() {
                        let scale = cell.load(Ordering::Relaxed);
                        if scale != self.applied_pointer_scale {
                            self.applied_pointer_scale = scale;
                            self.cursor.set_scale(
                                multiplier.clamp(0.1, 8.0) * super::pointer_factor(scale),
                            );
                            tracing::info!(
                                pointer_ui_scale = scale,
                                "resending the pointer at a new scale"
                            );
                        }
                    }
                    let mut cursor_updates = self.cursor.poll();
                    if !cursor_updates.is_empty() {
                        let first = cursor_updates.remove(0);
                        for u in cursor_updates.into_iter().rev() {
                            self.pending.push_front(u);
                        }
                        return Ok(Some(first));
                    }
                }

                self.promote_frame_to_egfx();

                if let Some(update) = self.pending.pop_front() {
                    return Ok(Some(update));
                }

                // Client minimized (sent `SuppressOutput { desktop_rect: None }`)
                // — stop pulling SCK samples and stop encoding/shipping. Without
                // this, mstsc accumulates EGFX frames during a long minimize and
                // the refocus chew-through locks up its input dispatch for
                // seconds (typing/clicks queue behind decode-and-paint of every
                // buffered frame). Also tear down any in-flight flush burst —
                // re-submitting stale frames during a minimize is pointless.
                // Cursor + pending bitmap branches above still flow (they're
                // tiny and harmless to buffer at the client). Re-check the flag
                // every ~100 ms so resume is responsive.
                //
                // **Only honor suppress after the first EGFX frame has shipped.**
                // mstsc's normal connect handshake includes a
                // `SuppressOutput { None }` *before* its display surface is
                // ready; blocking the first frame leaves it with a half-init'd
                // surface that doesn't recover when we un-suppress (mstsc
                // freezes on the connection). FreeRDP doesn't issue suppress
                // during connect, so it was never affected. Gate the gate on
                // `first_egfx_frame_sent` so the handshake completes normally.
                //
                // **Debounce so transient flaps don't oscillate the pipeline.**
                // Under heavy local CPU/IO (cargo build) mstsc backs off
                // briefly when the encoded backlog grows, sending a quick
                // `SuppressOutput { None }` → `RefreshRectangle` pair (tens of
                // ms). Reacting to those rapid flaps stops/starts the video
                // pipeline and toggles the audio mute, both audible as
                // stutter. Track "first observed suppress" locally and only
                // engage the gate once the flag has been steady-`true` for
                // `SUPPRESS_DEBOUNCE`. Real multi-second minimizes still trip
                // normally.
                if self.first_egfx_frame_sent {
                    if let Some(flag) = self.display_suppressed.as_ref() {
                        if flag.load(Ordering::Relaxed) {
                            let started = *self.suppressed_since.get_or_insert_with(Instant::now);
                            if started.elapsed() >= SUPPRESS_DEBOUNCE {
                                self.was_suppressed = true;
                                self.flush_remaining = 0;
                                // A healthy minimized stream needs no frames.
                                // Still handle an explicit native stop while
                                // output is suppressed.
                                if let Some(stop) = self.stream.stop_reason() {
                                    let Some(first_frame) = self.reopen_stream(stop).await else {
                                        return Ok(None);
                                    };
                                    // Preserve the seed for a static desktop
                                    // after waking or restoring the window.
                                    let mut queue = self.stream.samples.queue.lock().unwrap();
                                    queue.samples.truncate(SAMPLE_BUFFER - 1);
                                    queue.samples.push_front(first_frame);
                                }
                                tokio::time::sleep(Duration::from_millis(100)).await;
                                continue;
                            }
                        } else {
                            self.suppressed_since = None;
                        }
                    }
                }

                // While a flush burst OR a debounced resize is pending, don't
                // block indefinitely on SCK: it stops delivering frames on a
                // static screen, so wait at most one frame interval before
                // looping back to the top (where the flush burst re-submits the
                // last frame, and a settled resize gets picked up promptly
                // instead of stalling until the next real desktop change).
                // Neither pending — the common idle case — still wakes at the
                // cursor-poll interval, so a pointer-shape change on a static
                // screen (the resize arrows at a window edge, which change no
                // pixels because the captured frame excludes the cursor) gets
                // polled and sent instead of waiting for the next redraw.
                // A frame held during the EGFX negotiation also waits at most
                // one frame interval, so it goes out once EGFX is ready even
                // when the screen is static.
                let sample = if self.flush_remaining > 0
                    || self.pending_resize.has_pending()
                    || self.held_frame
                {
                    match tokio::time::timeout(self.frame_interval, self.stream.next()).await {
                        Ok(sample) => sample,
                        Err(_) => {
                            if self.held_frame {
                                self.send_held_frame();
                                continue;
                            }
                            if self.flush_remaining > 0 {
                                self.flush_remaining -= 1;
                                if let Some(gfx) = self.gfx.as_ref() {
                                    if let Some(pb) = self.last_frame.as_ref() {
                                        if let Err(e) =
                                            submit_to_gfx(gfx, pb, self.nv12, false, true)
                                        {
                                            tracing::warn!(error = ?e, "EGFX flush submit_bgra failed");
                                        }
                                    }
                                }
                            }
                            continue;
                        }
                    }
                } else {
                    match tokio::time::timeout(CURSOR_POLL_INTERVAL, self.stream.next()).await {
                        Ok(sample) => sample,
                        Err(_) => continue,
                    }
                };
                let sample = match sample {
                    Ok(sample) => sample,
                    Err(stop) => match self.reopen_stream(stop).await {
                        Some(sample) => sample,
                        // The server ends the session on EOF, dropping the
                        // capture lease and running normal display cleanup.
                        None => return Ok(None),
                    },
                };

                // Skip non-renderable frames (Idle, Blank, Suspended, Stopped).
                if let Some(status) = sample.frame_status() {
                    if !status.has_content() {
                        continue;
                    }
                }

                let Some(pixel_buffer) = sample.image_buffer() else {
                    continue;
                };

                // Dimensions come from the buffer itself, no lock: in NV12 mode
                // the EGFX path never needs the pixels on the CPU at all.
                let pb_width =
                    u16::try_from(pixel_buffer.width()).context("pixel buffer width > u16")?;
                let pb_height =
                    u16::try_from(pixel_buffer.height()).context("pixel buffer height > u16")?;

                // Log the dimensions SCK actually delivers, once per session.
                // `pb_width`/`pb_height` are the real captured buffer size — at
                // backing (Retina) pixels with `--hidpi`, at logical points
                // otherwise. Confirms `--hidpi` took effect and that SCK didn't
                // silently downscale.
                if !self.seeded {
                    tracing::debug!(
                        pb_width,
                        pb_height,
                        pixel_format = format_args!("{:#010x}", pixel_buffer.pixel_format()),
                        force_full_frame = self.force_full_frame,
                        "capture: first frame delivered"
                    );
                    if self.nv12 {
                        log_nv12_luma_range(&pixel_buffer);
                    }
                }

                // EGFX/H.264 path: submit the full frame to the encoder. Once
                // EGFX has negotiated (`Ok(true)`), it owns the display — skip
                // the legacy BitmapUpdate emission entirely (cursor still flows
                // via the poll at the top of the loop). Before negotiation, or
                // for non-EGFX clients (`Ok(false)`), fall through to legacy.
                if let Some(gfx) = self.gfx.as_ref() {
                    // On-demand keyframes (opt-in via --keyframe-on-change). A
                    // large change at once (window raised to front, scroll, app
                    // launch) is applied cleanly by some clients (mstsc) only on a
                    // keyframe — as a P-frame it can render garbled/stale until the
                    // next periodic IDR. Force an IDR on the rising edge of a large
                    // dirty area; small changes (typing, caret) stay below the
                    // threshold and remain cheap P-frames. (We deliberately do NOT
                    // skip unchanged frames here — mstsc only flushes its ~2-frame
                    // presentation buffer when frames keep arriving, so a continuous
                    // stream is what keeps typing/window-switching snappy at 60fps.
                    // The vImage conversion makes a steady stream cheap anyway.)
                    let kfc = self.keyframe_on_change;
                    let big_change = if kfc.enabled {
                        let frame_px = u64::from(pb_width) * u64::from(pb_height);
                        // Dirty-rect area as a "how much moved" fraction (rects may
                        // be in source coords when scaling, but the ratio holds).
                        let changed_px: u64 = sample
                            .dirty_rects()
                            .map(|rects| {
                                rects
                                    .iter()
                                    .map(|r| {
                                        let s = r.size();
                                        (s.width.max(0.0) as u64) * (s.height.max(0.0) as u64)
                                    })
                                    .sum()
                            })
                            .unwrap_or(0);
                        // Briefly after a click, treat a much smaller change as
                        // keyframe-worthy too (a click usually precedes a UI
                        // update; the IDR still only fires if a change follows).
                        let high = if self
                            .click_signal
                            .as_ref()
                            .is_some_and(|s| s.within(kfc.click_window))
                        {
                            kfc.click_pct
                        } else {
                            kfc.change_pct
                        };
                        let low = (high / 2).max(1); // hysteresis re-arm band
                        let is_big = frame_px > 0 && changed_px * 100 >= frame_px * high;
                        let is_quiet = frame_px == 0 || changed_px * 100 < frame_px * low;
                        // Rising edge only: fire once when a large change begins,
                        // then stay quiet until it subsides below `low`. Without
                        // this, sustained churn (video, htop) above the threshold
                        // would force an IDR every frame and wreck quality.
                        if self.kf_armed && is_big {
                            self.kf_armed = false;
                            true
                        } else if is_quiet {
                            self.kf_armed = true;
                            false
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    // Force an IDR on the first frame after un-suppress: the
                    // P-frames we would have sent during the minimize depend
                    // on a reference frame mstsc may no longer hold (or its
                    // decoder state may have been torn down). Without a fresh
                    // keyframe the resume frame would decode against missing
                    // state and render garbled.
                    let resume_keyframe = if self.was_suppressed {
                        self.was_suppressed = false;
                        tracing::debug!("client un-suppress edge — forcing IDR on next encode");
                        // If EGFX is on the reliable UDP tunnel, mstsc's surface
                        // won't survive the minimize/restore — switch back to TCP
                        // BEFORE shipping the restore frame into the now-stale
                        // tunnel (no-op on TCP/lossy/default; see
                        // `Gfx::demigrate_on_resume`).
                        gfx.demigrate_on_resume();
                        true
                    } else {
                        false
                    };
                    match submit_to_gfx(
                        gfx,
                        &pixel_buffer,
                        self.nv12,
                        big_change || resume_keyframe,
                        false,
                    ) {
                        Ok(true) => {
                            self.held_frame = false;
                            self.seeded = true;
                            // First-EGFX-frame milestone: arms the suppress
                            // gate (see `first_egfx_frame_sent` in the struct).
                            if !self.first_egfx_frame_sent {
                                self.first_egfx_frame_sent = true;
                                tracing::debug!(
                                    "first EGFX frame shipped — suppress gate now armed"
                                );
                            }
                            // Stash this frame and arm the flush burst so that,
                            // once SCK goes idle after this change, we re-submit
                            // it enough times to drain mstsc's presentation
                            // buffer. A retain, not a copy (see `last_frame`).
                            self.last_frame = Some(pixel_buffer.clone());
                            self.flush_remaining = self.flush_frames;
                            continue;
                        }
                        // EGFX still negotiating on this connection: do not send
                        // the frame as a legacy update (Windows 10 mstsc can stay
                        // frozen on a legacy picture sent just before EGFX starts;
                        // see `hold_legacy_for_egfx`). Keep it: on a static screen
                        // no other frame may come (see `held_frame`).
                        Ok(false) if gfx.hold_legacy(self.started_at) => {
                            self.last_frame = Some(pixel_buffer.clone());
                            self.held_frame = true;
                            continue;
                        }
                        Ok(false) => {
                            // Not ready yet: this frame goes out as legacy
                            // bitmaps, but retain it to re-send over H.264
                            // once the pipeline comes up (a retain, not a
                            // copy — see `last_frame`).
                            if gfx.pipeline_pending()
                                && self.started_at.elapsed() < EGFX_PENDING_FRAME_WINDOW
                            {
                                self.last_frame = Some(pixel_buffer.clone());
                                self.egfx_pending_frame = true;
                            }
                        }
                        Err(e) => tracing::warn!(error = ?e, "EGFX submit_bgra failed"),
                    }
                }

                // This frame goes out as legacy, so a held one is obsolete.
                self.held_frame = false;

                // Legacy bitmap path: needs the pixels as BGRA on the CPU.
                let guard = pixel_buffer
                    .lock(CVPixelBufferLockFlags::READ_ONLY)
                    .map_err(|e| anyhow!("CVPixelBuffer::lock OSStatus {e}"))?;
                let (src, stride_bytes): (&[u8], usize) = if self.nv12 {
                    nv12_to_bgra(&guard, &mut self.nv12_bgra)?;
                    (&self.nv12_bgra, usize::from(pb_width) * 4)
                } else {
                    (guard.as_slice(), guard.bytes_per_row())
                };

                // Decide the rect set to emit. On the first frame we always
                // send the full frame so the client's bitmap cache is seeded.
                // After that, SCK's dirty_rects tells us what changed; if the
                // attachment is missing (older macOS, no key), fall back to
                // the full frame.
                let dirty = if !self.seeded || self.force_full_frame {
                    None
                } else {
                    sample.dirty_rects()
                };

                let rects: Vec<(u16, u16, u16, u16)> = match dirty {
                    Some(list) if !list.is_empty() => list
                        .into_iter()
                        .filter_map(|r| {
                            let origin = r.origin();
                            let size = r.size();
                            let x = origin.x.max(0.0).round() as u32;
                            let y = origin.y.max(0.0).round() as u32;
                            let w = size.width.max(0.0).round() as u32;
                            let h = size.height.max(0.0).round() as u32;
                            let x = u16::try_from(x.min(u32::from(pb_width))).ok()?;
                            let y = u16::try_from(y.min(u32::from(pb_height))).ok()?;
                            let w =
                                u16::try_from(w.min(u32::from(pb_width.saturating_sub(x)))).ok()?;
                            let h = u16::try_from(h.min(u32::from(pb_height.saturating_sub(y))))
                                .ok()?;
                            if w == 0 || h == 0 {
                                None
                            } else {
                                Some((x, y, w, h))
                            }
                        })
                        .collect(),
                    _ => vec![(0, 0, pb_width, pb_height)],
                };

                // Split oversized rects into strips so each BitmapUpdate stays
                // within the size mstsc will render (see split_strips). No-op
                // for already-small rects.
                for (x, y, w, h) in rects {
                    for (sx, sy, sw, sh) in split_strips(x, y, w, h) {
                        if let Some(update) = rect_update(src, stride_bytes, sx, sy, sw, sh) {
                            self.pending.push_back(update);
                        }
                    }
                }
                self.seeded = true;
            }
        }
    }

    #[async_trait::async_trait]
    impl RdpServerDisplayUpdates for ScreenCaptureUpdates {
        async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
            self.capture_next_update().await.map_err(display_error)
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod stub {
    use super::*;
    use anyhow::Context;
    use std::collections::VecDeque;

    pub struct StubUpdates {
        queue: VecDeque<DisplayUpdate>,
        /// When true, `next_update` returns `Ok(None)` once `queue` drains
        /// (ends the stream) instead of parking forever. Only the test
        /// constructor sets this; production `new` keeps the park-forever
        /// behavior so a real non-macOS build's update loop neither spins
        /// nor exits after the single seed frame.
        end_on_drain: bool,
    }

    impl StubUpdates {
        pub fn new(width: u16, height: u16) -> Result<Self> {
            let w = NonZeroU16::new(width).context("width must be > 0")?;
            let h = NonZeroU16::new(height).context("height must be > 0")?;
            let stride = NonZeroUsize::new(usize::from(width) * 4).context("stride must be > 0")?;
            let pixel_count = usize::from(width) * usize::from(height);
            let mut data = Vec::with_capacity(pixel_count * 4);
            for _ in 0..pixel_count {
                data.extend_from_slice(&[0xFF, 0x10, 0x80, 0x90]);
            }
            let mut queue = VecDeque::new();
            queue.push_back(DisplayUpdate::Bitmap(BitmapUpdate {
                x: 0,
                y: 0,
                width: w,
                height: h,
                format: PixelFormat::ARgb32,
                data: Bytes::from(data),
                stride,
            }));
            Ok(Self {
                queue,
                end_on_drain: false,
            })
        }

        /// Test-only: drive an explicit sequence of updates through the
        /// `RdpServerDisplayUpdates` trait, then end the stream. Lets the
        /// protocol-layer tests assert the update plumbing without a real
        /// ScreenCaptureKit backend.
        #[cfg(test)]
        pub fn with_updates(updates: impl IntoIterator<Item = DisplayUpdate>) -> Self {
            Self {
                queue: updates.into_iter().collect(),
                end_on_drain: true,
            }
        }
    }

    #[async_trait::async_trait]
    impl RdpServerDisplayUpdates for StubUpdates {
        async fn next_update(&mut self) -> ServerResult<Option<DisplayUpdate>> {
            if let Some(u) = self.queue.pop_front() {
                return Ok(Some(u));
            }
            if self.end_on_drain {
                return Ok(None);
            }
            std::future::pending::<()>().await;
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn locked_display_sleep_with_missing_source_disconnects_without_reopening() {
        let result = recover_capture(
            CaptureStop::MissingSource,
            || true,
            || -> std::future::Ready<Result<Option<()>, CaptureStop>> {
                panic!("a locked, sleeping display should not be reopened")
            },
        )
        .await;
        assert_eq!(result, CaptureRecovery::LockedDisplaySleep);
    }

    #[tokio::test(start_paused = true)]
    async fn awake_lock_screen_can_recover_even_if_sck_lost_its_source() {
        let mut attempts = 0;
        let result = recover_capture(
            CaptureStop::MissingSource,
            || false,
            || {
                attempts += 1;
                std::future::ready(Ok((attempts == 2).then_some(42)))
            },
        )
        .await;
        assert_eq!(attempts, 2);
        assert_eq!(result, CaptureRecovery::Restored(42));
    }

    #[tokio::test(start_paused = true)]
    async fn capture_recovery_deadline_covers_a_native_callback_that_never_arrives() {
        let now = tokio::time::Instant::now();
        let result = recover_capture(
            CaptureStop::Other,
            || true,
            std::future::pending::<Result<Option<()>, CaptureStop>>,
        )
        .await;
        assert_eq!(result, CaptureRecovery::TimedOut);
        assert_eq!(now.elapsed(), CAPTURE_RECOVERY_WINDOW);
    }

    #[tokio::test(start_paused = true)]
    async fn repeating_stream_stops_share_one_recovery_window() {
        let now = tokio::time::Instant::now();
        let result = recover_capture(
            CaptureStop::Other,
            || false,
            || async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                Err::<Option<()>, _>(CaptureStop::Other)
            },
        )
        .await;
        assert_eq!(result, CaptureRecovery::TimedOut);
        assert_eq!(now.elapsed(), CAPTURE_RECOVERY_WINDOW);
    }

    fn online(id: u32, vendor: u32, builtin: bool, size_px: (u32, u32)) -> OnlineDisplay {
        OnlineDisplay {
            id,
            vendor,
            builtin,
            size_px,
        }
    }

    #[test]
    fn a_reregistered_virtual_display_is_found_by_vendor_then_by_size() {
        const PANEL: u32 = 0x610;
        const ODD: u32 = 1;
        let ours = VIRTUAL_DISPLAY_VENDOR;
        let session = (3024, 1898);
        let same = [online(22, ours, false, session)];
        assert_eq!(pick_reopened_display(22, &same, session), Some(22));
        let by_vendor = [
            online(1, PANEL, true, (3024, 1964)),
            online(23, ours, false, session),
        ];
        assert_eq!(pick_reopened_display(22, &by_vendor, session), Some(23));
        let by_size = [
            online(1, PANEL, true, session),
            online(23, ODD, false, session),
        ];
        assert_eq!(pick_reopened_display(22, &by_size, session), Some(23));
        let ambiguous = [
            online(23, ODD, false, session),
            online(24, ODD, false, session),
        ];
        assert_eq!(pick_reopened_display(22, &ambiguous, session), None);
        let gone = [online(1, PANEL, true, (3024, 1964))];
        assert_eq!(pick_reopened_display(22, &gone, session), None);
    }

    /// #205: mirror the main display even when SCK lists another first.
    #[test]
    fn mirror_display_prefers_the_main_display() {
        // Main display listed second: pick it, not the first.
        assert_eq!(mirror_display_index(&[3, 1, 7], 1), Some(1));
        // Main listed first, or the only display: unchanged from before.
        assert_eq!(mirror_display_index(&[1, 3], 1), Some(0));
        assert_eq!(mirror_display_index(&[1], 1), Some(0));
        // Main not in SCK's list at all: fall back to the first, as before.
        assert_eq!(mirror_display_index(&[3, 7], 1), Some(0));
        // No displays: nothing to mirror.
        assert_eq!(mirror_display_index(&[], 1), None);
    }

    #[test]
    fn shared_desktop_size_roundtrips() {
        let size = SharedDesktopSize::new(1512, 982);
        assert_eq!(size.get(), (1512, 982));
        size.set(1920, 1080);
        assert_eq!(size.get(), (1920, 1080));
        // Clones observe the same cell — that's the whole point.
        let clone = size.clone();
        clone.set(u16::MAX, 1);
        assert_eq!(size.get(), (u16::MAX, 1));
    }

    #[test]
    fn negotiated_size_is_not_input_size_until_display_commit() {
        let size = SharedDesktopSize::new(3840, 2160);
        size.set(1920, 1080);
        assert_eq!(size.get(), (1920, 1080));
        assert_eq!(size.get_for_input(), (3840, 2160));
        size.clone().commit_input_size(1920, 1080);
        assert_eq!(size.get_for_input(), (1920, 1080));
    }

    #[test]
    fn new_session_clears_transients_and_invalidates_old_delayed_work() {
        let lifecycle = DisplayLifecycle::new();
        lifecycle.begin(Some(200));
        lifecycle.request();
        let old = lifecycle.advance_layout();
        lifecycle.pending.request(1600, 900);
        lifecycle.suppress.store(true, Ordering::Relaxed);
        lifecycle.end();
        assert!(!lifecycle.is_current(old));
        assert!(!lifecycle.pending.has_pending());
        assert!(!lifecycle.suppress.load(Ordering::Relaxed));
        lifecycle.begin(None);
        assert!(!lifecycle.is_current(old));
        assert_eq!(lifecycle.token(), (old.0 + 1, 0));
        assert!(lifecycle.is_current(lifecycle.token()));
    }

    #[test]
    fn newer_layout_cancels_only_previous_layout_work_not_the_session() {
        let lifecycle = DisplayLifecycle::new();
        lifecycle.begin(None);
        lifecycle.request();
        let first = lifecycle.advance_layout();
        lifecycle.request(); // Duplicate/no-op receipt does not cancel a sweep.
        assert!(lifecycle.is_current(first));
        let second = lifecycle.advance_layout();
        assert_eq!(first.0, second.0);
        assert!(second.1 > first.1);
        assert!(!lifecycle.is_current(first));
        assert!(lifecycle.is_current(second));
        // A server-driven reactivation consumes the echo flag, not begin/end.
        lifecycle.suppress.store(true, Ordering::Relaxed);
        assert!(lifecycle.suppress.swap(false, Ordering::Relaxed));
        assert!(lifecycle.is_current(second));
    }

    #[test]
    fn fixed_resolution_and_live_resize_do_not_override_scale_policy() {
        let current = (3840, 2160);
        let request = DesktopSize {
            width: 1920,
            height: 1080,
        };
        for auto in [false, true] {
            for live in [false, true] {
                let target = layout_target(auto, live, Some(1), 2, current, request).unwrap();
                assert_eq!(
                    (target.width, target.height),
                    if auto && live { (1920, 1080) } else { current }
                );
                let target = layout_target(auto, live, Some(2), 2, current, request);
                assert_eq!(target.is_some(), auto && live);
            }
        }
        assert!(layout_target(false, true, None, 2, current, request).is_none());
    }

    #[test]
    fn pointer_and_letterbox_use_measured_fractional_scales() {
        assert_eq!(pointer_factor(50), 0.5);
        assert_eq!(pointer_factor(100), 1.0);
        assert_eq!(pointer_factor(200), 2.0);
        let actual = crate::virtual_display::DisplayMeasurement {
            points_width: Some(1920.0),
            points_height: Some(1200.0),
            ..Default::default()
        };
        assert_eq!(aspect_preserving_scale(&actual, (3840, 2160)), Some(180.0));
        assert!(aspect_preserving_scale(&Default::default(), (3840, 2160)).is_none());
    }

    #[test]
    fn repeated_scale_pdus_keep_an_unapplied_scale_request() {
        let current = (3840, 2160);
        let requested = DesktopSize {
            width: 1920,
            height: 1080,
        };
        for (auto, live) in [(false, true), (true, false)] {
            for _ in 0..2 {
                let target = layout_target(auto, live, Some(1), 2, current, requested).unwrap();
                assert_eq!((target.width, target.height), current);
            }
            assert!(layout_target(auto, live, Some(2), 2, current, requested).is_none());
        }
    }

    #[test]
    fn returning_to_current_layout_cancels_a_pending_drag() {
        let pending = PendingResize::new();
        pending.request(2560, 1440);
        pending.clear(); // Every newly validated layout replaces the old intent.
        let current = (3840, 2160);
        assert!(layout_target(
            true,
            true,
            Some(1),
            1,
            current,
            DesktopSize {
                width: current.0,
                height: current.1
            }
        )
        .is_none());
        assert_eq!(pending.take_settled(Duration::ZERO), None);
    }

    #[test]
    fn pending_resize_starts_empty() {
        let pr = PendingResize::new();
        assert!(!pr.has_pending());
        assert_eq!(pr.take_settled(Duration::ZERO), None);
    }

    #[test]
    fn pending_resize_settles_once_debounce_elapses() {
        // `take_settled` takes the debounce as a parameter, so timing can be
        // tested deterministically without sleeping: a huge debounce never
        // elapses, a zero debounce elapses immediately.
        let pr = PendingResize::new();
        pr.request(1920, 1080);
        assert!(pr.has_pending());
        assert_eq!(pr.take_settled(Duration::from_secs(3600)), None);
        assert_eq!(pr.take_settled(Duration::ZERO), Some((1920, 1080)));
        // Consumed — nothing left to take, even at zero debounce.
        assert!(!pr.has_pending());
        assert_eq!(pr.take_settled(Duration::ZERO), None);
    }

    #[test]
    fn pending_resize_later_request_supersedes_earlier() {
        // A drag emits several layout PDUs; only the last size should apply.
        let pr = PendingResize::new();
        pr.request(1024, 768);
        pr.request(1920, 1080);
        assert_eq!(pr.take_settled(Duration::ZERO), Some((1920, 1080)));
    }

    #[test]
    fn split_strips_passes_small_rects_through() {
        // A rect already within the limit is returned unchanged (single strip).
        assert_eq!(split_strips(0, 0, 1280, 720), vec![(0, 0, 1280, 720)]);
        assert_eq!(split_strips(10, 20, 640, 480), vec![(10, 20, 640, 480)]);
        // Zero-area rects produce nothing.
        assert_eq!(split_strips(0, 0, 0, 100), Vec::new());
        assert_eq!(split_strips(0, 0, 100, 0), Vec::new());
    }

    #[test]
    fn split_strips_breaks_tall_rects_into_strips() {
        // 1920×1080 (≈2.1 MP) exceeds the 1280×720 cap and must be split.
        let strips = split_strips(0, 0, 1920, 1080);
        assert!(strips.len() > 1, "oversized rect should split: {strips:?}");
        // Each strip is within the per-update pixel budget.
        for &(_, _, w, h) in &strips {
            assert!(u32::from(w) * u32::from(h) <= MAX_BITMAP_UPDATE_PIXELS);
            assert_eq!(w, 1920, "width is preserved; only rows are split");
        }
        // Strips tile the original rect contiguously with no gaps/overlap and
        // cover the full height.
        assert_eq!(strips.first().unwrap().1, 0);
        let mut next_y = 0u16;
        for &(x, y, _, h) in &strips {
            assert_eq!(x, 0);
            assert_eq!(y, next_y);
            next_y += h;
        }
        assert_eq!(next_y, 1080, "strips cover the whole height");
    }

    #[test]
    fn adopt_client_size_adopts_in_band_change_when_auto() {
        // Auto-size on, a different in-band size → adopt it.
        assert_eq!(
            adopt_client_size(
                true,
                (1512, 982),
                DesktopSize {
                    width: 1920,
                    height: 1080
                }
            ),
            Some(DesktopSize {
                width: 1920,
                height: 1080
            })
        );
    }

    #[test]
    fn adopt_client_size_refuses_when_disabled_or_unchanged_or_out_of_band() {
        // auto_size off → never adopt.
        assert_eq!(
            adopt_client_size(
                false,
                (1512, 982),
                DesktopSize {
                    width: 1920,
                    height: 1080
                }
            ),
            None
        );
        // No-op echo (already current) → no change.
        assert_eq!(
            adopt_client_size(
                true,
                (1920, 1080),
                DesktopSize {
                    width: 1920,
                    height: 1080
                }
            ),
            None
        );
        // Below the 200..=8192 protocol band → refuse.
        assert_eq!(
            adopt_client_size(
                true,
                (1512, 982),
                DesktopSize {
                    width: 199,
                    height: 1080
                }
            ),
            None
        );
        // Above the band → refuse.
        assert_eq!(
            adopt_client_size(
                true,
                (1512, 982),
                DesktopSize {
                    width: 1920,
                    height: 8193
                }
            ),
            None
        );
        // Band edges are inclusive and adoptable.
        assert_eq!(
            adopt_client_size(
                true,
                (1512, 982),
                DesktopSize {
                    width: 200,
                    height: 8192
                }
            ),
            Some(DesktopSize {
                width: 200,
                height: 8192
            })
        );
    }

    #[test]
    fn cap_client_size_applies_the_operator_maximum() {
        let size = |width, height| DesktopSize { width, height };
        let max = Some(size(1920, 1080));
        // Issue #211: a client echoing 3840x2160 under a 1920x1080 cap.
        assert_eq!(cap_client_size(size(3840, 2160), max), size(1920, 1080));
        // Per dimension: a smaller side is kept.
        assert_eq!(cap_client_size(size(1280, 2160), max), size(1280, 1080));
        assert_eq!(cap_client_size(size(1280, 720), max), size(1280, 720));
        // No cap: unchanged.
        assert_eq!(cap_client_size(size(3840, 2160), None), size(3840, 2160));
        // Served at the cap already, the oversized echo is not adopted.
        assert_eq!(
            adopt_client_size(true, (1920, 1080), cap_client_size(size(3840, 2160), max)),
            None
        );
    }

    #[test]
    fn sck_frame_cap_only_for_a_faster_or_unknown_display() {
        assert!(!sck_needs_frame_cap(60, 60.0));
        assert!(!sck_needs_frame_cap(60, 59.94));
        assert!(!sck_needs_frame_cap(120, 60.0));
        assert!(sck_needs_frame_cap(60, 120.0));
        assert!(sck_needs_frame_cap(15, 60.0));
        assert!(sck_needs_frame_cap(60, 0.0));
    }

    #[test]
    fn capture_scaling_mode_native_or_backing_is_one_to_one() {
        let native = (1512, 982);
        let backing = (3024, 1964); // Retina backing pixels
                                    // Configured == native points: 1:1, no full frame, no letterbox.
        assert_eq!(
            capture_scaling_mode(native, backing, (1512, 982), true, false),
            (false, false)
        );
        // Configured == backing pixels (--hidpi): also 1:1, no full frame, no
        // letterbox — the HiDPI exemption that must NOT force full frames.
        assert_eq!(
            capture_scaling_mode(native, backing, (3024, 1964), true, false),
            (false, false)
        );
    }

    #[test]
    fn capture_scaling_mode_letterboxes_when_scaling_on_auto_size() {
        let native = (1512, 982);
        let backing = (3024, 1964);
        // Auto-size to a non-native size (scaling), no --stretch → letterbox.
        assert_eq!(
            capture_scaling_mode(native, backing, (1920, 1080), true, false),
            (true, true)
        );
        // --stretch opts back into fill: full frames, but no letterbox.
        assert_eq!(
            capture_scaling_mode(native, backing, (1920, 1080), true, true),
            (true, false)
        );
        // Explicit --width/--height (auto_size = false): full frames (SCK scales)
        // but the picture fills (stretch), so no letterbox.
        assert_eq!(
            capture_scaling_mode(native, backing, (1920, 1080), false, false),
            (true, false)
        );
    }

    // The programmable stub only exists on non-macOS builds (the Linux CI
    // target); it backs the protocol-layer trait tests below.
    #[cfg(not(target_os = "macos"))]
    #[tokio::test]
    async fn stub_updates_yields_injected_sequence_then_ends() {
        use stub::StubUpdates;

        let mk = |w: u16| {
            DisplayUpdate::Bitmap(BitmapUpdate {
                x: 0,
                y: 0,
                width: NonZeroU16::new(w).unwrap(),
                height: NonZeroU16::new(1).unwrap(),
                format: PixelFormat::ARgb32,
                data: Bytes::from(vec![0u8; usize::from(w) * 4]),
                stride: NonZeroUsize::new(usize::from(w) * 4).unwrap(),
            })
        };

        let mut updates: Box<dyn RdpServerDisplayUpdates + Send> =
            Box::new(StubUpdates::with_updates([mk(10), mk(20), mk(30)]));

        // Driven through the trait, the injected updates come back in order...
        for expected_w in [10u16, 20, 30] {
            match updates.next_update().await.unwrap() {
                Some(DisplayUpdate::Bitmap(b)) => assert_eq!(b.width.get(), expected_w),
                other => panic!("expected bitmap width {expected_w}, got {other:?}"),
            }
        }
        // ...and then the stream ends (the test constructor sets end_on_drain).
        assert!(updates.next_update().await.unwrap().is_none());
    }
    #[test]
    fn reconnect_that_ends_between_polls_still_invalidates_old_disconnect() {
        let tracker = SessionTracker::default();
        tracker.enter();
        tracker.leave();
        let old_epoch = tracker.connection_epoch.load(Ordering::SeqCst);
        tracker.enter();
        tracker.leave();
        assert_eq!(tracker.count.load(Ordering::SeqCst), 0);
        assert_ne!(tracker.connection_epoch.load(Ordering::SeqCst), old_epoch);
    }
}
