# Utterly

Minimal push-to-talk dictation. Hold **Ctrl+Space**, speak, release — your
words are transcribed and pasted into the focused app.

## Quick start

1. Grab a free API key from **Google AI Studio**: https://aistudio.google.com/apikey
2. Plug it into the app: right-click the tray icon → **Paste API key from
   clipboard** (copy the key first), or run `utterly --set-key YOUR_KEY`.
3. Click any text field, hold **Ctrl+Space**, speak, release. Done.

First-run notes: on macOS, right-click → Open the app once (unsigned build),
then grant Microphone + Accessibility. On Windows, allow microphone access.
On Linux, a system tray (AppIndicator) is needed for the menu.

## Settings

Everything lives in the tray-icon menu — no separate settings window:

- **Microphone** — system default or any input device
- **Hold-to-talk hotkey** — Ctrl+Space, Alt+Space, or Ctrl+Shift+Space
- **Transcription mode** — Smart or Verbatim (applies to the next utterance)
- **Paste API key from clipboard** — paste a key copied from AI Studio
- **Quit**

The same options exist as CLI flags (`--list-mics`, `--set-mic`,
`--set-hotkey`, `--set-mode`, `--set-key`). Config is stored as JSON with
restricted permissions (`~/.config/utterly/`, `%APPDATA%\Utterly` on Windows).

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
- Free tier: the Gemini API free tier covers personal dictation (rate-limited).
  Paid usage is usage-based at roughly $0.009 per minute of transcription —
  no subscription. Note the tradeoff vs fully-offline tools: audio is sent to
  Google for transcription (free-tier content may be used to improve products;
  paid-tier content is not).

## Benchmarks

Measured on Linux x86_64 (release build, idle pill with live mic + tray):

| | Utterly | Local-Whisper dictation apps | Wispr Flow (commercial) |
|---|---|---|---|
| Installer | **1.3 MB** (UPX; 3.4 MB raw) | 150–600 MB model downloads plus runtimes | store download |
| RAM idle | **~4 MB** resident | gigabytes (models resident in memory) | 750 MB minimum (store listing) |
| CPU idle | **~0–1%** | model inference on CPU/GPU | — |
| Price | free tier + ~$0.009/min after | free offline | $12/mo |

Reference points (public listings): Whisper Flow iOS app is 521 MB on the
App Store; open-source Whisper dictation clones download ~150 MB (base) to
~600 MB (Parakeet) models on first run; Wispr Flow's store page lists 750 MB
minimum memory and a $12/mo plan. Utterly trades offline use for size: it
needs internet and an API key, and in return ships no weights and idles at a
few megabytes.

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

Idle pill (grey) and listening pill (red):

![Utterly idle pill](assets/screenshots/pill-idle.png)
![Utterly listening pill](assets/screenshots/pill-listening.png)
