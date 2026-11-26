//! Filter configuration, key/URL parsing, and user-facing status copy.

use url::Url;

use crate::audio::DEFAULT_NOISE_FLOOR;

/// Optional compile-time default overlay server URL (e.g. set via
/// `GSTRING_OVERLAY_URL=https://... cargo build --release`).
pub const COMPILE_TIME_OVERLAY_URL: Option<&str> = option_env!("GSTRING_OVERLAY_URL");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureMode {
    Always,
    StreamingOnly,
    StreamingOrRecording,
}

impl CaptureMode {
    pub const KEY_ALWAYS: &'static str = "always";
    pub const KEY_STREAMING: &'static str = "streaming";
    pub const KEY_STREAMING_OR_RECORDING: &'static str = "streaming_or_recording";

    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        match value.trim() {
            Self::KEY_ALWAYS => Self::Always,
            Self::KEY_STREAMING_OR_RECORDING => Self::StreamingOrRecording,
            _ => Self::StreamingOnly,
        }
    }

    #[must_use]
    pub fn is_active(self, streaming: bool, recording: bool) -> bool {
        match self {
            Self::Always => true,
            Self::StreamingOnly => streaming,
            Self::StreamingOrRecording => streaming || recording,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoiseGatePreset {
    Low,
    Normal,
    High,
}

impl NoiseGatePreset {
    pub const KEY_LOW: &'static str = "low";
    pub const KEY_NORMAL: &'static str = "normal";
    pub const KEY_HIGH: &'static str = "high";

    #[must_use]
    pub fn from_setting(value: &str) -> Self {
        match value.trim() {
            Self::KEY_LOW => Self::Low,
            Self::KEY_HIGH => Self::High,
            _ => Self::Normal,
        }
    }

    #[must_use]
    pub fn rms_threshold(self) -> f32 {
        match self {
            Self::Low => 0.004,
            Self::Normal => DEFAULT_NOISE_FLOOR,
            Self::High => 0.016,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    /// Normalized `ws://.../overlay/ws` or `wss://.../overlay/ws` endpoint.
    pub ws_url: String,
    /// Bearer token (`Role::PluginCapture` or `Role::Capture`).
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigError {
    MissingKey,
    MissingServerUrl,
    InvalidServerUrl,
    InsecureServerUrl,
    ServerHostMismatch,
    InvalidToken,
}

impl ConfigError {
    #[must_use]
    pub fn message(&self) -> &'static str {
        match self {
            Self::MissingKey => {
                "Not configured — paste your OBS plugin key from the GString dashboard."
            }
            Self::MissingServerUrl => {
                "Server URL missing — paste a full OBS plugin key or enter the GString overlay server URL."
            }
            Self::InvalidServerUrl => "Server URL is invalid — expected an https:// or wss:// URL.",
            Self::InsecureServerUrl => {
                "Server URL must use https:// or wss:// (plain ws:// is only allowed for localhost)."
            }
            Self::ServerHostMismatch => {
                "Server URL override does not match the host embedded in the OBS plugin key. Clear the override or use a matching key."
            }
            Self::InvalidToken => {
                "Capture key looks incomplete — copy a fresh OBS plugin key from the dashboard."
            }
        }
    }
}

/// Parse either a bundled key (`gstring:<base_url>#<token>`) or a bare token
/// (`v1.<payload>.<sig>`) combined with an optional Server URL override.
pub fn parse_connection(
    capture_key: &str,
    server_url_override: &str,
) -> Result<ConnectionConfig, ConfigError> {
    let raw_key = capture_key.trim();
    if raw_key.is_empty() {
        return Err(ConfigError::MissingKey);
    }

    let (embedded_url, token) = if let Some(rest) = raw_key.strip_prefix("gstring:") {
        let Some((url_part, tok_part)) = rest.split_once('#') else {
            return Err(ConfigError::InvalidToken);
        };
        (Some(url_part.trim()), tok_part.trim())
    } else {
        (None, raw_key)
    };

    if token.is_empty() || token.chars().any(char::is_whitespace) {
        return Err(ConfigError::InvalidToken);
    }

    let override_url = Some(server_url_override.trim()).filter(|s| !s.is_empty());
    let embedded = embedded_url.filter(|s| !s.is_empty());

    // Refuse redirecting a bundled key's Bearer token to a different remote
    // host (guards against tampered scene-collection exports overriding the URL).
    if let (Some(over), Some(emb)) = (override_url, embedded) {
        let over_norm =
            Url::parse(&normalize_ws_url(over)?).map_err(|_| ConfigError::InvalidServerUrl)?;
        let emb_norm =
            Url::parse(&normalize_ws_url(emb)?).map_err(|_| ConfigError::InvalidServerUrl)?;
        if over_norm.host_str() != emb_norm.host_str() {
            return Err(ConfigError::ServerHostMismatch);
        }
    }

    let base_url = [
        override_url,
        embedded,
        COMPILE_TIME_OVERLAY_URL
            .map(str::trim)
            .filter(|s| !s.is_empty()),
    ]
    .into_iter()
    .flatten()
    .next()
    .ok_or(ConfigError::MissingServerUrl)?;

    let ws_url = normalize_ws_url(base_url)?;
    Ok(ConnectionConfig {
        ws_url,
        token: token.to_owned(),
    })
}

fn is_loopback_host(url: &Url) -> bool {
    match url.host() {
        Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

/// Normalize a user-provided overlay base URL or full WS URL into
/// `wss://<host>/overlay/ws` (or `ws://` for loopback only).
pub fn normalize_ws_url(input: &str) -> Result<String, ConfigError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ConfigError::MissingServerUrl);
    }
    let mut url = Url::parse(trimmed).map_err(|_| ConfigError::InvalidServerUrl)?;
    let plaintext = match url.scheme() {
        "https" => {
            url.set_scheme("wss")
                .map_err(|()| ConfigError::InvalidServerUrl)?;
            false
        }
        "wss" => false,
        "http" => {
            url.set_scheme("ws")
                .map_err(|()| ConfigError::InvalidServerUrl)?;
            true
        }
        "ws" => true,
        _ => return Err(ConfigError::InvalidServerUrl),
    };
    if url.host_str().is_none() {
        return Err(ConfigError::InvalidServerUrl);
    }
    if plaintext && !is_loopback_host(&url) {
        return Err(ConfigError::InsecureServerUrl);
    }
    url.set_query(None);
    url.set_fragment(None);
    if !url.path().ends_with("/overlay/ws") {
        let base_path = url.path().trim_end_matches('/');
        let next_path = if base_path.is_empty() {
            "/overlay/ws".to_owned()
        } else {
            format!("{base_path}/overlay/ws")
        };
        url.set_path(&next_path);
    }
    Ok(url.to_string())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterSettings {
    pub capture_key: String,
    pub server_url: String,
    pub capture_mode: CaptureMode,
    pub respect_obs_mute: bool,
    pub noise_gate: NoiseGatePreset,
}

impl Default for FilterSettings {
    fn default() -> Self {
        Self {
            capture_key: String::new(),
            server_url: COMPILE_TIME_OVERLAY_URL.unwrap_or("").to_owned(),
            capture_mode: CaptureMode::StreamingOnly,
            respect_obs_mute: true,
            noise_gate: NoiseGatePreset::Normal,
        }
    }
}

const MAX_SERVER_MESSAGE_CHARS: usize = 200;

/// Sanitize and truncate an untrusted server-supplied string before showing it
/// in OBS Filter Properties or writing it to the OBS log.
#[must_use]
pub fn sanitize_server_message(raw: &str) -> String {
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(MAX_SERVER_MESSAGE_CHARS)
        .collect();
    cleaned.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Human-readable description for fatal server verdicts.
#[must_use]
pub fn fatal_verdict_message(code: &str, fallback: &str) -> String {
    let safe_fallback = sanitize_server_message(fallback);
    match code {
        "superseded" => {
            "Replaced by another capture source (close the web capture tab or extra filter, then click Reconnect)."
                .to_owned()
        }
        "quota_exceeded" => "This month's caption minutes are used up.".to_owned(),
        "unauthorized" => {
            "This plugin key was reset or expired. Copy a fresh OBS plugin key from the dashboard."
                .to_owned()
        }
        "disabled" => {
            "Captions are turned off in the dashboard. Turn them on, then click Reconnect."
                .to_owned()
        }
        _ if !safe_fallback.is_empty() => format!("Server stopped capture: {safe_fallback}"),
        _ => "The captions server stopped the session. Click Reconnect to try again.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bundled_plugin_key() {
        let conn = parse_connection("gstring:https://overlay.gstringbot.com#v1.abc.def", "")
            .expect("bundled key parses");
        assert_eq!(conn.ws_url, "wss://overlay.gstringbot.com/overlay/ws");
        assert_eq!(conn.token, "v1.abc.def");
    }

    #[test]
    fn server_url_override_rejects_different_host_for_bundled_key() {
        assert_eq!(
            parse_connection(
                "gstring:https://overlay.gstringbot.com#v1.abc.def",
                "https://evil.example.test",
            ),
            Err(ConfigError::ServerHostMismatch)
        );
        // Same host with explicit port/path is permitted.
        let conn = parse_connection(
            "gstring:https://overlay.gstringbot.com#v1.abc.def",
            "wss://overlay.gstringbot.com:443/overlay/ws",
        )
        .expect("matching host override parses");
        assert_eq!(conn.ws_url, "wss://overlay.gstringbot.com/overlay/ws");
    }

    #[test]
    fn rejects_plaintext_ws_for_non_loopback_hosts() {
        assert_eq!(
            parse_connection("v1.payload.sig", "http://overlay.gstringbot.com"),
            Err(ConfigError::InsecureServerUrl)
        );
        assert_eq!(
            parse_connection("gstring:ws://overlay.gstringbot.com#v1.abc.def", ""),
            Err(ConfigError::InsecureServerUrl)
        );
        // Loopback remains allowed for local development.
        let local = parse_connection("v1.payload.sig", "http://127.0.0.1:8080/")
            .expect("loopback http allowed");
        assert_eq!(local.ws_url, "ws://127.0.0.1:8080/overlay/ws");
        let localhost = parse_connection("v1.payload.sig", "ws://localhost:8080")
            .expect("localhost ws allowed");
        assert_eq!(localhost.ws_url, "ws://localhost:8080/overlay/ws");
    }

    #[test]
    fn bare_token_uses_explicit_server_url() {
        let conn = parse_connection("v1.payload.sig", "wss://bot.example.test/overlay/ws")
            .expect("bare token parses");
        assert_eq!(conn.ws_url, "wss://bot.example.test/overlay/ws");
        assert_eq!(conn.token, "v1.payload.sig");
    }

    #[test]
    fn rejects_empty_key_and_invalid_urls() {
        assert_eq!(
            parse_connection("  ", "https://example.test"),
            Err(ConfigError::MissingKey)
        );
        assert_eq!(
            parse_connection("v1.a.b", "ftp://example.test"),
            Err(ConfigError::InvalidServerUrl)
        );
        assert_eq!(
            parse_connection("gstring:https://example.test", ""),
            Err(ConfigError::InvalidToken)
        );
    }

    #[test]
    fn sanitizes_and_truncates_server_messages() {
        let long = format!("line1\n\r\t{}", "a".repeat(300));
        let clean = sanitize_server_message(&long);
        assert!(!clean.contains('\n'));
        assert!(!clean.contains('\r'));
        assert!(clean.len() <= MAX_SERVER_MESSAGE_CHARS);
    }

    #[test]
    fn capture_mode_gates_correctly() {
        assert!(CaptureMode::Always.is_active(false, false));
        assert!(!CaptureMode::StreamingOnly.is_active(false, true));
        assert!(CaptureMode::StreamingOnly.is_active(true, false));
        assert!(CaptureMode::StreamingOrRecording.is_active(false, true));
        assert!(!CaptureMode::StreamingOrRecording.is_active(false, false));
    }
}
