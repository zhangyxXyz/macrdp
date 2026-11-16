//! Side-effect-free config parsing and validation shared by startup and the GUI.
//! Parsing never evaluates shell expressions or reads referenced credential files.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{anyhow, Context, Result};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;
pub const BUILD_COMMIT: &str = env!("MACRDP_BUILD_COMMIT");

#[derive(Debug)]
pub struct Document {
    pub values: BTreeMap<String, String>,
    pub extra_flags: Vec<String>,
    pub warnings: Vec<String>,
    pub errors: Vec<String>,
    pub revision: String,
}

impl Document {
    pub fn read(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config file {}", path.display()))?;
        Ok(Self::parse(&text))
    }

    pub fn parse(text: &str) -> Self {
        let mut doc = Self {
            values: BTreeMap::new(),
            extra_flags: vec![],
            warnings: vec![],
            errors: vec![],
            revision: String::new(),
        };
        for (index, raw) in text.lines().enumerate() {
            let line_number = index + 1;
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let line = line
                .strip_prefix("export")
                .filter(|rest| rest.starts_with(char::is_whitespace))
                .map(str::trim_start)
                .unwrap_or(line);
            let Some((key, raw_value)) = line.split_once('=') else {
                doc.warnings
                    .push(format!("line {line_number}: ignored line without '='"));
                continue;
            };
            let key = key.trim();
            if key.is_empty() || !key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                doc.errors
                    .push(format!("line {line_number}: invalid configuration key"));
                continue;
            }
            let raw_value = raw_value.trim();
            let value =
                if let Some(quote) = raw_value.chars().next().filter(|c| *c == '\'' || *c == '"') {
                    if raw_value.len() < 2 || !raw_value.ends_with(quote) {
                        doc.errors.push(format!(
                            "line {line_number}: {key} has unmatched surrounding quotes"
                        ));
                        continue;
                    }
                    &raw_value[1..raw_value.len() - 1]
                } else {
                    raw_value
                };
            if let Some(previous) = doc.values.get(key) {
                if previous != value {
                    doc.errors.push(format!("line {line_number}: {key} is repeated with conflicting values; keep one definition"));
                } else {
                    doc.warnings.push(format!(
                        "line {line_number}: duplicate {key} has the same value"
                    ));
                }
            }
            if !SUPPORTED_KEYS.contains(&key) {
                doc.warnings.push(format!(
                    "line {line_number}: unknown key {key} is preserved but ignored by this build"
                ));
            }
            doc.values.insert(key.to_owned(), value.to_owned());
        }
        match split_flags(
            doc.values
                .get("EXTRA_FLAGS")
                .map(String::as_str)
                .unwrap_or_default(),
        ) {
            Ok(flags) => doc.extra_flags = flags,
            Err(error) => doc.errors.push(error.to_owned()),
        }
        doc.validate();
        // A fingerprint, not authentication. Length framing makes the canonical
        // sorted representation unambiguous; comments/order/quote style don't
        // change it. Credentials are never included in any JSON field as text.
        let mut digest = Sha256::new();
        digest.update(b"macrdp-config-v1\0");
        for (key, value) in &doc.values {
            digest.update((key.len() as u64).to_be_bytes());
            digest.update(key);
            digest.update((value.len() as u64).to_be_bytes());
            digest.update(value);
        }
        doc.revision = format!("sha256:{:x}", digest.finalize());
        doc
    }

    fn validate(&mut self) {
        // These switches are translated by `value == "1"`; rejecting typos
        // avoids a successful check that silently disables a requested feature.
        for key in BOOL_KEYS {
            if self
                .values
                .get(*key)
                .is_some_and(|value| !matches!(value.as_str(), "0" | "1"))
            {
                self.errors.push(format!("{key} must be 0 or 1"));
            }
        }
        if let Some(value) = self.values.get("MODIFIER_LAYOUT") {
            if !matches!(
                value.as_str(),
                "standard" | "swap-left-win-alt" | "swap-win-alt"
            ) {
                self.errors.push(
                    "MODIFIER_LAYOUT must be standard, swap-left-win-alt, or swap-win-alt".into(),
                );
            }
        }
        for key in ["VD_WIDTH", "VD_HEIGHT"] {
            if let Some(value) = self.values.get(key) {
                if !value
                    .parse::<u16>()
                    .is_ok_and(|v| (200..=8192).contains(&v))
                {
                    self.errors
                        .push(format!("{key} must be an integer in 200..=8192 pixels"));
                }
            }
        }
        if let Some(value) = self.values.get("VD_HIDPI") {
            if !matches!(value.to_ascii_lowercase().as_str(), "0" | "1" | "auto") {
                self.errors.push("VD_HIDPI must be 0, 1, or auto".into());
            }
        }
        if let Some(value) = self.values.get("RESOLUTION_POLICY") {
            if !matches!(value.as_str(), "client" | "fixed") {
                self.errors
                    .push("RESOLUTION_POLICY must be client or fixed".into());
            }
        }
        if let Some(value) = self.values.get("LIVE_RESIZE") {
            if !matches!(value.as_str(), "0" | "1") {
                self.errors.push("LIVE_RESIZE must be 0 or 1".into());
            }
        }
        if let Some(value) = self.values.get("PRIMARY_MODE") {
            if !matches!(value.as_str(), "none" | "detach" | "capture" | "shield") {
                self.errors
                    .push("PRIMARY_MODE must be none, detach, capture, or shield".into());
            }
        }
        if let Some(value) = self.values.get("STATS_PORT") {
            if !value.is_empty() && !value.parse::<u16>().is_ok_and(|port| port != 0) {
                self.errors
                    .push("STATS_PORT must be an integer in 1..=65535".into());
            }
        }
        let has_flag = |flag: &str| {
            self.extra_flags
                .iter()
                .any(|t| t == flag || t.strip_prefix(flag).is_some_and(|s| s.starts_with('=')))
        };
        if self
            .values
            .get("RESOLUTION_POLICY")
            .is_some_and(|v| v == "client")
            && has_flag("--no-client-resolution")
        {
            self.errors.push("RESOLUTION_POLICY=client conflicts with EXTRA_FLAGS --no-client-resolution; remove the legacy flag or select fixed".into());
        }
        // A dedicated display field and an escape-hatch flag must never silently
        // disagree. Even equal duplicates are rejected: clap rejects them too.
        for (key, flag) in [
            ("CAPTURE_NV12", "--capture-nv12"),
            ("MODIFIER_LAYOUT", "--modifier-layout"),
            ("MAP_CTRL_TO_CMD", "--map-ctrl-to-cmd"),
            ("RESOLUTION_POLICY", "--resolution-policy"),
            ("LIVE_RESIZE", "--live-resize"),
            ("VD_HIDPI", "--virtual-display-hidpi"),
            ("MAX_CLIENT_SIZE", "--max-client-size"),
        ] {
            if self.values.get(key).is_some_and(|v| !v.is_empty()) && has_flag(flag) {
                self.errors.push(format!(
                    "{key} conflicts with EXTRA_FLAGS {flag}; keep the dedicated setting only"
                ));
            }
        }
        if self.values.get("VIRTUAL_DISPLAY").is_some_and(|v| v == "1") {
            for flag in ["--virtual-display", "--width", "--height"] {
                if has_flag(flag) {
                    self.errors.push(format!("VIRTUAL_DISPLAY/VD_WIDTH/VD_HEIGHT conflict with EXTRA_FLAGS {flag}; use the dedicated settings"));
                }
            }
        }
        for flag in [
            "--config",
            "--check-config",
            "--capabilities",
            "--help",
            "--version",
            "--usb-spike",
        ] {
            if has_flag(flag) {
                self.errors.push(format!(
                    "EXTRA_FLAGS {flag} is an operational command, not a server setting"
                ));
            }
        }
        if self.values.get("BITRATE").is_some_and(|v| !v.is_empty()) && has_flag("--bitrate") {
            self.warnings.push("BITRATE takes precedence over EXTRA_FLAGS --bitrate (legacy compatibility); remove the duplicate flag".into());
        }
        if !self.values.contains_key("RESOLUTION_POLICY") && has_flag("--no-client-resolution") {
            self.warnings.push("EXTRA_FLAGS --no-client-resolution selects fixed resolution; migrate to RESOLUTION_POLICY=fixed".into());
        }
    }

    pub fn ensure_valid(&self) -> Result<()> {
        if self.errors.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(self.errors.join("; ")))
        }
    }

    /// Called only once real startup has begun; never from either inspection CLI.
    pub fn apply_environment(&self) {
        for (key, env) in ENV_BRIDGE {
            if let Some(value) = self.values.get(*key).filter(|value| !value.is_empty()) {
                std::env::set_var(env, value);
            }
        }
    }

    /// Clap may quote an invalid token. Redact all credential-bearing tokens
    /// before returning diagnostic text to the GUI or a log.
    pub fn redact_error(&self, error: &str) -> String {
        let mut result = error.to_owned();
        let mut secret_next = false;
        for token in &self.extra_flags {
            if secret_next {
                if !token.is_empty() {
                    result = result.replace(token, "[redacted]");
                }
                secret_next = false;
            } else if token == "--password" {
                secret_next = true;
            } else if let Some(value) = token.strip_prefix("--password=") {
                if !value.is_empty() {
                    result = result.replace(value, "[redacted]");
                }
            }
        }
        result
    }
}

/// Shell-like token grouping, with no substitutions, commands, or expansion.
/// Matches historical whitespace splitting unless quotes/escapes are present.
fn split_flags(text: &str) -> Result<Vec<String>, &'static str> {
    let mut result = vec![];
    let mut token = String::new();
    let mut quote = None;
    let mut escape = false;
    let mut started = false;
    for ch in text.chars() {
        if escape {
            token.push(ch);
            escape = false;
            started = true;
            continue;
        }
        if ch == '\\' && quote != Some('\'') {
            escape = true;
            started = true;
            continue;
        }
        if let Some(q) = quote {
            if ch == q {
                quote = None;
            } else {
                token.push(ch);
            }
        } else if ch == '\'' || ch == '"' {
            quote = Some(ch);
            started = true;
        } else if ch.is_whitespace() {
            if started {
                result.push(std::mem::take(&mut token));
                started = false;
            }
        } else {
            token.push(ch);
            started = true;
        }
    }
    if quote.is_some() || escape {
        return Err("EXTRA_FLAGS has an unmatched quote or trailing escape");
    }
    if started {
        result.push(token);
    }
    Ok(result)
}

pub fn capabilities() -> serde_json::Value {
    serde_json::json!({
        "schema_version": SCHEMA_VERSION, "build_commit": BUILD_COMMIT,
        "version": env!("CARGO_PKG_VERSION"),
        "features": ["config_validation", "config_revision", "resolution_policy", "live_resize", "virtual_display_hidpi", "display_state_v2", "modifier_layout"],
        "supported_config_keys": SUPPORTED_KEYS,
    })
}

// Generated from the existing launch/config bridge; kept explicit so feature
// probing is independent of version strings and doesn't run any server code.
pub const SUPPORTED_KEYS: &[&str] = &[
    "ADAPTIVE_BITRATE",
    "ALT_BACKTICK_SWITCH",
    "ALT_Q_QUIT",
    "ALT_SPACE_SPOTLIGHT",
    "ALT_TAB_SWITCH",
    "APP_SWITCHER_HUD",
    "AUDIT_FILE",
    "AUDIT_LOG",
    "AUTO_RECONNECT",
    "AUTO_UNLOCK",
    "BIND",
    "BITRATE",
    "BLANK_RECOVERY",
    "BLANK_RECOVERY_ARM_MS",
    "BLANK_RECOVERY_ESTABLISHED_MAX_WAIT_MS",
    "BLANK_RECOVERY_ESTABLISHED_MIN_QOE",
    "BLANK_RECOVERY_ESTABLISHED_REPORTS",
    "BLANK_RECOVERY_ESTABLISHED_WALL_REPORTS",
    "BLANK_RECOVERY_HEAL_CONFIRM_MS",
    "BLANK_RECOVERY_MAX_ATTEMPTS",
    "BLANK_RECOVERY_MAX_CONSECUTIVE_DROPS",
    "BLANK_RECOVERY_MAX_RTT_MS",
    "BLANK_RECOVERY_MAX_WAIT_MS",
    "BLANK_RECOVERY_MIN_QOE",
    "BLANK_RECOVERY_MIN_RENDER_REPORTS",
    "BLANK_RECOVERY_MIN_WALL_REPORTS",
    "BLANK_RECOVERY_POST_ATTEMPT_WINDOW_MS",
    "BLANK_RECOVERY_POST_ATTEMPT_ZEROS",
    "BLANK_RECOVERY_REACTIVATE",
    "BLANK_RECOVERY_RETRY_MS",
    "CAMERA_DUMP",
    "CAPTURE_NV12",
    "CAPTURE_PRIMARY",
    "CONN_GUARD",
    "DETACH_RESTART_ON_STUCK",
    "ENABLE_AAC",
    "AUDIO_REMOTE_ONLY",
    "ENABLE_CAMERA_REDIRECTION",
    "ENABLE_DRIVE_REDIRECTION",
    "ENABLE_H264",
    "ENABLE_LOSSY_AUDIO",
    "ENABLE_MICROPHONE_REDIRECTION",
    "ENABLE_SMARTCARD_REDIRECTION",
    "ENABLE_UDP_MULTITRANSPORT",
    "ENABLE_USB_REDIRECTION",
    "EXTRA_FLAGS",
    "GUARD_COOLDOWN_BASE_SECS",
    "GUARD_COOLDOWN_MAX_SECS",
    "GUARD_FAILFAST_SECS",
    "GUARD_FAIL_THRESHOLD",
    "GUARD_RL_MAX",
    "GUARD_RL_WINDOW_SECS",
    "HEALTHCHECK_FAILURES",
    "HEALTHCHECK_INTERVAL_SECS",
    "HEALTHCHECK_TIMEOUT_SECS",
    "HEALTH_CHECK",
    "HIDPI",
    "KEYBOARD_LAYOUT",
    "LIVE_RESIZE",
    "LOCK_ON_DISCONNECT",
    "LOCK_ON_DISCONNECT_DELAY_MS",
    "LOG_DIR",
    "MAP_CTRL_TO_CMD",
    "MAX_CLIENT_SIZE",
    "MIC_DUMP",
    "MODIFIER_LAYOUT",
    "NO_REMAP_APPS",
    "PASSWORD_FILE",
    "PRIMARY_MODE",
    "RESOLUTION_POLICY",
    "RESTORE_WINDOWS_ON_DISCONNECT",
    "REVERSE_SCROLL",
    "RICH_CLIPBOARD",
    "STATS_ENDPOINT",
    "STATS_PORT",
    "TLS_CERT",
    "TLS_KEY",
    "UDP_MIGRATE_EGFX",
    "UNMINIMIZE",
    "USB_PREFETCH_DEPTH",
    "USB_STREAM_STALL_MS",
    "USERNAME",
    "USE_KEYCHAIN",
    "VD_HEIGHT",
    "VD_HIDPI",
    "VD_WIDTH",
    "VIRTUAL_DISPLAY",
];
const BOOL_KEYS: &[&str] = &[
    "ADAPTIVE_BITRATE",
    "ALT_BACKTICK_SWITCH",
    "ALT_Q_QUIT",
    "ALT_SPACE_SPOTLIGHT",
    "ALT_TAB_SWITCH",
    "APP_SWITCHER_HUD",
    "AUTO_UNLOCK",
    "CAPTURE_NV12",
    "CAPTURE_PRIMARY",
    "ENABLE_AAC",
    "AUDIO_REMOTE_ONLY",
    "ENABLE_CAMERA_REDIRECTION",
    "ENABLE_DRIVE_REDIRECTION",
    "ENABLE_H264",
    "ENABLE_LOSSY_AUDIO",
    "ENABLE_MICROPHONE_REDIRECTION",
    "ENABLE_SMARTCARD_REDIRECTION",
    "ENABLE_UDP_MULTITRANSPORT",
    "ENABLE_USB_REDIRECTION",
    "HIDPI",
    "LOCK_ON_DISCONNECT",
    "MAP_CTRL_TO_CMD",
    "RESTORE_WINDOWS_ON_DISCONNECT",
    "REVERSE_SCROLL",
    "RICH_CLIPBOARD",
    "STATS_ENDPOINT",
    "UDP_MIGRATE_EGFX",
    "UNMINIMIZE",
    "USE_KEYCHAIN",
    "VIRTUAL_DISPLAY",
];

const ENV_BRIDGE: &[(&str, &str)] = &[
    ("CONN_GUARD", "MACRDP_CONN_GUARD"),
    ("AUDIT_LOG", "MACRDP_AUDIT_LOG"),
    ("GUARD_RL_MAX", "MACRDP_GUARD_RL_MAX"),
    ("GUARD_RL_WINDOW_SECS", "MACRDP_GUARD_RL_WINDOW_SECS"),
    ("GUARD_FAIL_THRESHOLD", "MACRDP_GUARD_FAIL_THRESHOLD"),
    ("GUARD_FAILFAST_SECS", "MACRDP_GUARD_FAILFAST_SECS"),
    (
        "GUARD_COOLDOWN_BASE_SECS",
        "MACRDP_GUARD_COOLDOWN_BASE_SECS",
    ),
    ("GUARD_COOLDOWN_MAX_SECS", "MACRDP_GUARD_COOLDOWN_MAX_SECS"),
    // Health-check watchdog (env-driven like the auth guard; on by default
    // when headless). See src/health.rs.
    ("HEALTH_CHECK", "MACRDP_HEALTHCHECK"),
    (
        "HEALTHCHECK_INTERVAL_SECS",
        "MACRDP_HEALTHCHECK_INTERVAL_SECS",
    ),
    (
        "HEALTHCHECK_TIMEOUT_SECS",
        "MACRDP_HEALTHCHECK_TIMEOUT_SECS",
    ),
    ("HEALTHCHECK_FAILURES", "MACRDP_HEALTHCHECK_FAILURES"),
    // Blank recovery (the mstsc reconnect-blank auto-heal, needs
    // --enable-h264) + the auto-reconnect cookie. Env-driven like the
    // guard (read in h264.rs per connection / main.rs at server build,
    // both after this bridge runs). BLANK_RECOVERY=0 is the ask that
    // motivated bridging: disabling the detector for an overlay-link
    // profile used to need a plist EnvironmentVariables edit.
    ("BLANK_RECOVERY", "MACRDP_BLANK_RECOVERY"),
    (
        "BLANK_RECOVERY_REACTIVATE",
        "MACRDP_BLANK_RECOVERY_REACTIVATE",
    ),
    (
        "BLANK_RECOVERY_MAX_RTT_MS",
        "MACRDP_BLANK_RECOVERY_MAX_RTT_MS",
    ),
    ("BLANK_RECOVERY_MIN_QOE", "MACRDP_BLANK_RECOVERY_MIN_QOE"),
    (
        "BLANK_RECOVERY_MIN_RENDER_REPORTS",
        "MACRDP_BLANK_RECOVERY_MIN_RENDER_REPORTS",
    ),
    (
        "BLANK_RECOVERY_ESTABLISHED_REPORTS",
        "MACRDP_BLANK_RECOVERY_ESTABLISHED_REPORTS",
    ),
    (
        "BLANK_RECOVERY_ESTABLISHED_MIN_QOE",
        "MACRDP_BLANK_RECOVERY_ESTABLISHED_MIN_QOE",
    ),
    (
        "BLANK_RECOVERY_ESTABLISHED_MAX_WAIT_MS",
        "MACRDP_BLANK_RECOVERY_ESTABLISHED_MAX_WAIT_MS",
    ),
    (
        "BLANK_RECOVERY_ESTABLISHED_WALL_REPORTS",
        "MACRDP_BLANK_RECOVERY_ESTABLISHED_WALL_REPORTS",
    ),
    (
        "BLANK_RECOVERY_MAX_WAIT_MS",
        "MACRDP_BLANK_RECOVERY_MAX_WAIT_MS",
    ),
    (
        "BLANK_RECOVERY_MIN_WALL_REPORTS",
        "MACRDP_BLANK_RECOVERY_MIN_WALL_REPORTS",
    ),
    (
        "BLANK_RECOVERY_HEAL_CONFIRM_MS",
        "MACRDP_BLANK_RECOVERY_HEAL_CONFIRM_MS",
    ),
    (
        "BLANK_RECOVERY_POST_ATTEMPT_ZEROS",
        "MACRDP_BLANK_RECOVERY_POST_ATTEMPT_ZEROS",
    ),
    (
        "BLANK_RECOVERY_POST_ATTEMPT_WINDOW_MS",
        "MACRDP_BLANK_RECOVERY_POST_ATTEMPT_WINDOW_MS",
    ),
    ("BLANK_RECOVERY_ARM_MS", "MACRDP_BLANK_RECOVERY_ARM_MS"),
    ("BLANK_RECOVERY_RETRY_MS", "MACRDP_BLANK_RECOVERY_RETRY_MS"),
    (
        "BLANK_RECOVERY_MAX_ATTEMPTS",
        "MACRDP_BLANK_RECOVERY_MAX_ATTEMPTS",
    ),
    (
        "BLANK_RECOVERY_MAX_CONSECUTIVE_DROPS",
        "MACRDP_BLANK_RECOVERY_MAX_CONSECUTIVE_DROPS",
    ),
    ("AUTO_RECONNECT", "MACRDP_AUTO_RECONNECT"),
    // USB-redirection read-ahead tunables (--enable-usb-redirection). These
    // are env-only, read via getenv() in usb_spike.m; bridging them lets the
    // webcam knobs be set from config.env instead of the plist's
    // EnvironmentVariables (which a `kickstart -k` won't even pick up).
    ("USB_PREFETCH_DEPTH", "MACRDP_USB_PREFETCH_DEPTH"),
    ("USB_STREAM_STALL_MS", "MACRDP_USB_STREAM_STALL_MS"),
    // Camera-redirection decode diagnostics (--enable-camera-redirection): raw
    // H.264 + PNG frame dumps to $TMPDIR. Env-only like the USB knobs above;
    // bridged so it can be flipped in config.env when debugging the camera.
    ("CAMERA_DUMP", "MACRDP_CAMERA_DUMP"),
    ("MIC_DUMP", "MACRDP_MIC_DUMP"),
    // #168 stopgap: when --detach-primary can't re-enable the physical panel
    // on disconnect (macOS 26.x won't do it in-process), restart under
    // launchd to restore it. Env-read at the disconnect edge.
    ("DETACH_RESTART_ON_STUCK", "MACRDP_DETACH_RESTART_ON_STUCK"),
    // Extra safety-buffer delay (--lock-on-disconnect) on top of the
    // overlay watcher's REACTIVATION_GRACE before actually locking the
    // screen. Env-read at the disconnect edge via lock_on_disconnect_delay_ms().
    (
        "LOCK_ON_DISCONNECT_DELAY_MS",
        "MACRDP_LOCK_ON_DISCONNECT_DELAY_MS",
    ),
    // Loopback live-telemetry endpoint port (--stats-endpoint). Read via
    // getenv() in stats::default_port(); bridged like the USB/camera knobs
    // above so STATS_PORT can be set from config.env.
    ("STATS_PORT", "MACRDP_STATS_PORT"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsing_is_canonical_and_preserves_unknowns() {
        let first = Document::parse("export VD_WIDTH='3840'\nVD_HEIGHT=\"2160\"\nUNKNOWN=future\n");
        let second = Document::parse("# comment\nUNKNOWN=future\nVD_HEIGHT=2160\nVD_WIDTH=3840\n");
        assert_eq!(first.values, second.values);
        assert_eq!(first.revision, second.revision);
        assert!(first.errors.is_empty());
        assert!(first
            .warnings
            .iter()
            .any(|s| s.contains("unknown key UNKNOWN")));
    }

    #[test]
    fn rejects_ambiguous_quotes_duplicates_and_display_boundaries() {
        for text in [
            "VD_WIDTH='3840\"",
            "VD_WIDTH=3840\nVD_WIDTH=1920",
            "VD_WIDTH=199",
            "VD_HEIGHT=8193",
            "VD_HIDPI=true",
            "VD_HIDPI=2",
            "RESOLUTION_POLICY=auto",
            "LIVE_RESIZE=yes",
        ] {
            assert!(!Document::parse(text).errors.is_empty(), "{text}");
        }
        for text in [
            "VD_WIDTH=200\nVD_HEIGHT=8192",
            "VD_HIDPI=0",
            "VD_HIDPI=1",
            "VD_HIDPI=auto",
            "LIVE_RESIZE=0",
            "LIVE_RESIZE=1",
        ] {
            assert!(Document::parse(text).errors.is_empty(), "{text}");
        }
        let equal = Document::parse("VD_WIDTH=3840\nVD_WIDTH=3840");
        assert!(equal.errors.is_empty());
        assert!(!equal.warnings.is_empty());
    }

    #[test]
    fn reports_legacy_and_dedicated_display_conflicts() {
        let doc = Document::parse("RESOLUTION_POLICY=client\nEXTRA_FLAGS=--no-client-resolution");
        assert!(doc
            .errors
            .iter()
            .any(|s| s.contains("--no-client-resolution")));
        assert!(
            !Document::parse("VD_HIDPI=auto\nEXTRA_FLAGS=--virtual-display-hidpi")
                .errors
                .is_empty()
        );
        assert!(!Document::parse("EXTRA_FLAGS=--config nested.env")
            .errors
            .is_empty());
    }

    #[test]
    fn flag_tokenization_never_evaluates_shell() {
        assert_eq!(
            split_flags("--username 'two words' --password \"$(echo secret)\"").unwrap(),
            ["--username", "two words", "--password", "$(echo secret)"]
        );
        assert!(split_flags("--fps '30").is_err());
    }

    #[test]
    fn validation_does_not_apply_environment_or_read_credentials() {
        let before = std::env::var_os("MACRDP_STATS_PORT");
        let doc = Document::parse(
            "STATS_PORT=61234\nPASSWORD_FILE=/nonexistent/config-validation-secret\n",
        );
        doc.ensure_valid().unwrap();
        assert_eq!(std::env::var_os("MACRDP_STATS_PORT"), before);
    }

    #[test]
    fn diagnostics_redact_password_tokens() {
        let doc = Document::parse("EXTRA_FLAGS='--password secret-value --fps bad'");
        assert_eq!(
            doc.redact_error("invalid secret-value"),
            "invalid [redacted]"
        );
        assert!(!doc.revision.contains("secret-value"));
    }
}
