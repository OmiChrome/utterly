# Changelog

All notable user-facing changes to Utterly are recorded here. Format follows
Keep a Changelog: `Added`, `Changed`, `Fixed` under each release.

## [Unreleased]

### Added

- Dark native Windows settings with General, Dictionary, Intelligence, and
  System pages for transcription, vocabulary, recording, and pill preferences.
- Small live transcript pill alongside the recording waveform.
- Ctrl+Win as the default hold-to-talk shortcut on new Windows configurations;
  existing saved shortcuts and the three legacy presets remain supported.
- App, tray, and Windows icons derived from the original `icon.png`.
- Original generated Windows recording start/stop cues and optional audio
  ducking with restoration.
- Bounded Windows caret-context support for optional name hints, local Auto
  Dictionary learning, and context-aware text insertion.
- `utterly --settings` opens the native Windows Settings window at launch.
- Windows DPAPI protection for the saved API key.
- Optional take history (Settings → System → "Save audio and transcripts"):
  each take's enhanced microphone audio (`audio.wav`) and final transcript
  (`transcript.txt`) are stored in a timestamped folder under the app data
  directory. Storage stays local and period-bounded, and the feature is off
  by default.
- History page in the Windows Settings window: keep-history dropdown (day,
  week, month, or year old — changing it purges nothing until you confirm),
  storage maintenance with confirmation dialogs (delete older history,
  delete all, open the history folder), and a take list showing each take's
  transcript preview with hover-revealed play/pause and delete buttons plus
  a re-run-transcription action that rewrites the saved transcript from the
  stored audio. The tray menu carries the same retention and purge actions
  on every platform.
- Local operational logging for diagnosing transcription accuracy: day-stamped
  files under the app data `logs` folder (a week retained, automatically
  pruned), a per-take summary line with duration, chunks sent/silent/dropped,
  peak level, and result size, plus connect/retry/mic-watchdog/history
  events. Content stays private — transcripts and keys are never written to
  logs. `utterly --show-logs` opens the folder; `UTTERLY_LOG=debug` raises
  verbosity.

### Changed

- Replaced the earlier large floating pill with a compact, always-on-top
  native capsule, live transcript surface, and tiny idle handle.
- Position the pill above the active display's taskbar by default.
- `--set-key` now reads from the clipboard instead of taking a secret argument.
- Focused-app icon and idle handle can be disabled in Settings.
- "Mute notifications" suppresses Utterly's temporary pill notices only.
- On Windows, final text always stays on the clipboard; automatic paste
  requires the same editable target captured at recording start.
- Windows context options read at most 160 characters on either side of the
  caret, skip password/read-only controls, and do not scan the rest of the
  screen. Context Awareness can send extracted names as session vocabulary;
  Auto Dictionary stores candidates locally, and saved vocabulary is sent with
  future Gemini sessions.
- The former 5 MB package, 3 MB auxiliary size goal, and 20 MiB private-memory
  budgets are fully removed — no size or RAM limit is enforced anywhere. The
  Linux release dropped its UPX pass and 5 MB CI size gate, shipping the same
  raw optimized binary as every other platform; package size stays an
  informational measurement only.

### Fixed

- Wait for Gemini's `setupComplete` response and disable automatic activity
  detection before starting a manual push-to-talk turn.
- Report failed pastes and always release the simulated Ctrl/Cmd modifier.

See the [Windows QA report](docs/qa/willow-native-2026-09-24.md) for
implementation and verification status.

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
