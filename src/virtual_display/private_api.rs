//! All touches to undocumented `CGVirtualDisplay*` private API live here.
//!
//! When Apple renames or restructures the surface in a future macOS,
//! this is the only file that should need to change. The public side
//! (in `mod.rs`) talks to this layer through stable Rust types only —
//! no `*mut AnyObject`, no private class names leak out.
//!
//! ## API surface
//!
//! On macOS 26+ the surface is a fully Obj-C class `CGVirtualDisplay`
//! with `-initWithDescriptor:` / `-applySettings:` / `-displayID`. The
//! older C functions `CGVirtualDisplayCreate` /
//! `CGVirtualDisplayApplySettings` / `CGVirtualDisplayGetDisplayID`
//! that BetterDisplay's wrappers cover were removed/renamed at some
//! point during the macOS 16/26 cycle. We don't currently support the
//! older path — if you need it, add a fallback in `create()` that
//! detects class absence via `AnyClass::get` and reaches for `dlsym`
//! on the C symbols instead.
//!
//! ## Maintenance contract
//!
//! - All Obj-C classes resolved via `AnyClass::get(...)`. A missing
//!   class surfaces as an `Err` naming what's gone.
//! - Selectors used: `alloc`, `init`, `release`, `setName:`,
//!   `setMaxPixelsWide:`, `setMaxPixelsHigh:`, `setSizeInMillimeters:`,
//!   `setProductID:`, `setVendorID:`, `setSerialNum:`,
//!   `initWithDescriptor:`, `initWithWidth:height:refreshRate:`,
//!   `arrayWithObject:`, `setHiDPI:`, `setModes:`, `applySettings:`,
//!   `displayID`. Confirmed against macOS 26.4 (`dyld_info -exports`
//!   on `/System/Library/Frameworks/CoreGraphics.framework/...`).

#![cfg(target_os = "macos")]

use std::ffi::{c_void, CString};

use anyhow::{anyhow, Result};
use core_foundation::base::{CFRelease, CFType, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::dictionary::{CFDictionary, CFDictionaryRef};
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};
use objc2_foundation::{CGSize, NSString};

type CGDirectDisplayID = u32;

/// `CGSessionCopyCurrentDictionary()` — undocumented CoreGraphics/SkyLight
/// symbol, the same "CGS" private-API family as `CGSConfigureDisplayEnabled`
/// above. Returns a dictionary describing the current login-session state;
/// the also-undocumented `CGSSessionScreenIsLocked` key reads `1` while the
/// lock screen or screen saver is active. This alone does not establish that
/// a password is required: the screen-saver grace period may still be running.
/// Retained as the READ gate for the reconnect-time auto-unlock attempt;
/// disconnect lock confirmation uses `screen_authentication_is_required`.
///
/// Returns `None` when the lookup itself couldn't be completed (missing
/// symbol, or a null dictionary) — genuinely "unknown," not "not locked."
/// **This distinction is load-bearing at the call site:** `attempt_auto_unlock`
/// treats a confirmed `Some(false)` as a lock-cycle boundary and resets the
/// shared per-lock submission budget there, but must NOT do that on a mere
/// lookup failure — an intermittently-flaky symbol would otherwise reset the
/// budget on every failed read, defeating the cap `AUTO_UNLOCK_MAX_SUBMISSIONS`
/// exists to enforce. Deciding *whether to attempt* an unlock can still fail
/// toward "skip" on `None` (safe, same as before); it's specifically the
/// budget-reset side that needed the tri-state.
///
/// **If Apple ever removes this** (as happened to the `CGSession` *binary*
/// on macOS 26 — see docs/known-quirks.md, the `--lock-on-disconnect`
/// entry), the documented longer-lived alternative is watching the
/// `com.apple.screenIsLocked` / `com.apple.screenIsUnlocked` distributed
/// notifications (`NSDistributedNotificationCenter`) instead — an older
/// mechanism with a longer track record, not a drop-in replacement for this
/// function's shape.
pub(super) fn screen_is_locked() -> Option<bool> {
    screen_flag(&copy_session_dictionary()?)
}

fn copy_session_dictionary() -> Option<CFDictionary<CFString, CFType>> {
    unsafe {
        let rtld_default = -2isize as *mut c_void;
        let name = CString::new("CGSessionCopyCurrentDictionary").unwrap();
        let sym = libc::dlsym(rtld_default, name.as_ptr());
        if sym.is_null() {
            return None;
        }
        let copy_dict: unsafe extern "C" fn() -> CFDictionaryRef = std::mem::transmute(sym);
        let dict = copy_dict();
        if dict.is_null() {
            return None;
        }
        // The dictionary follows the CF Copy rule. Typed access checks the
        // values before interpreting them; a changed private shape is unknown.
        Some(CFDictionary::wrap_under_create_rule(dict))
    }
}

fn screen_flag(session: &CFDictionary<CFString, CFType>) -> Option<bool> {
    match session.find(CFString::new("CGSSessionScreenIsLocked")) {
        Some(value) => value.downcast::<CFBoolean>().map(bool::from),
        // loginwindow omits this flag for an unlocked desktop.
        None => Some(false),
    }
}

/// Separate the screen-saver flag from an authentication-required lock.
/// Primary-source implementation and time boundary:
/// https://github.com/openai/codex/blob/main/codex-rs/tui/src/session_lock/macos.rs
/// Both keys are private. A missing/malformed deadline while the flag is true
/// remains unknown, so callers cannot claim that a password is required.
pub(super) fn screen_authentication_is_required() -> Option<bool> {
    let session = copy_session_dictionary()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs();
    authentication_required(&session, i64::try_from(now).ok()?)
}

fn authentication_required(session: &CFDictionary<CFString, CFType>, now: i64) -> Option<bool> {
    if !screen_flag(session)? {
        return Some(false);
    }
    let deadline = session
        .find(CFString::new("CGSSessionScreenLockedTime"))?
        .downcast::<CFNumber>()?
        .to_i64()?;
    Some((1..=now).contains(&deadline))
}

/// `CGSConfigureDisplayEnabled(config, display, enabled)` — undocumented
/// SkyLight symbol re-exported from CoreGraphics on macOS 26. Called
/// between `CGBeginDisplayConfiguration` / `CGCompleteDisplayConfiguration`
/// to enable or disable a display from the WindowServer's perspective
/// (backlight off, no menu bar, no windows can be placed there).
pub(super) struct DisplayEnabler(unsafe extern "C" fn(*mut c_void, CGDirectDisplayID, bool) -> i32);

impl DisplayEnabler {
    pub(super) fn load() -> Result<Self> {
        unsafe {
            let rtld_default = -2isize as *mut c_void;
            let name = CString::new("CGSConfigureDisplayEnabled").unwrap();
            let p = libc::dlsym(rtld_default, name.as_ptr());
            if p.is_null() {
                return Err(anyhow!(
                    "private CoreGraphics symbol `CGSConfigureDisplayEnabled` \
                     not found — display detach is unavailable on this macOS"
                ));
            }
            Ok(Self(std::mem::transmute::<
                *mut c_void,
                unsafe extern "C" fn(*mut c_void, CGDirectDisplayID, bool) -> i32,
            >(p)))
        }
    }

    /// Apply the enabled/disabled state inside an active CG display config.
    /// `config` is the `CGDisplayConfigRef` returned by
    /// `CGDisplay::begin_configuration` — its layout is `*mut c_void` in
    /// the core-graphics crate, which is what the FFI expects.
    pub(super) fn set(&self, config: *mut c_void, display: u32, enabled: bool) -> Result<(), i32> {
        let err = unsafe { (self.0)(config, display, enabled) };
        if err == 0 {
            Ok(())
        } else {
            Err(err)
        }
    }
}

fn class_required(name: &str) -> Result<&'static AnyClass> {
    AnyClass::get(name).ok_or_else(|| {
        anyhow!(
            "private Obj-C class `{name}` not registered in the runtime — \
             virtual-display support is unavailable on this macOS"
        )
    })
}

/// Owned `CGVirtualDisplay` instance. Released via Obj-C `release` on
/// Drop, which is what un-registers the display from the system.
pub(super) struct Handle {
    raw: *mut AnyObject,
    display_id: u32,
}

// SAFETY: CGVirtualDisplay is a CF-bridged Obj-C class; CF types are
// thread-safe. Our usage is single-threaded but the wrapper crosses
// thread boundaries in main's task setup.
unsafe impl Send for Handle {}

impl Handle {
    pub(super) fn display_id(&self) -> u32 {
        self.display_id
    }

    pub(super) fn live_display_id(&self) -> u32 {
        unsafe { msg_send![self.raw, displayID] }
    }

    /// Re-apply settings with a single new mode — the live-resize primitive.
    /// Same `applySettings:` call that registered the display in `create`;
    /// calling it again on the live instance switches the active mode (the
    /// WindowServer relays out windows like a physical monitor changing
    /// resolution). The new mode must fit the descriptor's max dimensions
    /// (8192×8192, set in `create`).
    /// `hidpi` is re-sent on every call because `applySettings:` replaces the
    /// whole settings object; it can also switch a live display between a
    /// scaled and a 1:1 mode.
    pub(super) fn apply_mode(
        &self,
        width: u32,
        height: u32,
        refresh_hz: u32,
        hidpi: bool,
    ) -> Result<()> {
        apply_single_mode(self.raw, width, height, refresh_hz, hidpi)
    }
}

/// Build a one-mode `CGVirtualDisplaySettings` and `applySettings:` it onto
/// `display`. Shared by `create` (initial registration) and
/// `Handle::apply_mode` (live resize). The temporaries are leaked, same
/// rationale as in `create` — tiny, rare, and obviously double-free-proof.
///
/// With `hidpi`, macOS reads the mode's width/height as POINTS and backs the
/// display with a 2× pixel framebuffer — provided the descriptor's physical
/// size makes that framebuffer Retina-dense (see `create`); otherwise it picks
/// a 1:1 duplicate mode at twice the point size instead.
fn apply_single_mode(
    display: *mut AnyObject,
    width: u32,
    height: u32,
    refresh_hz: u32,
    hidpi: bool,
) -> Result<()> {
    let settings_class = class_required("CGVirtualDisplaySettings")?;
    let mode_class = class_required("CGVirtualDisplayMode")?;
    let nsarray_class = class_required("NSArray")?;

    unsafe {
        let mode: *mut AnyObject = msg_send![mode_class, alloc];
        let mode: *mut AnyObject = msg_send![
            mode,
            initWithWidth: width,
            height: height,
            refreshRate: f64::from(refresh_hz),
        ];
        if mode.is_null() {
            return Err(anyhow!("[CGVirtualDisplayMode init...] returned nil"));
        }
        let modes: *mut AnyObject = msg_send![nsarray_class, arrayWithObject: mode];

        let settings: *mut AnyObject = msg_send![settings_class, alloc];
        let settings: *mut AnyObject = msg_send![settings, init];
        if settings.is_null() {
            return Err(anyhow!("[CGVirtualDisplaySettings init] returned nil"));
        }
        let _: () = msg_send![settings, setHiDPI: u32::from(hidpi)];
        let _: () = msg_send![settings, setModes: modes];

        let ok: bool = msg_send![display, applySettings: settings];
        if !ok {
            return Err(anyhow!(
                "[CGVirtualDisplay applySettings:] returned false — mode {width}x{height}@{refresh_hz} \
                 is likely unsupported"
            ));
        }
    }
    Ok(())
}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.raw.is_null() {
            unsafe {
                let _: () = msg_send![self.raw, release];
            }
            self.raw = std::ptr::null_mut();
        }
    }
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    static kCGDisplayShowDuplicateLowResolutionModes: CFTypeRef;
    fn CGDisplayCopyAllDisplayModes(display: u32, options: CFTypeRef) -> CFTypeRef;
    fn CGDisplayModeGetWidth(mode: CFTypeRef) -> usize;
    fn CGDisplayModeGetHeight(mode: CFTypeRef) -> usize;
    fn CGDisplayModeGetPixelWidth(mode: CFTypeRef) -> usize;
    fn CGBeginDisplayConfiguration(config: *mut *mut c_void) -> i32;
    fn CGConfigureDisplayWithDisplayMode(
        config: *mut c_void,
        display: u32,
        mode: CFTypeRef,
        options: CFTypeRef,
    ) -> i32;
    fn CGCompleteDisplayConfiguration(config: *mut c_void, option: u32) -> i32;
    fn CGCancelDisplayConfiguration(config: *mut c_void) -> i32;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    static kCFBooleanTrue: CFTypeRef;
    static kCFTypeDictionaryKeyCallBacks: c_void;
    static kCFTypeDictionaryValueCallBacks: c_void;
    fn CFDictionaryCreate(
        alloc: CFTypeRef,
        keys: *const CFTypeRef,
        values: *const CFTypeRef,
        n: isize,
        kcb: *const c_void,
        vcb: *const c_void,
    ) -> CFTypeRef;
    fn CFArrayGetCount(a: CFTypeRef) -> isize;
    fn CFArrayGetValueAtIndex(a: CFTypeRef, i: isize) -> CFTypeRef;
}

/// `kCGConfigureForSession`.
const CONFIGURE_FOR_SESSION: u32 = 1;

/// Put `display` on the mode of `w × h` points backed by `px_w` pixels, the
/// way picking a resolution in System Settings does. `Ok(false)` when the
/// display doesn't list such a mode.
///
/// macOS stores a preferred mode per display and, after `applySettings:`,
/// restores it whenever the new mode list still contains it, instead of
/// taking the mode just applied. This replaces the stored mode. The scope
/// is the login session: the caller exits, which undoes an app-scoped
/// change, and a permanent one would also write the display preferences.
///
/// Only for a process that exits right after: the caller's own view of the
/// display (`CGDisplayBounds`, `CGDisplayCopyDisplayMode`) stops following
/// later mode changes once it has done this.
///
/// Reads the modes through the C API: `core-graphics` 0.24's
/// `CGDisplayMode::all_display_modes` over-releases them.
pub(super) fn select_mode(display: u32, w: u32, h: u32, px_w: u32) -> Result<bool> {
    unsafe {
        let keys = [kCGDisplayShowDuplicateLowResolutionModes];
        let vals = [kCFBooleanTrue];
        let opts = CFDictionaryCreate(
            std::ptr::null(),
            keys.as_ptr(),
            vals.as_ptr(),
            1,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        );
        let modes = CGDisplayCopyAllDisplayModes(display, opts);
        CFRelease(opts);
        if modes.is_null() {
            return Ok(false);
        }
        let mut found: CFTypeRef = std::ptr::null();
        for i in 0..CFArrayGetCount(modes) {
            let m = CFArrayGetValueAtIndex(modes, i);
            if CGDisplayModeGetWidth(m) == w as usize
                && CGDisplayModeGetHeight(m) == h as usize
                && CGDisplayModeGetPixelWidth(m) == px_w as usize
            {
                found = m;
                break;
            }
        }
        if found.is_null() {
            CFRelease(modes);
            return Ok(false);
        }
        let mut config: *mut c_void = std::ptr::null_mut();
        let e = CGBeginDisplayConfiguration(&mut config);
        if e != 0 {
            CFRelease(modes);
            return Err(anyhow!("CGBeginDisplayConfiguration failed (CGError {e})"));
        }
        let e = CGConfigureDisplayWithDisplayMode(config, display, found, std::ptr::null());
        if e != 0 {
            CGCancelDisplayConfiguration(config);
            CFRelease(modes);
            return Err(anyhow!(
                "CGConfigureDisplayWithDisplayMode failed (CGError {e})"
            ));
        }
        let e = CGCompleteDisplayConfiguration(config, CONFIGURE_FOR_SESSION);
        CFRelease(modes);
        if e != 0 {
            return Err(anyhow!(
                "CGCompleteDisplayConfiguration failed (CGError {e})"
            ));
        }
        Ok(true)
    }
}

#[derive(Debug, thiserror::Error)]
#[error("CGVirtualDisplay rejected the descriptor (initWithDescriptor: returned nil)")]
pub(super) struct DescriptorRejected;

/// Allocate a virtual display at the requested pixel size, apply a
/// single-mode settings object, and return the owning handle.
///
/// Temporary Obj-C mode/settings objects
/// are intentionally leaked — they're tiny, and avoiding manual `release` calls keeps the code
/// obviously free of double-free hazards.
///
/// `width`/`height` are the mode size: pixels when `hidpi` is false, points
/// (backed by 2× pixels) when it is true. `size_mm` is the physical panel
/// size the display reports; with `hidpi` it decides whether macOS actually
/// selects the scaled mode, so the caller picks it (`panel_size_mm`).
/// `hidpi_capable` marks a display that may run HiDPI at some point in its
/// life (it gets its own product id); `hidpi` is the initial mode.
pub(super) fn create(
    width: u32,
    height: u32,
    refresh_hz: u32,
    name: &str,
    hidpi_capable: bool,
    hidpi: bool,
    size_mm: (f64, f64),
) -> Result<Handle> {
    let desc_class = class_required("CGVirtualDisplayDescriptor")?;
    let display_class = class_required("CGVirtualDisplay")?;

    let ns_name = NSString::from_str(name);

    unsafe {
        // 1. Descriptor: alloc/init + property setters. Every property
        //    is cosmetic except the pixel dimensions; the rest just
        //    populates the metadata Displays.app shows for the monitor.
        let desc: *mut AnyObject = msg_send![desc_class, alloc];
        let desc: *mut AnyObject = msg_send![desc, init];
        if desc.is_null() {
            return Err(anyhow!("CGVirtualDisplayDescriptor init returned nil"));
        }
        // Rust worker threads do not pump the main dispatch queue. Give
        // WindowServer a serviced queue for registration and teardown.
        extern "C" {
            fn dispatch_get_global_queue(identifier: isize, flags: usize) -> *mut AnyObject;
        }
        let queue = dispatch_get_global_queue(0, 0);
        let _: () = msg_send![desc, setQueue: queue];
        let _: () = msg_send![desc, setName: &*ns_name];
        // Max dimensions are a CAP fixed for the display's lifetime (the
        // descriptor can't be re-applied), not the active resolution — the
        // mode below is what sizes the framebuffer. Advertise the RDP
        // protocol maximum (8192, MS-RDPBCGR) instead of the initial mode
        // size so a live resize (`apply_mode`) can later switch to any
        // protocol-legal mode, including ones larger than the first.
        let _: () = msg_send![desc, setMaxPixelsWide: 8192u32];
        let _: () = msg_send![desc, setMaxPixelsHigh: 8192u32];
        // Not just a label when hiDPI is set: macOS chooses between the
        // scaled mode and its 1:1 duplicate by pixel density, which it
        // derives from this size. Fixed for the display's lifetime.
        let _: () = msg_send![
            desc,
            setSizeInMillimeters: CGSize { width: size_mm.0, height: size_mm.1 }
        ];
        // A separate product id for HiDPI: macOS remembers a display's chosen
        // mode by vendor/product, and the regular display has always run 1:1.
        // Tests that create a real display ("macT") must not share an identity
        // with a running macrdp: macOS applies mode settings per vendor/product,
        // so re-moding one display also re-modes the other (observed: a test's
        // re-modes dragged the agent's display from 3456×2170 to 800×502).
        let product: u32 = if cfg!(test) {
            0x6D616354 // "macT"
        } else if hidpi_capable {
            0x6D616348 // "macH"
        } else {
            0x6D616372 // "macr"
        };
        let _: () = msg_send![desc, setProductID: product];
        let _: () = msg_send![desc, setVendorID: super::identity::VENDOR];
        // Was hardcoded to 1: a second macrdp process (e.g. two concurrent
        // installs, or dev testing alongside a running instance) registering a
        // display with an identical vendor/product/serial triple gets its
        // descriptor rejected by CGVirtualDisplay initWithDescriptor: outright
        // (returns nil). Per-process id keeps concurrent instances from colliding.
        let _: () = msg_send![desc, setSerialNum: std::process::id()];

        // 2. CGVirtualDisplay instance from the descriptor.
        let display: *mut AnyObject = msg_send![display_class, alloc];
        let display: *mut AnyObject = msg_send![display, initWithDescriptor: desc];
        let _: () = msg_send![desc, release];
        if display.is_null() {
            return Err(DescriptorRejected.into());
        }

        // 3+4. One mode at the requested resolution, applied via settings —
        //      the applySettings: call is what actually registers the display
        //      with the WindowServer so it appears in Displays. Shared with
        //      the live-resize path (`Handle::apply_mode`).
        if let Err(e) = apply_single_mode(display, width, height, refresh_hz, hidpi) {
            let _: () = msg_send![display, release];
            return Err(e);
        }

        // 5. Read back the assigned displayID.
        let display_id: u32 = msg_send![display, displayID];
        if display_id == 0 {
            let _: () = msg_send![display, release];
            return Err(anyhow!(
                "[CGVirtualDisplay displayID] returned 0 — display registered \
                 but the system hasn't assigned a directDisplayID"
            ));
        }

        Ok(Handle {
            raw: display,
            display_id,
        })
    }
}

#[cfg(test)]
mod lock_state_tests {
    use super::*;

    fn session(flag: Option<CFType>, deadline: Option<CFType>) -> CFDictionary<CFString, CFType> {
        let mut fields = Vec::new();
        if let Some(flag) = flag {
            fields.push((CFString::new("CGSSessionScreenIsLocked"), flag));
        }
        if let Some(deadline) = deadline {
            fields.push((CFString::new("CGSSessionScreenLockedTime"), deadline));
        }
        CFDictionary::from_CFType_pairs(&fields)
    }

    #[test]
    fn screen_saver_grace_does_not_count_as_an_authenticated_lock() {
        let deadline = 1_794_900_000_i64;
        let session = session(
            Some(CFBoolean::true_value().as_CFType()),
            Some(CFNumber::from(deadline).as_CFType()),
        );
        // Auto-unlock still sees an active lock UI during the grace period.
        assert_eq!(screen_flag(&session), Some(true));
        assert_eq!(authentication_required(&session, deadline - 1), Some(false));
        assert_eq!(authentication_required(&session, deadline), Some(true));
        assert_eq!(authentication_required(&session, deadline + 1), Some(true));
    }

    #[test]
    fn an_inactive_screen_flag_never_confirms_a_password_lock() {
        for flag in [None, Some(CFBoolean::false_value().as_CFType())] {
            let session = session(flag, Some(CFNumber::from(1_i64).as_CFType()));
            assert_eq!(screen_flag(&session), Some(false));
            assert_eq!(authentication_required(&session, 10), Some(false));
        }
    }

    #[test]
    fn missing_or_changed_private_values_remain_unknown() {
        for deadline in [None, Some(CFString::new("not a timestamp").as_CFType())] {
            let session = session(Some(CFBoolean::true_value().as_CFType()), deadline);
            assert_eq!(screen_flag(&session), Some(true));
            assert_eq!(authentication_required(&session, 10), None);
        }
        let session = session(Some(CFNumber::from(1_i64).as_CFType()), None);
        assert_eq!(screen_flag(&session), None);
        assert_eq!(authentication_required(&session, 10), None);
    }

    #[test]
    fn zero_or_negative_deadline_does_not_require_authentication() {
        for deadline in [0_i64, -1] {
            let session = session(
                Some(CFBoolean::true_value().as_CFType()),
                Some(CFNumber::from(deadline).as_CFType()),
            );
            assert_eq!(authentication_required(&session, 10), Some(false));
        }
    }
}
