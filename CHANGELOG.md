# Changelog

All notable user-facing changes to Utterly are recorded here. Format follows
Keep a Changelog: `Added`, `Changed`, `Fixed` under each release.

## [0.1.0] - 2026-09-10

First release.

### Added

- Push-to-talk dictation pill: hold Ctrl+Space to listen, release to
  transcribe into the focused text area.
- Gemini 3.5 Transcribe Live backend with Smart mode (removes ums and ahs,
  fixes self-corrections, formats text) and Verbatim mode (exact words),
  switchable per utterance.
- Tray icon with settings menu: microphone picker, hold-to-talk hotkey
  presets (Ctrl+Space, Alt+Space, Ctrl+Shift+Space), transcription mode,
  paste-API-key-from-clipboard, quit.
- CLI flags mirroring every setting (`--list-mics`, `--set-mic`,
  `--set-hotkey`, `--set-mode`, `--set-key`).
- Borderless pill centered in the lower third with idle/listening/
  transcribing states and a live mic meter; transcript preview in the
  window title.
- Capture watchdog that reopens stale-silent microphone streams.
- Single-instance guard so a second copy can never silently steal the
  global hotkey.
- Cross-platform builds: Windows x64, Linux x64, macOS ARM64 + Intel.
- GitHub Actions checks (fmt, clippy, tests, Windows compile check) and
  tag-driven releases with attached binaries.

### Fixed

- Turn markers nested inside `realtimeInput` (top-level `activityStart`
  got the session closed by the server).
- Resampler fractional carry stalling capture after the first callback.
- Chunker discarding partial reads and losing audio under load.
- Blocking TLS connect/reads that could freeze dictation mid-press.
- Clippy identity-op lint flagged by newer toolchains.
