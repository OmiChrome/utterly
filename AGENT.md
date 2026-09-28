# Utterly working notes

Utterly is a native Rust push-to-talk dictation app. Its compact always-on-top
pill shows recording state and live transcript text; on Windows, a separate
native Win32 window provides dark Settings pages. Speech audio streams to
Gemini 3.5 Transcribe Live. Keep product claims tied to behavior in the source
and evidence in the QA report.

## Interface and defaults

- New Windows configs default to holding Ctrl+Win; releasing either key ends
  the take. Existing saved shortcuts are retained. Alt+Space, Ctrl+Space, and
  Ctrl+Shift+Space remain available. macOS/Linux default to Alt+Space.
- Pill geometry is state-dependent: 32 × 6 logical px idle (about 40 × 8
  physical px at 125% scale), 100 × 36 while recording, 440 × 80 with the live
  transcript surface, and 48 × 20 while finalizing. Temporary status notices
  use a wider compact surface.
- The Windows Settings window uses native Win32 drawing/controls and Segoe UI,
  with charcoal surfaces, muted borders, readable light text, and violet
  accents. Its pages are General, Dictionary, Intelligence, and System.
- Preferences are persisted in the config. Interaction sounds, Windows audio
  ducking/restoration, overlay positioning, the idle handle, app-icon
  visibility, and suppressing Utterly's temporary pill notices have runtime
  behavior. Muting notices does not alter Windows notification settings.
- Context Awareness, Auto Dictionary, and Smart Text Insertion are wired on
  Windows. Context is bounded to at most 160 characters on each side of the
  caret; capture rejects password, disabled, read-only, and non-editable
  controls. Context Awareness sends selected name hints as vocabulary for the
  current Gemini session. Auto Dictionary keeps conservative learned names in
  the local vocabulary; saved vocabulary accompanies Gemini sessions.
- Smart insertion uses captured text around the caret only when the same
  editable target is still focused. The transcript stays on the clipboard;
  if target validation fails, do not paste into another application.
- The original app art is `icon.png`; `scripts/prepare-icon.py` creates the PNG,
  tray RGBA, and ICO resources consumed at runtime. Recording cue WAVs are
  original generated tones from `scripts/prepare-sounds.py`. Willow screenshots,
  media, and installed assets under `docs/references/` are research material,
  not runtime dependencies or shipped assets.

## Design priorities

Keep the implementation native Rust and polish the interface before spending
time on tiny package or memory numbers. The user's former hard 5 MB package
and 20 MiB private-memory budgets are waived. Measure package size, private
bytes, working set, CPU, and active-recording behavior when useful, and label
the state measured. Windows CI reports portable package size without a 5 MB
gate; the pre-existing Linux release size gates remain. The 23 September
Windows benchmark describes the earlier
pill/settings design; it is historical context, not a measurement of this
updated interface. See [the benchmark](docs/benchmarks/windows-11-2026-09-23.md)
and [the Willow research/asset report](docs/willow-research.md).

Stay with native OS facilities where they provide good interaction. The main
pill is `winit` + `softbuffer`; Windows Settings and audio effects use Win32;
microphone capture uses `cpal`; tray menus use `tray-icon`/`muda`; clipboard
and paste use `arboard`/`enigo`. Avoid adding a web runtime or a GUI framework
for this interface. Measure before optimizing or adding dependencies.

## Source map

- `src/main.rs` — CLI, config/session state, menu commands, streaming, and
  final transcript handling.
- `src/audio.rs` — microphone capture, resampling, ring buffer, and RMS gate.
- `src/transcribe.rs` — Gemini Live WebSocket protocol.
- `src/hotkey.rs` — push-to-talk presets and platform hotkey handling.
- `src/ui.rs` — native pill rendering, positioning, transcript surface, and
  event loop.
- `src/settings.rs` — native Windows settings window and controls.
- `src/system.rs` — Windows recording cues and reversible audio ducking.
- `src/output.rs` — clipboard/paste, bounded dictionary candidates, and
  smart insertion helpers.
- `src/native_output.rs` — Windows UI Automation focus/context capture and
  focused-application icon extraction.
- `src/config.rs` — JSON preferences, vocabulary, and API-key storage.
- `src/tray.rs` — tray icon and menu.

## Data and privacy behavior

- The configured microphone audio and transcription settings go to Google's
  Gemini Live service during a take. Custom vocabulary is also sent with the
  session. Follow the account's applicable Google terms and quotas.
- When Smart Text Insertion, Context Awareness, or Auto Dictionary is enabled,
  Windows reads bounded caret context from the focused editable control. It
  does not scrape the window or screen and skips password/read-only controls.
  Context Awareness sends extracted candidate names as session vocabulary;
  Auto Dictionary persists candidates locally, after which saved custom
  vocabulary accompanies Gemini sessions. Smart insertion uses context locally.
- “Mute notifications” suppresses Utterly's temporary pill notices only. It
  does not suppress Windows or other applications' notifications.
- Windows saves the API key using DPAPI for the current user. Unix config
  files are best-effort mode 0600. Keep API keys and personal voice samples out
  of logs, tests, and commits.

## Code conventions

- Prefer direct, simple Rust and the standard library when it fits.
- Keep UI/platform objects on their owning threads; pass plain data and
  channels across threads.
- Bound audio buffers and session waits. Do not silently discard final text
  or errors that matter to the user.
- Keep tests focused on behavior and migration rules. Avoid claiming external
  service, hardware, or UI checks from unit-test results alone.

## Build and verification

```sh
cargo fmt --all -- --check
cargo check
cargo test --all-targets -- --test-threads=1
cargo clippy --all-targets -- -D warnings
cargo build --release
```

Live Gemini and physical microphone checks need a real API key and device;
state when tests use public sample audio or a simulation. Windows UI, hotkey,
audio ducking, and resource measurements need Windows verification. Record
results in `docs/qa/willow-native-2026-09-24.md` with the command or setup and
actual output. The research scripts are reproducible with Python; generating
the Willow reference set is not required to build the app.

## Release process

Version is in `Cargo.toml`; releases are produced by the tagged GitHub Actions
workflow. Update `CHANGELOG.md` for user-visible behavior. Do not commit
generated local secrets, unrelated config changes, or media captured from
personal accounts.
