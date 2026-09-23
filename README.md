# Utterly

![Utterly microphone app icon](assets/utterly-app-icon.png)

Minimal push-to-talk dictation. Hold **Alt+Space**, speak, release — your
words are transcribed and pasted into the focused app.

## Quick start

1. Grab a free API key from **Google AI Studio**: https://aistudio.google.com/apikey
2. Copy it, then right-click the tray icon → **Paste API key from clipboard**,
   or run `utterly --set-key`.
3. Click any text field, hold **Alt+Space**, speak, release. Done.

The compact pill sits above your other windows (always-on-top, no taskbar
button). Drag its body to reposition it (position is remembered); click the
mic dot to toggle listening, or hover the top-right for the tiny close
control to hide the pill to the tray. Settings live in the tray menu only.

First-run notes: on macOS, right-click → Open the app once (unsigned build),
then grant Microphone + Accessibility. On Windows, allow microphone access.
On Linux, a system tray (AppIndicator) is needed for the menu.

## Settings

On Windows, choose **Settings** from the tray
menu. The small native window has a normal title bar, close button, and
draggable caption. It contains:

- **Transcription style** — Smart cleans up and formats; Verbatim keeps the
  spoken words. The choice applies to the next utterance.
- **Keybind** — Alt+Space, Ctrl+Space, or Ctrl+Shift+Space.
- **Personal dictionary** — add or remove up to 1,000 words or phrases. They
  are sent with the next Gemini session; Google's guidance says best results
  typically use 100 or fewer entries ([Gemini Live transcription docs](https://ai.google.dev/gemini-api/docs/live-api/live-transcribe)).

The tray menu also contains microphone selection, mode and hotkey shortcuts,
API-key paste, and Quit. macOS and Linux keep mode and hotkey choices in the
tray menu. Other settings have CLI flags (`--list-mics`, `--set-mic`,
`--set-hotkey`, `--set-mode`, `--set-key`); copy the key before running
`--set-key` because command-line arguments can expose secrets to other
processes.

Config is stored as JSON (`~/.config/utterly/`, `%APPDATA%\Utterly` on
Windows). The key is protected with Windows DPAPI and file permissions are
restricted to the current user on Unix.

Smart mode removes ums and ahs, fixes self-corrections
("Tuesday—no, Wednesday") and formats the text. Verbatim returns exact words.

## Why this model

Utterly streams microphone audio to **Gemini 3.5 Transcribe Live**
(`gemini-3.5-transcribe-live`) over a WebSocket and receives text back as you
speak, with a cleaned-up final transcript about a second after release
(measured in loopback tests).

- Built for live speech-to-text: low-latency streaming, 85+ languages with
  automatic detection, custom vocabulary for names and jargon.
- No model downloads, no ML runtime in the app — that is the whole reason the
  installer is megabytes, not gigabytes.
- Usage depends on your current Google AI Studio quota and billing terms. Audio
  is sent to Google for transcription; check the terms for your account tier.

## Size and performance

Measured on Windows 11 (release build, 120 DPI): the executable is 1.46 MiB,
and the portable ZIP with the app icon is 0.95 MiB. The compact pill is
252 × 48 logical pixels. Idle CPU was 0.21% of one core over 30 seconds, with
4.60 MiB private memory; opening Settings measured 0.62% CPU and 4.89 MiB
private memory. Windows working set, which includes shared system pages,
peaked at 25.10 MiB with Settings open. See the [full Windows 11 benchmark](docs/benchmarks/windows-11-2026-09-23.md)
and rerun it with `scripts/benchmark-windows.ps1`.

The Windows release is a portable ZIP, not an installer. Its measured size is
below the 5 MB allowance. Utterly streams audio only while dictating; the
benchmark's idle measurements do not include an active transcription stream.

## Build from source

Requires Rust stable plus system mic/tray libraries (Linux:
`libasound2-dev libxkbcommon-dev libgtk-3-dev libayatana-appindicator3-dev`).

```sh
cargo test            # unit tests
cargo build --release # -> target/release/utterly
```

Windows and macOS build from the same tree (`winit`, `cpal`, tray, hotkeys
and clipboard all have native backends; Linux-only code is `cfg`-gated).
Tagged `v*` pushes build all four binaries automatically via GitHub Actions
(see `.github/workflows/`).

## Screenshots

Idle pill (grey), listening pill (red), transcribing pill (green):

![Utterly idle pill](assets/screenshots/pill-idle.png)
![Utterly listening pill](assets/screenshots/pill-listening.png)
![Utterly transcribing pill](assets/screenshots/pill-transcribing.png)

The generated Windows icon is included as `assets/utterly.ico` in the release
ZIP for shortcuts and file associations.
