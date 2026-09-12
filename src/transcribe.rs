//! Gemini 3.5 Transcribe Live client (Google AI Studio).
//!
//! Docs: https://ai.google.dev/gemini-api/docs/live-api/live-transcribe
//! Model: `gemini-3.5-transcribe-live` over Live API WebSocket.
//! - Setup: `{ setup: { model, generationConfig:{responseModalities:["TEXT"]},
//!            inputAudioTranscription:{ languageCodes:[], mode:"SMART" } } }`
//!   SMART = disfluency removal (ums/ahs), grammar cleanup, auto-format.
//!   VERBATIM is the default; we explicitly request SMART for dictation.
//! - Audio: `realtimeInput.audio = { data: base64(PCM16LE 16k mono), mimeType }`
//! - Push-to-talk (manual VAD): `realtimeInput.activityStart` on press,
//!   `realtimeInput.activityEnd` + `audioStreamEnd:true` on release.
//! - Server: `serverContent.interimInputTranscription.text` (live preview) and
//!   `serverContent.inputTranscription.text` (final, SMART-cleaned).
//! - Limits: 10 min/session, 85+ langs, custom vocab ≤1000 terms.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use std::net::TcpStream;
use tungstenite::{connect, stream::MaybeTlsStream, Message, WebSocket};

pub const MODEL: &str = "gemini-3.5-transcribe-live";
pub const WS_URL: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
pub const PCM_MIME: &str = "audio/pcm;rate=16000";

pub type Ws = WebSocket<MaybeTlsStream<TcpStream>>;

pub fn ws_url(api_key: &str) -> String {
    format!("{WS_URL}?key={api_key}")
}

/// Build the setup JSON.
/// - model + TEXT modality + inputAudioTranscription{languageCodes, mode}.
/// - SMART mode: disfluency removal (ums/ahs), grammar cleanup, formatting.
///   Accepted by the server (setupComplete) and verified live.
/// - Manual push-to-talk uses realtimeInput.activityStart/End messages; no
///   realtimeInputConfig needed (auto-VAD settings left at server default —
///   explicit automaticActivityDetection objects were also tried and the
///   session stayed mute; the documented minimal setup is what works).
///
/// Transcription-mode presets for the settings menu.
pub const MODES: &[&str] = &["smart", "verbatim"];

/// Normalize user input to a canonical mode. Unknown => smart (default).
pub fn normalize_mode(want: &str) -> &'static str {
    let w: String = want.chars().filter(|c| !c.is_whitespace()).collect();
    if w.eq_ignore_ascii_case("verbatim") {
        MODES[1]
    } else {
        MODES[0]
    }
}

pub fn setup_json(language_codes: &[String], mode: &str) -> String {
    let langs: Vec<String> = language_codes.to_vec();
    let langs_json = serde_json::to_string(&langs).unwrap_or_else(|_| "[]".to_string());
    let wire = if normalize_mode(mode) == "verbatim" {
        "VERBATIM"
    } else {
        "SMART"
    };
    format!(
        "{{\"setup\":{{\"model\":\"models/{MODEL}\",\
        \"generationConfig\":{{\"responseModalities\":[\"TEXT\"]}},\
        \"inputAudioTranscription\":{{\"languageCodes\":{langs_json},\"mode\":\"{wire}\"}}}}}}"
    )
}

pub fn connect_live(api_key: &str, language_codes: &[String], mode: &str) -> Result<Ws, String> {
    connect_live_timeout(
        api_key,
        language_codes,
        mode,
        std::time::Duration::from_secs(10),
    )
}

fn connect_live_timeout(
    api_key: &str,
    language_codes: &[String],
    mode: &str,
    timeout: std::time::Duration,
) -> Result<Ws, String> {
    use std::sync::mpsc;
    let url = ws_url(api_key);
    let langs = language_codes.to_vec();
    let (tx, rx) = mpsc::channel::<Result<Ws, String>>();
    // Handshake on a helper thread: tungstenite's TCP/TLS connect has no
    // timeout, and sick networks (dead IPv6 route, captive portal) can stall
    // it for minutes — which would freeze dictation mid-press. Bound it; a
    // timed-out attempt orphans its thread, which exits on its own. (Found
    // headless-testing: connect with an unreachable route never returned.)
    let _ = std::thread::Builder::new()
        .name("utterly-connect".into())
        .stack_size(512 * 1024) // transient; gone when the attempt ends
        .spawn(move || {
            let res = connect(url)
                .map(|(ws, _response)| ws)
                .map_err(|e| format!("ws connect: {e}"));
            let _ = tx.send(res);
        });
    // Spawn failure also funnels here (no sender => timeout), never a hang.
    let mut ws = rx
        .recv_timeout(timeout)
        .map_err(|_| "ws connect timed out after 10s (check network / proxy)".to_string())??;
    let setup = setup_json(&langs, mode);
    ws.send(Message::Text(setup))
        .map_err(|e| format!("ws setup: {e}"))?;
    Ok(ws)
}

#[inline]
pub fn send_pcm(ws: &mut Ws, pcm: &[i16]) -> Result<(), String> {
    // Reuse a thread-local buffer for JSON building so the 10/sec hot loop
    // doesn't re-allocate the ~4.3KB frame each chunk. The final Message still
    // needs an owned String (tungstenite takes ownership), so we clone once
    // for the socket and put the warm buffer back for the next chunk. This
    // saves the base64 temp + frame-build allocs; only the socket-owned copy
    // allocates per chunk. take/replace avoids holding the RefCell borrow
    // across blocking IO.
    let mut buf = PCM_JSON_BUF.with(|cell| cell.take());
    realtime_audio_json_into(pcm, &mut buf);
    let msg = buf.clone();
    PCM_JSON_BUF.with(|cell| cell.replace(buf));
    ws.send(Message::Text(msg))
        .map_err(|e| format!("ws send: {e}"))
}

std::thread_local! {
    static PCM_JSON_BUF: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// Build the `realtimeInput.audio` JSON for one PCM chunk. Pure function so
/// the wire format is unit-testable (a malformed frame would be silently
/// ignored by the server — zero transcript, zero error).
/// Kept as the owned wrapper around `realtime_audio_json_into` for callers /
/// tests; the hot loop (`send_pcm`) uses the `into` variant directly.
#[allow(dead_code)]
pub fn realtime_audio_json(pcm: &[i16]) -> String {
    let mut msg = String::new();
    realtime_audio_json_into(pcm, &mut msg);
    msg
}

/// Clear `buf` and write the same bytes `realtime_audio_json` would return.
/// Reuses `buf`'s capacity so the 10/sec hot loop avoids a fresh ~4.3KB
/// `String` alloc per chunk; a second call with the same-sized `pcm` must
/// not grow capacity.
pub fn realtime_audio_json_into(pcm: &[i16], buf: &mut String) {
    buf.clear();
    // Zero-copy-ish: reinterpret i16 LE bytes without an extra Vec<i16> copy.
    let bytes: &[u8] =
        unsafe { std::slice::from_raw_parts(pcm.as_ptr() as *const u8, pcm.len() * 2) };
    // Pre-reserve base64 ((n+2)/3*4) + JSON overhead so a warm buffer never
    // reallocs for same-sized chunks.
    let b64_len = bytes.len().div_ceil(3) * 4;
    buf.reserve(b64_len + 96);
    buf.push_str("{\"realtimeInput\":{\"audio\":{\"data\":\"");
    // Append base64 directly into the reused buffer (no temp String alloc).
    B64.encode_string(bytes, buf);
    buf.push_str("\",\"mimeType\":\"");
    buf.push_str(PCM_MIME);
    buf.push_str("\"}}}");
}

pub fn send_activity_start(ws: &mut Ws) -> Result<(), String> {
    // Manual VAD turn-boundary (push-to-talk). MUST be nested inside
    // realtimeInput — a top-level {"activityStart":{}} makes the server close
    // the session with: Invalid JSON payload / Unknown name "activityStart".
    // (Caught live-testing: the close reason names the bad field exactly.)
    ws.send(Message::Text(
        "{\"realtimeInput\":{\"activityStart\":{}}}".to_string(),
    ))
    .map_err(|e| format!("ws activityStart: {e}"))
}

/// End-of-turn marker for manual VAD, sent on key release (before the
/// audio-stream end below so the final turn boundary is explicit).
pub fn send_activity_end(ws: &mut Ws) -> Result<(), String> {
    ws.send(Message::Text(
        "{\"realtimeInput\":{\"activityEnd\":{}}}".to_string(),
    ))
    .map_err(|e| format!("ws activityEnd: {e}"))
}

pub fn send_audio_end(ws: &mut Ws) -> Result<(), String> {
    ws.send(Message::Text(
        "{\"realtimeInput\":{\"audioStreamEnd\":true}}".to_string(),
    ))
    .map_err(|e| format!("ws audioStreamEnd: {e}"))
}

/// One parsed server event.
#[derive(Debug, Default)]
pub struct TranscriptEvent {
    /// Low-latency preview; render grey/italic, replace on each update.
    pub interim: Option<String>,
    /// Authoritative SMART-cleaned segment; append + commit on release.
    pub finalized: Option<String>,
    /// True when server closed / error text encountered.
    pub closed: bool,
}

/// Parse a raw server JSON text frame. Never panics; unknown shapes => empty event.
pub fn parse_server_text(raw: &str) -> TranscriptEvent {
    let mut ev = TranscriptEvent::default();
    let v: serde_json::Value = match serde_json::from_str(raw) {
        Ok(v) => v,
        Err(_) => return ev,
    };
    let sc = &v["serverContent"];
    if sc.is_null() {
        // setupComplete / errors surface here; treat GoAway/close as closed.
        if v["setupComplete"].is_object() {
            return ev;
        }
        return ev;
    }
    if let Some(t) = sc["interimInputTranscription"]["text"].as_str() {
        if !t.is_empty() {
            ev.interim = Some(t.to_string());
        }
    }
    if let Some(t) = sc["inputTranscription"]["text"].as_str() {
        if !t.is_empty() {
            ev.finalized = Some(t.to_string());
        }
    }
    if sc["generationComplete"].as_bool() == Some(true) {
        ev.closed = true;
    }
    ev
}

/// True for read-timeout IO errors (WouldBlock/TimedOut from `set_read_timeout`).
/// Pure helper so timeout mapping is unit-testable without a live socket.
pub fn is_timeout_err(e: &tungstenite::Error) -> bool {
    matches!(
        e,
        tungstenite::Error::Io(io)
        if matches!(
            io.kind(),
            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
        )
    )
}

/// True when the socket is already gone (clean close handshake done, or a
/// read/write after it). Maps to a closed event, not a timeout retry.
pub fn is_closed_err(e: &tungstenite::Error) -> bool {
    matches!(
        e,
        tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed
    )
}

/// Closed-session event: server close frame or closed-connection error.
pub fn closed_event() -> TranscriptEvent {
    TranscriptEvent {
        closed: true,
        ..TranscriptEvent::default()
    }
}

/// Read one raw server frame with timeout. Used at session start to verify
/// the setup while a rejection is still loud: the server answers malformed
/// requests with close frames carrying code + reason (e.g. unknown fields),
/// which a typed parser would swallow silently.
pub fn recv_raw(ws: &mut Ws, ms: u64) -> Option<String> {
    let timeout = std::time::Duration::from_millis(ms);
    match ws.get_mut() {
        MaybeTlsStream::Plain(p) => {
            let _ = p.set_read_timeout(Some(timeout));
        }
        MaybeTlsStream::Rustls(s) => {
            let _ = s.get_ref().set_read_timeout(Some(timeout));
        }
        _ => {}
    }
    match ws.read() {
        Ok(Message::Text(t)) => Some(t),
        Ok(Message::Binary(b)) => Some(String::from_utf8_lossy(&b).into_owned()),
        Ok(Message::Close(frame)) => Some(match frame {
            Some(f) => format!("<CLOSE code={:?} reason={}>", f.code, f.reason),
            None => "<CLOSE no frame>".to_string(),
        }),
        Ok(_) => Some(String::new()),
        Err(e) if is_timeout_err(&e) => None,
        Err(e) => Some(format!("<read error: {e}>")),
    }
}

/// Blocking receive with timeout. Returns None on timeout (caller keeps streaming).
/// The timeout is set on the INNER TcpStream so it applies to both the Plain
/// and Rustls variants (rustls `StreamOwned::get_ref` yields the TcpStream).
/// Without this, reads on a TLS stream block forever and stall the audio loop.
pub fn recv_timeout(ws: &mut Ws, ms: u64) -> Option<TranscriptEvent> {
    let timeout = std::time::Duration::from_millis(ms);
    match ws.get_mut() {
        MaybeTlsStream::Plain(p) => {
            let _ = p.set_read_timeout(Some(timeout));
        }
        MaybeTlsStream::Rustls(s) => {
            let _ = s.get_ref().set_read_timeout(Some(timeout));
        }
        // MaybeTlsStream is #[non_exhaustive] (future TLS backends).
        _ => {}
    }
    match ws.read() {
        Ok(Message::Text(t)) => Some(parse_server_text(&t)),
        Ok(Message::Binary(b)) => Some(parse_server_text(&String::from_utf8_lossy(&b))),
        Ok(Message::Close(_)) => Some(closed_event()),
        Ok(_) => Some(TranscriptEvent::default()),
        Err(e) if is_closed_err(&e) => Some(closed_event()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn setup_requests_smart_text_mode() {
        let s = setup_json(&[], "smart");
        assert!(s.contains("models/gemini-3.5-transcribe-live"), "{s}");
        assert!(s.contains("\"TEXT\""), "{s}");
        assert!(s.contains("\"SMART\""), "{s}");
        assert!(s.contains("\"languageCodes\":[]"), "{s}");
    }

    #[test]
    fn setup_verbatim_mode() {
        let s = setup_json(&[], "verbatim");
        assert!(s.contains("\"VERBATIM\""), "{s}");
        assert!(!s.contains("SMART"), "{s}");
        // Unknown input falls back to smart (the default).
        assert!(setup_json(&[], "nonsense").contains("\"SMART\""));
        assert_eq!(normalize_mode(" VERBATIM "), "verbatim");
    }

    #[test]
    fn setup_carries_language_hints() {
        let s = setup_json(&["en-US".to_string()], "smart");
        assert!(s.contains("en-US"), "{s}");
    }

    #[test]
    fn parse_interim_and_final() {
        let raw = r#"{"serverContent":{"interimInputTranscription":{"text":"hello wo"},"inputTranscription":{"text":"Hello world."}}}"#;
        let ev = parse_server_text(raw);
        assert_eq!(ev.interim.as_deref(), Some("hello wo"));
        assert_eq!(ev.finalized.as_deref(), Some("Hello world."));
        assert!(!ev.closed);
    }

    #[test]
    fn parse_garbage_never_panics() {
        let ev = parse_server_text("not json{{{");
        assert!(ev.interim.is_none() && ev.finalized.is_none() && !ev.closed);
        let ev = parse_server_text(r#"{"setupComplete":{}}"#);
        assert!(!ev.closed);
    }

    #[test]
    fn realtime_audio_json_into_reuses_buffer() {
        let pcm: Vec<i16> = (0..1600).map(|i| ((i * 37) % 1000 - 500) as i16).collect();
        let expected = realtime_audio_json(&pcm);
        let mut buf = String::new();
        realtime_audio_json_into(&pcm, &mut buf);
        assert_eq!(buf, expected, "into-variant must match owned variant");
        let cap = buf.capacity();
        assert!(cap >= buf.len(), "buffer must fit output");
        realtime_audio_json_into(&pcm, &mut buf);
        assert_eq!(buf, expected, "second call output must match");
        assert_eq!(
            buf.capacity(),
            cap,
            "second call with same buf must not grow capacity"
        );
    }

    #[test]
    fn realtime_audio_json_round_trips() {
        use base64::Engine as _;
        let pcm: Vec<i16> = (0..1600).map(|i| ((i * 37) % 1000 - 500) as i16).collect();
        let msg = realtime_audio_json(&pcm);
        let v: serde_json::Value = serde_json::from_str(&msg).expect("valid JSON");
        assert_eq!(
            v["realtimeInput"]["audio"]["mimeType"],
            "audio/pcm;rate=16000"
        );
        let raw = base64::engine::general_purpose::STANDARD
            .decode(v["realtimeInput"]["audio"]["data"].as_str().unwrap())
            .expect("valid base64");
        assert_eq!(raw.len(), 3200, "100ms x 16-bit mono");
        // Little-endian round-trip of first + last sample.
        let first = i16::from_le_bytes([raw[0], raw[1]]);
        let last = i16::from_le_bytes([raw[3198], raw[3199]]);
        assert_eq!((first, last), (pcm[0], pcm[1599]));
    }

    #[test]
    fn timeout_io_kinds_map_to_none() {
        use std::io;
        let wb = tungstenite::Error::Io(io::Error::new(io::ErrorKind::WouldBlock, "would block"));
        let to = tungstenite::Error::Io(io::Error::new(io::ErrorKind::TimedOut, "timed out"));
        assert!(is_timeout_err(&wb), "WouldBlock must be a timeout");
        assert!(is_timeout_err(&to), "TimedOut must be a timeout");
        let other = tungstenite::Error::Io(io::Error::new(io::ErrorKind::ConnectionReset, "reset"));
        assert!(
            !is_timeout_err(&other),
            "non-timeout IO must not be a timeout"
        );
        assert!(
            !is_timeout_err(&tungstenite::Error::ConnectionClosed),
            "close must not be a timeout"
        );
    }

    #[test]
    fn close_maps_to_closed_event() {
        let ev = closed_event();
        assert!(ev.closed, "close helper must set closed:true");
        assert!(ev.interim.is_none() && ev.finalized.is_none());
        assert!(is_closed_err(&tungstenite::Error::ConnectionClosed));
        assert!(is_closed_err(&tungstenite::Error::AlreadyClosed));
        use std::io;
        let wb = tungstenite::Error::Io(io::Error::new(io::ErrorKind::WouldBlock, "would block"));
        assert!(!is_closed_err(&wb), "timeout must not map to closed");
    }
}
