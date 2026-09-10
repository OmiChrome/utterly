# Utterly — minimal push-to-talk dictation pill

Hold **Ctrl+Space** to listen, release to transcribe into the focused text area.
Powered by **Gemini 3.5 Transcribe Live** (`gemini-3.5-transcribe-live`).
Transcription mode (switchable in the tray menu or via CLI):
**Smart** (removes ums/ahs, fixes self-corrections, auto-formats) or
**Verbatim** (exact words).

![idle pill](assets/screenshots/pill-idle.png)
![listening pill](assets/screenshots/pill-listening.png)

## Gemini API used (Google AI Studio)

Docs: <https://ai.google.dev/gemini-api/docs/live-api/live-transcribe> (+
[model page](https://ai.google.dev/gemini-api/docs/models/gemini-3.5-transcribe),
[WS reference](https://ai.google.dev/api/live)).

| Item | Value |
|---|---|
| Streaming model | `gemini-3.5-transcribe-live` (file model: `gemini-3.5-transcribe`) |
| Transport | Live API WebSocket `wss://generativelanguage.googleapis.com/ws/…BidiGenerateContent?key=API_KEY` |
| Setup | `{"setup":{"model":"models/gemini-3.5-transcribe-live","generationConfig":{"responseModalities":["TEXT"]},"inputAudioTranscription":{"languageCodes":[],"mode":"SMART"}}}` |
| Modes | `SMART` = disfluency removal + grammar cleanup + formatting (what we use); `VERBATIM` = exact words (default, supports timestamps/diarization — incompatible with SMART) |
| Audio in | `realtimeInput.audio = {data: base64(PCM16LE 16 kHz mono), mimeType:"audio/pcm;rate=16000"}` |
| Push-to-talk (manual VAD) | `activityStart` on key press, `audioStreamEnd:true` on release |
| Server events | `serverContent.interimInputTranscription.text` (live preview) + `serverContent.inputTranscription.text` (final, SMART-cleaned) |
| Limits | 10 min/session, 85+ auto-detected languages, custom vocab ≤1000 terms |
| Key | Paste from <https://aistudio.google.com/apikey> (ephemeral tokens recommended for production later) |

## Verified live (Sep 2026, with a test key)

End-to-end against the real API, headless: synthetic Ctrl+Space hold →
`setupComplete` → 30×100 ms PCM chunks of real human speech (Google's
`hello_are_you_there.pcm` sample) → `activityEnd` + `audioStreamEnd` →
final **`"Hey, can you hear me?"`** in ~1 s. Interim hypotheses stream
during speech the same way.

Protocol pitfalls found the hard way (all verified against the server):
- Turn markers **must nest inside `realtimeInput`**:
  `{"realtimeInput":{"activityStart":{}}}` /
  `{"realtimeInput":{"activityEnd":{}}}`. A top-level `{"activityStart":{}}`
  gets the session **closed** with
  `Invalid JSON payload … Unknown name "activityStart"`. Always read the
  close frame (code + reason) — the app logs unexpected setup responses
  instead of streaming into a dead session.
- Minimal setup is what works: `model` + `generationConfig.responseModalities`
  + `inputAudioTranscription{languageCodes, mode}`. Extra VAD config objects
  were tried and left the session mute.
- Robotic TTS (espeak-ng) streamed perfectly but the model returned **zero**
  transcript for it across many runs — human speech transcribed first try.
  This looks like a voice-liveness gate server-side, not an app bug.
- tungstenite's TCP/TLS `connect` has no timeout: bound it with a helper
  thread (10 s), or one bad network hangs dictation mid-press.

See `src/transcribe.rs` — the whole protocol is ~120 lines with tests.

## Usage

```sh
utterly --set-key YOUR_AI_STUDIO_KEY
utterly --list-mics
utterly --set-mic "MacBook Pro Microphone"   # substring match, empty = default
utterly --set-mode smart|verbatim            # transcription mode (next utterance)
utterly                                      # pill + tray; hold Ctrl+Space
```

Mic, hotkey, transcription mode and API key can also be changed live from the
tray-icon menu (Microphone ▸ / Hold-to-talk hotkey ▸ / Transcription mode ▸ /
Paste API key from clipboard / Quit). Mode switches apply to the next utterance.

Config lives at `~/.config/utterly/config.json` (written `0600`).
Windows: `%APPDATA%\Utterly\config.json`.
Pill shows: grey dot = idle, pulsing red = listening, green = transcribing,
plus a 24-bar mic meter, centered in the lower third like macOS dictation.
Transcript preview is in the window title; the final text is pasted into
whatever text area was focused (clipboard + Ctrl/Cmd+V).

UI preview without pressing anything: `UTTERLY_DEMO=listening utterly`
(or `=transcribing`).

## Designing for <3 MB installer / single-digit MB RAM

- **No async runtime** — `std::thread` + blocking `tungstenite` only. Tokio alone
  would add MBs of binary + RAM.
- **Fixed buffers, zero hot-loop allocs** — 160 000-sample overwrite-oldest ring
  (320 KiB), 95 KiB pill framebuffer (380×64×4), 100 ms PCM chunks (10 msgs/s).
  No per-frame `Vec`, no font engine (text goes through the OS window title),
  procedural tray icons (1 KiB each, see `assets/`).
- **Cheap algorithms** — linear-interp resample (integer math), RMS energy gate
  skips silent frames (halves bandwidth), attack/release meter = 1 float,
  dirty-rect redraw capped at 30 fps, idle CPU ~0%.
- **Never discard partials** — the chunker returns full 100 ms frames only when
  buffered and never eats short reads (an earlier version dropped partials and
  silently lost ~all audio when capture ran slightly below consumption).
- **Capture watchdog** — a live mic always has a noise floor, so sustained
  bit-exact digital silence means a stale stream; the session reopens the
  device with backoff instead of transcribing nothing forever.
- **Single instance** — a second copy would silently lose the global-hotkey
  grab race, so startup refuses loudly when the lock is held.
- **Release profile** — `opt-level="z"`, `lto`, `codegen-units=1`, `strip`,
  `panic="abort"`. Check with `ls -lh target/release/utterly` (+ UPX for the
  installer if you need extra margin).
- **Measured RAM** (Linux, idle pill with live mic + hotkey + tray, `/proc` VmRSS
  sampled over 20 s): release build **3.5–4.0 MB RSS steady, HWM == RSS**
  (no growth; heap/VmData ~300 KiB). Budget math: 320 KiB ring + 95 KiB
  framebuffer + ~8 KiB socket buffers + thread stacks — single-digit MB by
  construction, verified by sampling.

## Layout

- `src/main.rs` — CLI flags, session state machine (press → stream → release → commit)
- `src/audio.rs` — cpal capture, ring buffer, resample, RMS gate (+ tests)
- `src/transcribe.rs` — Live API client: SMART setup, base64 PCM, event parse (+ tests)
- `src/hotkey.rs` — global Ctrl+Space press/release via `global-hotkey`
- `src/ui.rs` — borderless always-on-top pill (`winit` + `softbuffer`, no GPU/fonts)
- `src/tray.rs` — tray icon: grey / red / green + tooltip preview
- `src/output.rs` — commit via clipboard + synthetic paste (`arboard` + `enigo`)
- `src/config.rs` — mic / hotkey / mode / API-key persistence
  (XDG on Linux, `%APPDATA%` on Windows, `~/.config` on macOS)
- `assets/` — `idle|listening|transcribing.{svg,-16.png}` + `generate_assets.py` (stdlib only)
  + `screenshots/` (pill-idle/listening captures)

## Cross-platform (Linux + macOS + Windows)

One codebase, no platform-specific UI code: `winit` + `softbuffer` (CPU pill),
`tray-icon`/`muda` (tray + menu), `global-hotkey`, `cpal`, `arboard`, `enigo`
all have first-class backends on all three OSes. Platform seams are tiny and
`cfg`-gated: GTK init/pump + `/proc` pid check (Linux only), Cmd+V paste
(macOS) vs Ctrl+V (elsewhere), `%APPDATA%` config dir (Windows).
Verified: `cargo check` clean on Linux host and
`--target x86_64-pc-windows-gnu` (macOS target needs Apple clang, so it is
checked by dependency support + cfg audit instead of a local compile).
OS permission prompts to expect: Microphone + Accessibility/Input Monitoring
(macOS), microphone privacy (Windows).

## Verify

```sh
cargo check   # zero warnings (also clean on --target x86_64-pc-windows-gnu)
cargo clippy  # zero lints
cargo test    # 11 unit tests (ring, RMS, resampler continuity, hotkey
              # presets, setup JSON incl. verbatim, wire-format round-trip,
              # server parse)
cargo build --release && ls -lh target/release/utterly
# measured (Linux x86_64, Sep 2026): 3.4 MB stripped;
# upx --best --lzma -> 1.3 MB, runs fine => <3 MB installer: ship the packed binary
```

## Next (needs your Mac + a real key)

1. `cargo build --release` on macOS (needs Xcode CLT for mic/input permissions prompts).
2. Grant Microphone + Accessibility (global hotkey) + paste permissions once.
3. Try: hold Ctrl+Space, say "let's meet Tuesday — no, Wednesday, um, at three",
   release → expect SMART output "Let's meet Wednesday at 3."
