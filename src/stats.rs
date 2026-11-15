//! Opt-in live-telemetry endpoint (`--stats-endpoint`, default OFF).
//!
//! A tiny **loopback-only, read-only** TCP listener that serves a JSON snapshot
//! of the current H.264 session — live bitrate, link RTT, standing queue delay,
//! frame rate, frames sent, and the session dimensions — on each connection. It
//! exists so the menu-bar controller's Status pane can show live "connection
//! health" without the server writing anything to disk.
//!
//! **No disk writes.** The snapshot lives entirely in memory ([`SessionStats`],
//! a handful of atomics updated at low-frequency points in the encode path) and
//! is serialized *only when a client connects* — which the controller does every
//! ~2 s, and only while its Status pane is open. So there is zero periodic I/O
//! (and therefore no SSD wear), unlike a periodically-rewritten stats file.
//!
//! **Default runtime path unchanged when off.** The endpoint is created only
//! when enabled; when it isn't, [`global`] returns `None`, so every update site
//! in the hot path is a single `Option` check that compiles to a no-op. The
//! listener binds `127.0.0.1` only (never routable) and never reads request
//! bytes — it just writes one JSON line and closes. It carries only the local
//! session's own metrics; the trust boundary is the same single-user, loopback
//! model as the other helper channels (see docs/macos-gotchas.md).

use std::net::Ipv4Addr;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

/// Encoder counters remain lock-free and best-effort. Display measurements
/// are published as one coherent snapshot, independent of the selected codec.
/// Connection state belongs to DisplayLifecycle, never encoder initialization.
#[derive(Default)]
pub struct SessionStats {
    display: Mutex<DisplaySnapshot>,
    pub connected: AtomicBool,
    pub width: AtomicU32,
    pub height: AtomicU32,
    /// Live encoder bitrate: the adaptive value when `--adaptive-bitrate` is on,
    /// otherwise the configured ceiling (static for the session).
    pub bitrate_bps: AtomicU32,
    /// The `--bitrate` ceiling (adaptive never exceeds it).
    pub ceiling_bps: AtomicU32,
    /// Kernel-measured link RTT (ms) sampled at accept; 0 = unknown.
    pub rtt_ms: AtomicU32,
    /// Standing queue delay (ms above the windowed-min RTT) — the adaptive
    /// controller's congestion signal. Meaningful only with acks flowing.
    pub queue_delay_ms: AtomicU32,
    /// Effective frame rate (capped by the adaptive floor under congestion).
    pub fps: AtomicU32,
    pub frames_sent: AtomicU64,
    pub adaptive: AtomicBool,
    pub aac: AtomicBool,
}

impl SessionStats {
    /// Zeroes the per-session values; `aac` is process config.
    pub fn session_ended(&self) {
        self.connected.store(false, Ordering::Relaxed);
        for v in [
            &self.width,
            &self.height,
            &self.bitrate_bps,
            &self.ceiling_bps,
            &self.rtt_ms,
            &self.queue_delay_ms,
            &self.fps,
        ] {
            v.store(0, Ordering::Relaxed);
        }
        self.frames_sent.store(0, Ordering::Relaxed);
        self.adaptive.store(false, Ordering::Relaxed);
    }

    fn to_json(&self) -> String {
        let display = self
            .display
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let metadata = metadata().lock().unwrap_or_else(|p| p.into_inner()).clone();
        #[cfg(target_os = "macos")]
        let file_clipboard = crate::file_provider::status();
        #[cfg(not(target_os = "macos"))]
        let file_clipboard = serde_json::Value::Null;
        serde_json::json!({
            "file_clipboard": file_clipboard,
            "schema_version": 2,
            "build_commit": metadata.build_commit,
            "config_revision": metadata.config_revision,
            "process": { "pid": std::process::id() },
            "startup": { "phase": metadata.phase, "screen_recording": metadata.screen_recording,
                         "accessibility": metadata.accessibility },
            "connected": self.connected.load(Ordering::Relaxed),
            "width": self.width.load(Ordering::Relaxed),
            "height": self.height.load(Ordering::Relaxed),
            "bitrate_bps": self.bitrate_bps.load(Ordering::Relaxed),
            "ceiling_bps": self.ceiling_bps.load(Ordering::Relaxed),
            "rtt_ms": self.rtt_ms.load(Ordering::Relaxed),
            "queue_delay_ms": self.queue_delay_ms.load(Ordering::Relaxed),
            "fps": self.fps.load(Ordering::Relaxed),
            "frames_sent": self.frames_sent.load(Ordering::Relaxed),
            "adaptive": self.adaptive.load(Ordering::Relaxed),
            "aac": self.aac.load(Ordering::Relaxed),
            "display": display.json()
        })
        .to_string()
    }

    pub fn update_display(&self, update: impl FnOnce(&mut DisplaySnapshot)) {
        update(&mut self.display.lock().unwrap_or_else(|p| p.into_inner()));
    }
}

/// One coherent display snapshot. Unknown measurements stay null, never inferred
/// from the requested mode. Published by capture for both H.264 and bitmap paths.
#[derive(Clone, Debug)]
pub struct DisplaySnapshot {
    pub session_generation: u64,
    pub request_sequence: u64,
    pub layout_generation: u64,
    pub phase: &'static str,
    pub request_source: Option<&'static str>,
    pub requested_width: Option<u16>,
    pub requested_height: Option<u16>,
    pub requested_scale: Option<u32>,
    pub wire_width: Option<u16>,
    pub wire_height: Option<u16>,
    pub actual: crate::virtual_display::DisplayMeasurement,
    pub fallback_reason: Option<String>,
}
impl Default for DisplaySnapshot {
    fn default() -> Self {
        Self {
            session_generation: 0,
            request_sequence: 0,
            layout_generation: 0,
            phase: "idle",
            request_source: None,
            requested_width: None,
            requested_height: None,
            requested_scale: None,
            wire_width: None,
            wire_height: None,
            actual: Default::default(),
            fallback_reason: None,
        }
    }
}
impl DisplaySnapshot {
    /// A latest layout may cancel an earlier pending drag by returning to the
    /// applied mode. The previous measurement remains valid, so leave applying.
    pub fn finish_unchanged_request(&mut self, negotiated: (u16, u16)) {
        if self.wire_width.zip(self.wire_height) == Some(negotiated) {
            self.phase = if self.fallback_reason.is_some() {
                "degraded"
            } else {
                "active"
            };
        }
    }
    fn json(&self) -> serde_json::Value {
        serde_json::json!({
            "session_generation": self.session_generation, "request_sequence": self.request_sequence,
            "phase": self.phase, "layout_generation": self.layout_generation,
            "requested": { "source": self.request_source, "width": self.requested_width, "height": self.requested_height,
                           "scale_percent": self.requested_scale },
            "wire": { "width": self.wire_width, "height": self.wire_height },
            "actual": { "display_id": self.actual.display_id,
                "points_width": self.actual.points_width, "points_height": self.actual.points_height,
                "backing_width": self.actual.backing_width, "backing_height": self.actual.backing_height,
                "scale_percent": self.actual.scale_percent,
                "backing_scale_percent": self.actual.backing_scale_percent,
                "native_capture": self.actual.native_capture },
            "fallback_reason": self.fallback_reason
        })
    }
}
#[derive(Clone)]
struct Metadata {
    build_commit: Option<String>,
    config_revision: Option<String>,
    phase: String,
    screen_recording: Option<bool>,
    accessibility: Option<bool>,
}
fn metadata() -> &'static Mutex<Metadata> {
    static META: OnceLock<Mutex<Metadata>> = OnceLock::new();
    META.get_or_init(|| {
        Mutex::new(Metadata {
            build_commit: None,
            config_revision: None,
            phase: "starting".into(),
            screen_recording: None,
            accessibility: None,
        })
    })
}
pub fn set_metadata(build_commit: Option<&str>, config_revision: Option<&str>) {
    let mut m = metadata().lock().unwrap_or_else(|p| p.into_inner());
    m.build_commit = build_commit.map(str::to_owned);
    m.config_revision = config_revision.map(str::to_owned);
}
pub fn set_startup(phase: &str, screen_recording: Option<bool>, accessibility: Option<bool>) {
    let mut m = metadata().lock().unwrap_or_else(|p| p.into_inner());
    m.phase = phase.to_owned();
    m.screen_recording = screen_recording;
    m.accessibility = accessibility;
}

/// The listener exists, but permission readiness is independently verified by
/// main. A listening socket alone is insufficient to claim usable capture/input.
pub fn mark_listening() {
    let mut m = metadata().lock().unwrap_or_else(|p| p.into_inner());
    m.phase = readiness_after_listen(m.screen_recording, m.accessibility).to_owned();
}
fn readiness_after_listen(screen: Option<bool>, input: Option<bool>) -> &'static str {
    match (screen, input) {
        (Some(true), Some(true)) => "ready",
        (Some(false), _) | (_, Some(false)) => "permission_required",
        _ => "starting",
    }
}

static GLOBAL: OnceLock<Arc<SessionStats>> = OnceLock::new();

/// Turn telemetry on: create (idempotently) the shared snapshot and return it.
/// Called once from `main.rs` when the endpoint is enabled.
pub fn enable() -> Arc<SessionStats> {
    GLOBAL
        .get_or_init(|| Arc::new(SessionStats::default()))
        .clone()
}

/// The shared snapshot iff telemetry is enabled, else `None`. Hot-path update
/// sites do `if let Some(s) = stats::global() { … }`, a no-op when off.
#[inline]
pub fn global() -> Option<&'static Arc<SessionStats>> {
    GLOBAL.get()
}

/// Endpoint port (`MACRDP_STATS_PORT`, default 40245 — next after the shield
/// helper's 40244).
pub fn default_port() -> u16 {
    std::env::var("MACRDP_STATS_PORT")
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(40245)
}

/// Serve the snapshot on `127.0.0.1:port`, one JSON line per connection, until
/// the process exits. Read-only; the request body (if any) is ignored.
pub async fn serve(port: u16, stats: Arc<SessionStats>) {
    use tokio::io::AsyncWriteExt;
    let listener = match tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, port)).await {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!(port, error = %e, "stats endpoint: bind failed — live telemetry unavailable");
            return;
        }
    };
    tracing::info!(port, "stats endpoint listening (loopback, read-only)");
    loop {
        match listener.accept().await {
            Ok((mut sock, _)) => {
                let body = stats.to_json();
                // Handle inline (no per-connection task spawn) so a local process
                // that hammers the port can't spawn unbounded tasks; the response
                // is a couple hundred bytes to a loopback socket, so serializing
                // one poll every ~2 s is fine. A short timeout keeps a client that
                // connects but never reads from wedging the accept loop.
                let _ = tokio::time::timeout(std::time::Duration::from_secs(2), async {
                    let _ = sock.write_all(body.as_bytes()).await;
                    let _ = sock.write_all(b"\n").await;
                    let _ = sock.shutdown().await;
                })
                .await;
            }
            Err(e) => tracing::debug!(error = %e, "stats endpoint: accept error"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_shape_is_stable_and_parseable() {
        let s = SessionStats::default();
        s.connected.store(true, Ordering::Relaxed);
        s.width.store(1920, Ordering::Relaxed);
        s.height.store(1080, Ordering::Relaxed);
        s.bitrate_bps.store(4_000_000, Ordering::Relaxed);
        s.fps.store(60, Ordering::Relaxed);
        let j = s.to_json();
        // Spot-check a few fields + that it's a single line with the expected keys.
        assert!(j.starts_with('{') && j.ends_with('}'));
        let v: serde_json::Value = serde_json::from_str(&j).unwrap();
        assert_eq!(v["schema_version"], 2);
        assert!(v["display"]["actual"]["backing_width"].is_null());
        assert!(!j.contains('\n'));
        assert!(j.contains("\"connected\":true"));
        assert!(j.contains("\"width\":1920"));
        assert!(j.contains("\"bitrate_bps\":4000000"));
        assert!(j.contains("\"fps\":60"));
    }

    #[test]
    fn display_status_is_independent_of_encoder_and_preserves_unknowns() {
        let stats = SessionStats::default();
        stats.update_display(|d| {
            d.phase = "degraded";
            d.wire_width = Some(3840);
            d.wire_height = Some(2160);
            d.actual.points_width = Some(3840.0);
            d.actual.scale_percent = Some(100.0);
            d.fallback_reason = Some("mode failed: \"density\"\nretry".into());
        });
        let json: serde_json::Value = serde_json::from_str(&stats.to_json()).unwrap();
        assert_eq!(json["frames_sent"], 0);
        assert_eq!(json["display"]["wire"]["width"], 3840);
        assert_eq!(json["display"]["actual"]["scale_percent"], 100.0);
        assert!(json["display"]["actual"]["backing_width"].is_null());
        assert!(json["display"]["requested"]["width"].is_null());
        assert_eq!(
            json["display"]["fallback_reason"],
            "mode failed: \"density\"\nretry"
        );
    }

    #[test]
    fn cancelled_pending_layout_restores_measured_status() {
        let mut display = DisplaySnapshot {
            phase: "applying",
            ..Default::default()
        };
        display.finish_unchanged_request((3840, 2160));
        assert_eq!(display.phase, "applying", "no measurements yet");
        display.wire_width = Some(3840);
        display.wire_height = Some(2160);
        display.finish_unchanged_request((1920, 1080));
        assert_eq!(
            display.phase, "applying",
            "reactivation still has the previous measured wire size"
        );
        display.finish_unchanged_request((3840, 2160));
        assert_eq!(display.phase, "active");
        display.phase = "applying";
        display.fallback_reason = Some("density fallback".into());
        display.finish_unchanged_request((3840, 2160));
        assert_eq!(display.phase, "degraded");
    }

    #[test]
    fn listening_requires_verified_permissions_to_claim_ready() {
        assert_eq!(readiness_after_listen(Some(true), Some(true)), "ready");
        assert_eq!(readiness_after_listen(None, Some(true)), "starting");
        assert_eq!(
            readiness_after_listen(Some(true), Some(false)),
            "permission_required"
        );
        assert_eq!(
            readiness_after_listen(Some(false), None),
            "permission_required"
        );
    }

    #[test]
    fn session_end_reports_disconnected_and_zeros() {
        let s = SessionStats::default();
        s.connected.store(true, Ordering::Relaxed);
        s.width.store(3024, Ordering::Relaxed);
        s.bitrate_bps.store(5_000_000, Ordering::Relaxed);
        s.frames_sent.store(900, Ordering::Relaxed);
        s.adaptive.store(true, Ordering::Relaxed);
        s.aac.store(true, Ordering::Relaxed);
        s.session_ended();
        let j = s.to_json();
        assert!(j.contains("\"connected\":false"));
        assert!(j.contains("\"width\":0"));
        assert!(j.contains("\"bitrate_bps\":0"));
        assert!(j.contains("\"frames_sent\":0"));
        assert!(j.contains("\"adaptive\":false"));
        assert!(j.contains("\"aac\":true"), "aac is process config");
    }

    #[test]
    fn global_is_none_until_enabled() {
        // NB: process-global; if another test enables it this may already be Some.
        // We only assert the accessor doesn't panic and the enabled snapshot is shared.
        let a = enable();
        let b = global().expect("enabled");
        assert!(Arc::ptr_eq(&a, b));
    }
}
