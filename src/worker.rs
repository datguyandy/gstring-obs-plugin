//! Background Tokio WebSocket worker for the OBS audio filter.
//!
//! Owns the network lifecycle completely off the OBS audio thread:
//! - connects to `/overlay/ws` with `Authorization: Bearer <token>` and
//!   `Sec-WebSocket-Protocol: gstring-overlay` (token never in the URL);
//! - sends `hello` with `sample_rate: 16000` and `resume` session id;
//! - streams 100 ms PCM binary frames from the bounded channel;
//! - syncs mute state immediately so OBS mute closes the Soniox stream;
//! - handles pings, reconnect backoff with jitter, and fatal server verdicts.

use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{self, Message};

use crate::audio::TARGET_SAMPLE_RATE;
use crate::config::{ConnectionConfig, fatal_verdict_message};

const BASE_BACKOFF_MS: u64 = 500;
const MAX_BACKOFF_MS: u64 = 15_000;
const PING_INTERVAL: Duration = Duration::from_secs(20);
const PING_TIMEOUT: Duration = Duration::from_secs(30);

/// Severity shown in OBS's read-only status info widget.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusTone {
    Normal,
    Warning,
    Error,
}

/// Live status reported back to OBS Filter Properties.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerStatus {
    pub tone: StatusTone,
    pub text: String,
}

impl WorkerStatus {
    pub fn normal(text: impl Into<String>) -> Self {
        Self {
            tone: StatusTone::Normal,
            text: text.into(),
        }
    }

    pub fn warning(text: impl Into<String>) -> Self {
        Self {
            tone: StatusTone::Warning,
            text: text.into(),
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        Self {
            tone: StatusTone::Error,
            text: text.into(),
        }
    }
}

/// Desired worker state driven by OBS filter settings + frontend events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredState {
    /// Parsed connection target, or an error string explaining what is missing.
    pub connection: Result<ConnectionConfig, String>,
    /// True when the filter is enabled and `CaptureMode` allows capturing.
    pub active: bool,
    /// Why capture is paused when `active == false`.
    pub inactive_reason: String,
    /// True when the parent OBS audio source is muted and respect-mute is on.
    pub muted: bool,
    /// Incremented by the "Reconnect now" button to clear a fatal latch.
    pub reconnect_nonce: u64,
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum CaptureIn<'a> {
    Hello {
        sample_rate: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        resume: Option<&'a str>,
    },
    Mute {
        muted: bool,
    },
    Stop,
    Ping,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerOut {
    Snapshot {
        session: Option<String>,
        status: String,
    },
    Status {
        session: Option<String>,
        status: String,
    },
    Error {
        code: String,
        message: String,
    },
    #[serde(other)]
    Other,
}

enum SessionOutcome {
    /// Desired state changed (e.g. stopped, key changed, or shutting down).
    ControlChanged,
    /// Transient disconnect — retry with backoff.
    Retry { status: WorkerStatus },
    /// Terminal refusal (401/403 or fatal server verdict) — stop retrying until
    /// settings change or user clicks Reconnect.
    Fatal { status: WorkerStatus },
}

pub type StatusCallback = Arc<dyn Fn(&WorkerStatus) + Send + Sync>;

pub struct StatusSink {
    current: Mutex<WorkerStatus>,
    on_change: StatusCallback,
}

impl StatusSink {
    pub fn new(initial: WorkerStatus, on_change: StatusCallback) -> Self {
        Self {
            current: Mutex::new(initial),
            on_change,
        }
    }

    pub fn set(&self, next: WorkerStatus) {
        let changed = match self.current.lock() {
            Ok(mut guard) => {
                if *guard == next {
                    false
                } else {
                    *guard = next.clone();
                    true
                }
            }
            Err(_) => false,
        };
        if changed {
            (self.on_change)(&next);
        }
    }

    pub fn get(&self) -> WorkerStatus {
        self.current
            .lock()
            .map(|g| g.clone())
            .unwrap_or_else(|_| WorkerStatus::normal("Starting…"))
    }
}

/// Run the background capture worker loop until `state_rx` is closed.
pub async fn run_worker(
    mut state_rx: watch::Receiver<DesiredState>,
    mut audio_rx: mpsc::Receiver<Bytes>,
    status: Arc<StatusSink>,
) {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let mut session_id: Option<String> = None;
    let mut attempt: u32 = 0;
    let mut fatal_latch: Option<(ConnectionConfig, u64)> = None;

    loop {
        let desired = state_rx.borrow_and_update().clone();

        let conn = match desired.connection {
            Ok(conn) => conn,
            Err(msg) => {
                session_id = None;
                attempt = 0;
                fatal_latch = None;
                drain_audio(&mut audio_rx);
                status.set(WorkerStatus::warning(msg));
                if state_rx.changed().await.is_err() {
                    break;
                }
                continue;
            }
        };

        if !desired.active {
            session_id = None;
            attempt = 0;
            drain_audio(&mut audio_rx);
            status.set(WorkerStatus::normal(desired.inactive_reason));
            if state_rx.changed().await.is_err() {
                break;
            }
            continue;
        }

        if let Some((latched_conn, latched_nonce)) = &fatal_latch {
            if *latched_conn == conn && *latched_nonce == desired.reconnect_nonce {
                drain_audio(&mut audio_rx);
                if state_rx.changed().await.is_err() {
                    break;
                }
                continue;
            }
            fatal_latch = None;
            attempt = 0;
            session_id = None;
        }

        status.set(if attempt == 0 {
            WorkerStatus::normal("Connecting to captions server…")
        } else {
            WorkerStatus::warning("Reconnecting to captions server…")
        });

        match run_single_connection(
            &conn,
            desired.reconnect_nonce,
            &mut session_id,
            &mut state_rx,
            &mut audio_rx,
            &status,
        )
        .await
        {
            SessionOutcome::ControlChanged => {
                attempt = 0;
                if !state_rx.has_changed().unwrap_or(false) && state_rx.borrow().active {
                    // Nothing left to wait on if channel closed.
                    if state_rx.changed().await.is_err() {
                        break;
                    }
                }
            }
            SessionOutcome::Fatal { status: st } => {
                session_id = None;
                attempt = 0;
                fatal_latch = Some((conn, desired.reconnect_nonce));
                status.set(st);
                if state_rx.changed().await.is_err() {
                    break;
                }
            }
            SessionOutcome::Retry { status: st } => {
                status.set(st);
                let delay = backoff_delay(attempt);
                attempt = attempt.saturating_add(1);
                tokio::select! {
                    _ = tokio::time::sleep(delay) => {}
                    changed = state_rx.changed() => {
                        if changed.is_err() {
                            break;
                        }
                        attempt = 0;
                    }
                }
            }
        }
    }
}

async fn run_single_connection(
    conn: &ConnectionConfig,
    reconnect_nonce: u64,
    session_id: &mut Option<String>,
    state_rx: &mut watch::Receiver<DesiredState>,
    audio_rx: &mut mpsc::Receiver<Bytes>,
    status: &Arc<StatusSink>,
) -> SessionOutcome {
    let mut request = match conn.ws_url.as_str().into_client_request() {
        Ok(req) => req,
        Err(_) => {
            return SessionOutcome::Fatal {
                status: WorkerStatus::error(
                    "Server URL is invalid — check the GString Captions filter settings.",
                ),
            };
        }
    };

    let Ok(auth_header) = HeaderValue::from_str(&format!("Bearer {}", conn.token)) else {
        return SessionOutcome::Fatal {
            status: WorkerStatus::error(
                "Capture key contains invalid characters — copy a fresh OBS plugin key from the dashboard.",
            ),
        };
    };
    request.headers_mut().insert("Authorization", auth_header);
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        HeaderValue::from_static("gstring-overlay"),
    );

    let connect_fut = tokio_tungstenite::connect_async(request);
    let connect_result = tokio::select! {
        res = tokio::time::timeout(Duration::from_secs(12), connect_fut) => res,
        changed = state_rx.changed() => {
            if changed.is_err() {
                return SessionOutcome::ControlChanged;
            }
            let next = state_rx.borrow().clone();
            if !next.active || next.connection.as_ref().ok() != Some(conn) || next.reconnect_nonce != reconnect_nonce {
                return SessionOutcome::ControlChanged;
            }
            return SessionOutcome::Retry {
                status: WorkerStatus::normal("Connecting to captions server…"),
            };
        }
    };

    let (ws, _) = match connect_result {
        Ok(Ok(pair)) => pair,
        Ok(Err(tungstenite::Error::Http(resp))) => {
            let code = resp.status().as_u16();
            return match code {
                401 => SessionOutcome::Fatal {
                    status: WorkerStatus::error(
                        "Invalid or revoked OBS plugin key — copy a fresh key from the GString dashboard.",
                    ),
                },
                403 => SessionOutcome::Fatal {
                    status: WorkerStatus::error(
                        "Captions are turned off in the dashboard. Turn them on, then click Reconnect.",
                    ),
                },
                _ => SessionOutcome::Retry {
                    status: WorkerStatus::warning(format!(
                        "Captions server returned HTTP {code}. Retrying…"
                    )),
                },
            };
        }
        Ok(Err(_)) | Err(_) => {
            return SessionOutcome::Retry {
                status: WorkerStatus::warning("Can't reach the captions server. Retrying…"),
            };
        }
    };

    let (mut sink, mut stream) = ws.split();

    let hello = CaptureIn::Hello {
        sample_rate: TARGET_SAMPLE_RATE,
        resume: session_id.as_deref(),
    };
    if send_json(&mut sink, &hello).await.is_err() {
        return SessionOutcome::Retry {
            status: WorkerStatus::warning("Connection closed during handshake. Retrying…"),
        };
    }

    let mut current_muted = state_rx.borrow().muted;
    if current_muted {
        let _ = send_json(&mut sink, &CaptureIn::Mute { muted: true }).await;
    }

    // Flush any audio buffered while disconnected so stale words aren't sent.
    drain_audio(audio_rx);

    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // Consume immediate first tick.
    ping_interval.tick().await;
    let mut awaiting_pong_since: Option<tokio::time::Instant> = None;

    loop {
        tokio::select! {
            changed = state_rx.changed() => {
                if changed.is_err() {
                    let _ = send_json(&mut sink, &CaptureIn::Stop).await;
                    let _ = sink.close().await;
                    return SessionOutcome::ControlChanged;
                }
                let next = state_rx.borrow().clone();
                if !next.active
                    || next.connection.as_ref().ok() != Some(conn)
                    || next.reconnect_nonce != reconnect_nonce
                {
                    let _ = send_json(&mut sink, &CaptureIn::Stop).await;
                    let _ = sink.close().await;
                    return SessionOutcome::ControlChanged;
                }
                if next.muted != current_muted {
                    current_muted = next.muted;
                    if current_muted {
                        drain_audio(audio_rx);
                        status.set(WorkerStatus::warning("Muted in OBS — not sending audio"));
                    } else {
                        status.set(WorkerStatus::normal("Connected — listening"));
                    }
                    if send_json(&mut sink, &CaptureIn::Mute { muted: current_muted }).await.is_err() {
                        return SessionOutcome::Retry {
                            status: WorkerStatus::warning("Connection lost. Reconnecting…"),
                        };
                    }
                }
            }

            maybe_frame = audio_rx.recv() => {
                let Some(frame) = maybe_frame else {
                    let _ = send_json(&mut sink, &CaptureIn::Stop).await;
                    let _ = sink.close().await;
                    return SessionOutcome::ControlChanged;
                };
                if current_muted {
                    continue;
                }
                if tokio::time::timeout(Duration::from_secs(5), sink.send(Message::Binary(frame)))
                    .await
                    .is_err()
                {
                    return SessionOutcome::Retry {
                        status: WorkerStatus::warning("Connection stalled. Reconnecting…"),
                    };
                }
            }

            _ = ping_interval.tick() => {
                if let Some(since) = awaiting_pong_since
                    && since.elapsed() >= PING_TIMEOUT
                {
                    return SessionOutcome::Retry {
                        status: WorkerStatus::warning("Captions server stopped responding. Reconnecting…"),
                    };
                }
                if awaiting_pong_since.is_none() {
                    awaiting_pong_since = Some(tokio::time::Instant::now());
                    if send_json(&mut sink, &CaptureIn::Ping).await.is_err() {
                        return SessionOutcome::Retry {
                            status: WorkerStatus::warning("Connection lost. Reconnecting…"),
                        };
                    }
                }
            }

            incoming = stream.next() => {
                match incoming {
                    Some(Ok(Message::Text(text))) => {
                        awaiting_pong_since = None;
                        if let Ok(msg) = serde_json::from_str::<ServerOut>(&text) {
                            match msg {
                                ServerOut::Status { session, status: s }
                                | ServerOut::Snapshot { session, status: s } => {
                                    *session_id = session;
                                    if !current_muted {
                                        status.set(status_from_server(&s));
                                    }
                                }
                                ServerOut::Error { code, message } => {
                                    if code == "provider" {
                                        status.set(WorkerStatus::warning(
                                            "Speech service temporarily unavailable — retrying…",
                                        ));
                                    } else {
                                        return SessionOutcome::Fatal {
                                            status: WorkerStatus::error(fatal_verdict_message(&code, &message)),
                                        };
                                    }
                                }
                                ServerOut::Other => {}
                            }
                        }
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        awaiting_pong_since = None;
                        if sink.send(Message::Pong(payload)).await.is_err() {
                            return SessionOutcome::Retry {
                                status: WorkerStatus::warning("Connection lost. Reconnecting…"),
                            };
                        }
                    }
                    Some(Ok(Message::Pong(_))) => {
                        awaiting_pong_since = None;
                    }
                    Some(Ok(Message::Close(_))) | Some(Err(_)) | None => {
                        return SessionOutcome::Retry {
                            status: WorkerStatus::warning("Connection closed. Reconnecting…"),
                        };
                    }
                    Some(Ok(_)) => {}
                }
            }
        }
    }
}

fn status_from_server(status: &str) -> WorkerStatus {
    match status {
        "speaking" => WorkerStatus::normal("Live — audio reaching captions server"),
        "listening" => WorkerStatus::normal("Connected — listening"),
        "idle" => WorkerStatus::normal("Connected — waiting for speech"),
        "reconnecting" => WorkerStatus::warning("Reconnecting speech service…"),
        "error" => WorkerStatus::error("Speech service unavailable — retrying…"),
        _ => WorkerStatus::normal("Connected"),
    }
}

async fn send_json<S, E>(sink: &mut S, value: &impl Serialize) -> Result<(), ()>
where
    S: futures_util::Sink<Message, Error = E> + Unpin,
{
    let text = serde_json::to_string(value).map_err(|_| ())?;
    tokio::time::timeout(
        Duration::from_secs(5),
        sink.send(Message::Text(text.into())),
    )
    .await
    .map_err(|_| ())?
    .map_err(|_| ())
}

fn drain_audio(rx: &mut mpsc::Receiver<Bytes>) {
    while rx.try_recv().is_ok() {}
}

fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.min(6);
    let base = BASE_BACKOFF_MS
        .saturating_mul(1u64 << shift)
        .min(MAX_BACKOFF_MS);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    // ±25% jitter without pulling in `rand`.
    let jitter_pct = 75 + u64::from(nanos % 51);
    Duration::from_millis((base * jitter_pct) / 100)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    #[tokio::test]
    #[allow(clippy::result_large_err)]
    async fn worker_authenticates_with_bearer_sends_hello_audio_mute_and_stop() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut seen_auth = String::new();
            let mut seen_proto = String::new();
            let mut ws = accept_hdr_async(stream, |req: &Request, mut resp: Response| {
                seen_auth = req
                    .headers()
                    .get("authorization")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                seen_proto = req
                    .headers()
                    .get("sec-websocket-protocol")
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .to_owned();
                resp.headers_mut().insert(
                    "sec-websocket-protocol",
                    HeaderValue::from_static("gstring-overlay"),
                );
                Ok(resp)
            })
            .await
            .unwrap();

            assert_eq!(seen_auth, "Bearer v1.test.token");
            assert_eq!(seen_proto, "gstring-overlay");

            // Expect hello first.
            let hello = match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => t.to_string(),
                other => panic!("expected hello text, got {other:?}"),
            };
            let v: serde_json::Value = serde_json::from_str(&hello).unwrap();
            assert_eq!(v["type"], "hello");
            assert_eq!(v["sample_rate"], 16_000);

            // Reply with status.
            ws.send(Message::Text(
                r#"{"type":"status","session":"0000000000000001","status":"listening"}"#.into(),
            ))
            .await
            .unwrap();

            // Expect binary PCM frame.
            let pcm = match ws.next().await.unwrap().unwrap() {
                Message::Binary(b) => b,
                other => panic!("expected binary pcm, got {other:?}"),
            };
            assert_eq!(pcm.as_ref(), &[1, 2, 3, 4]);

            // Expect mute.
            let mute = match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => t.to_string(),
                other => panic!("expected mute text, got {other:?}"),
            };
            let mv: serde_json::Value = serde_json::from_str(&mute).unwrap();
            assert_eq!(mv["type"], "mute");
            assert_eq!(mv["muted"], true);

            // Expect stop when deactivated.
            let stop = match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => t.to_string(),
                other => panic!("expected stop text, got {other:?}"),
            };
            let sv: serde_json::Value = serde_json::from_str(&stop).unwrap();
            assert_eq!(sv["type"], "stop");
        });

        let initial = DesiredState {
            connection: Ok(ConnectionConfig {
                ws_url: format!("ws://{addr}/overlay/ws"),
                token: "v1.test.token".to_owned(),
            }),
            active: true,
            inactive_reason: String::new(),
            muted: false,
            reconnect_nonce: 0,
        };
        let (state_tx, state_rx) = watch::channel(initial.clone());
        let (audio_tx, audio_rx) = mpsc::channel(8);
        let status = Arc::new(StatusSink::new(
            WorkerStatus::normal("init"),
            Arc::new(|_| {}),
        ));

        let worker_task = tokio::spawn(run_worker(state_rx, audio_rx, Arc::clone(&status)));

        // Wait briefly for status to reach listening.
        for _ in 0..50 {
            if status.get().text.contains("listening") {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(status.get().text.contains("listening"));

        audio_tx
            .send(Bytes::from_static(&[1, 2, 3, 4]))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        let mut muted_state = initial.clone();
        muted_state.muted = true;
        state_tx.send(muted_state).unwrap();
        tokio::time::sleep(Duration::from_millis(30)).await;

        let mut stopped_state = initial;
        stopped_state.active = false;
        stopped_state.inactive_reason = "Waiting for stream".to_owned();
        state_tx.send(stopped_state).unwrap();

        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();

        drop(state_tx);
        drop(audio_tx);
        tokio::time::timeout(Duration::from_secs(5), worker_task)
            .await
            .unwrap()
            .unwrap();
    }
}
