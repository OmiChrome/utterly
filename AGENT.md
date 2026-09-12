# AGENT.md — working notes for Utterly

Utterly is a minimal push-to-talk dictation pill. Hold Ctrl+Space, speak,
release: mic audio streams to Gemini 3.5 Transcribe Live and the transcript
is pasted into the focused app. Rust, one binary, no async runtime.

## Stack

Rust 2021. Direct dependencies only, each earning its place:

- `cpal` — mic capture (16 kHz mono request, resampled in-callback)
- `tungstenite` + `rustls-tls-webpki-roots` — Live API WebSocket, blocking I/O
- `winit` + `softbuffer` — borderless always-on-top pill, CPU framebuffer
- `tray-icon` / `muda` — tray icon + settings menu (no new dep for menus)
- `global-hotkey` — system-wide push-to-talk press/release
- `arboard`, `enigo` — clipboard + synthetic Ctrl/Cmd+V paste
- `serde` / `serde_json`, `base64` — config + wire format
- `gtk` — Linux-only (tray menus need init + pump there; never linked elsewhere)
- `raw-window-handle` — names softbuffer's generic Surface type

No tokio, no GUI framework, no font engine (text goes through the OS window
title), no image crate (tray icons are procedural RGBA).

## Constraints (hard budgets, not aspirations)

Floor: 5-year-old dual-core laptop, 4 GB RAM, integrated audio.
Primary budgets are user-felt (latency, cost, battery). Size/CPU/RAM
are guards that protect those, not vanity metrics.

- Latency: cold start <300 ms, press to interim 0.5-1.5 s typical,
  release to paste ~1 s (grace early-exits on close or 500 ms quiet
  after first final, 4000 ms hard cap; setup verify 400 ms, not 3000).
- Cost: silence gate stays (skip base64 + TLS below SILENCE_RMS),
  100 ms chunks (10 msgs/s), never stream pure silence. Users pay
  per minute; quiet rooms should cost near zero.
- Battery/idle: 0.0-0.3% idle, <5% of one core while streaming,
  <50 wakeups/s idle (10 ms recording tick, 50 ms idle tick, pill
  capped at 30 fps, dirty-rect only).
- Installer <5 MB raw stripped, <2 MB Linux UPX: `opt-level="z"`,
  `lto`, `codegen-units=1`, `strip`, `panic="abort"`, UPX Linux-only
  (3.4 MB -> 1.3 MB). Windows/macOS ship raw: UPX trips Defender
  heuristics and breaks Gatekeeper/notarization.
- RAM <20 MB RSS idle, <50 MB peak streaming: fixed 160k-sample ring
  (320 KiB), 380x64x4 framebuffer (95 KiB), 8 KiB socket frames, small
  fixed thread stacks. Measured: 3.5-4.0 MB RSS steady, heap ~300 KiB.

## Size, memory, CPU: techniques that actually moved the needle

Installer size (<5 MB raw, ~1.3 MB Linux UPX):

- Release profile does the heavy lifting: `opt-level="z"` (size over speed),
  `lto = true`, `codegen-units = 1`, `strip = true`, `panic = "abort"`.
  Each flag is load-bearing; removing LTO alone costs hundreds of KiB.
- The biggest wins are dependencies NOT taken: no tokio (an async runtime is
  megabytes by itself), no GUI framework (hand-rolled winit + softbuffer),
  no font engine (text renders through the OS window title). `arboard`
  ships with `default-features = false` (text-only clipboard): drops the
  `image/png/moxcms` stack (~300-600 KiB, killed the png 0.17 + 0.18 dupe).
  Tray icons stay 16x16 procedural RGBA, 1 KiB each.
- Platform-only deps behind `cfg` so other targets never link them
  (`gtk` is Linux-only).
- UPX `--best --lzma` Linux-only. Tradeoff: slower cold start (decompress
  to RAM on every launch, breaks demand paging); worth it on Linux under
  a 5 MB budget, forbidden on Windows (Defender flags UPX stubs) and macOS
  (breaks Hardened Runtime/notarization). CI gates Linux raw <5 MB and
  publishes sha256 + zips for Win/mac.
- Measure per platform, not once: macOS/Windows link different system code
  and came out under half the Linux size.

Memory footprint (~4 MB RSS steady, ~300 KiB heap):

- Allocate fixed buffers once at startup and never grow them: 160k-sample
  ring (320 KiB), 380x64x4 framebuffer (95 KiB), 100 ms PCM chunks.
- Overwrite-oldest ring: bounded by construction, no allocator pressure,
  no backlog that can OOM a long session.
- Zero hot-loop allocations: reuse the resample/output Vecs, build audio
  JSON by hand (serde stays out of the 10 Hz path), drain in place with
  `copy_from_slice`, take chunks by value from a stack array.
- Small fixed thread stacks (256 KiB for menu/hotkey threads, 512 KiB for
  the transient connect thread) instead of default 2-8 MB stacks.
- Bounded queues by design, not by hope: ready-signals stay ~1:1 with
  flushes (100 max); mpsc channels are unbounded in type but bounded by
  their producers' fixed rates.
- Verify with `/proc/PID/status` (VmRSS/VmHWM) and `smaps_rollup`, never
  `ps` on a wrapper PID: measuring the `timeout` supervisor instead of the
  app once reported 2.0 MB that was not ours.

CPU usage (0.0-0.3% idle, <5% one core streaming, <50 wakeups/s idle):

- Blocking I/O with timeouts everywhere; never spin on a channel or socket.
  Session tick is 10 ms recording / 50 ms idle (`tick_interval`), 100 ms
  meter gate, event loop parked on `WaitUntil` with a 33 ms cap.
  `recv_raw` returns None on WouldBlock/TimedOut (so setup Timeout is real);
  `recv_timeout` maps WS Close / ConnectionClosed to `closed = true` (so the
  grace loop actually early-exits instead of spinning to the cap).
- Render only on change (dirty flag): idle pill issues zero presents.
- Silence gate before the expensive path: quiet frames skip base64 + TLS
  writes, roughly halving network and crypto CPU in normal rooms. Gate
  threshold and meter threshold stay split (send at 120 RMS, animate at 200).
- Hot-loop alloc reuse: `realtime_audio_json_into` writes into a reused
  thread-local String (no ~4.3 KiB fresh alloc per 100 ms chunk at 10/s);
  wire bytes stay identical (round-trip test guards this).
- O(1) state instead of history: attack/release meter is one float, level
  is computed once per chunk and reused for meter, gate, and UI.
- Prefer integer math and `memcpy` in audio paths (linear-interp resample,
  two-segment ring drain); no FFTs, no per-sample branching.

Latency and cost (the budgets users feel):

- Grace drain early-exits on server close or 500 ms quiet after the first
  final (`should_stop_grace`, pure + tested); 4000 ms is a hard cap, not
  the common path. Release to paste is ~1 s, not ~4.1 s.
- Setup verify is 400 ms on the press path (`classify_setup_response`,
  pure + tested), not 3000 ms. Always proceed on timeout; log unexpected
  closes loudly with code + reason.
- Keep frames at 100 ms PCM (10 msgs/s): smaller chunks cut first-text a
  little but multiply TLS + JSON + syscall overhead and server cost.
- Future risks to watch: PipeWire/ALSA buffer-size mismatch (glitch without
  xrun), Wayland vs X11 hotkey/paste backends, enterprise TLS proxies
  (prefer OS native roots if webpki bundle causes failures), AV/Gatekeeper
  on packed binaries (reason Win/mac ship raw).

General practice: write the budget table first (bytes per buffer, msgs per
second, wakeups per second), then verify each number by sampling. When a
measurement surprises, distrust the harness before the code: most "app"
anomalies in this project traced to test tooling (stale processes holding
the hotkey grab, synthesizers that cannot hold keys, silent loopback audio
tripping the watchdog by design).

## Code style

Pragmatic Rust in the spirit of Linus + DHH. Minimal abstractions, clean
data structures, no unnecessary complexity.

- No em dashes in comments.
- Flat structs over deep nesting. State should be obvious at a glance.
- No premature abstraction. Three similar lines beat a helper used once.
- Early returns over deep if/else chains.
- `let _ =` for intentionally ignored results, with a short reason comment.
- Fixed buffers with named capacity constants (`RING_CAP`, `CHUNK`).
- Never silently discard data: chunker returns full frames only when buffered
  and never eats short reads; failed sends and setup rejections are logged.
- Pure functions for wire formats so tests can assert exact bytes
  (`setup_json`, `realtime_audio_json` round-trip test).
- Tests read as specifications: ring overwrite order, resampler continuity
  across callbacks, preset normalization, server parse incl. garbage input.
- Prefer `std` over new deps. Every dependency must justify its kilobytes.

## Architecture

- `main.rs` — CLI flags, single-instance lock, session state machine
  (press -> stream -> release -> commit), menu command handling, watchdog
- `audio.rs` — capture, ring buffer, resample, RMS gate, device picking
- `transcribe.rs` — Live API client: setup JSON, PCM frames, event parse
- `hotkey.rs` — press/release presets via global-hotkey
- `ui.rs` — pill window (winit event loop owns the main thread)
- `tray.rs` — tray icon + menu; items are !Send/!Sync so they live on main
- `output.rs` — clipboard + synthetic paste commit
- `config.rs` — JSON config (XDG / %APPDATA% / ~/.config), 0600 on unix

Cross-thread rules: UI updates flow session -> pill channel; menu ids cross
as plain `(u32, MenuCmd)` tables; checkmarks are applied on main only.

## Protocol facts (verified against the live server, Sep 2026)

- Setup: model + `generationConfig.responseModalities:[TEXT]` +
  `inputAudioTranscription{languageCodes, mode}`; SMART accepted.
- Turn markers nest inside realtimeInput (`activityStart`/`activityEnd`).
  A top-level activityStart gets the session closed with
  `Unknown name "activityStart"`. Always read close frames (code + reason);
  `recv_raw` exists for exactly this.
- Minimal setup is what transcribes; extra VAD config objects left sessions
  mute in testing. Manual bracketing works with server defaults.
- tungstenite has no connect/read timeouts: bound connect with a helper
  thread (10 s) and set read timeouts on the inner TcpStream (both Plain
  and Rustls variants).
- Robotic TTS streamed fine but yielded zero transcript across many runs;
  human speech transcribed first try. Test transcription with real speech
  (Google's `hello_are_you_there.pcm` sample is the reference).
- Finals arrive after the turn end; keep a few seconds of grace drain.

## Build and test

```sh
cargo check          # zero warnings (also: --target x86_64-pc-windows-gnu)
cargo clippy --all-targets -- -D warnings
cargo test           # 11 unit tests
cargo build --release
```

Headless end-to-end (needs Xvfb + mic loopback, Linux dev box):

```sh
Xvfb :99 -screen 0 1280x800x24 &
pactl load-module module-null-sink sink_name=tts_sink
pactl set-default-source tts_sink.monitor
DISPLAY=:99 ./target/debug/utterly &          # one instance only (lock refuses doubles)
espeak-ng --stdout "..." | paplay --device=tts_sink &
# hold Ctrl+Space 8 s, then read the log for the final transcript
```

Gotchas learned the hard way:

- One instance at a time: a second copy silently loses the X11 grab race.
  The lock refuses loudly; never test with overlapping runs.
- No CLI tool holds Space reliably (xdotool/xte `keydown` on plain keys is
  a no-op or flaky). Use raw XTEST (see the hold helper pattern) and verify
  delivery with xev running alongside the app (grab consumes events, so xev
  going quiet proves the app owns the grab).
- PipeWire null-sink loopback silence is bit-exact zero, which trips the
  stale-stream watchdog by design; real mics always have a noise floor.
- Keep shell commands short; use absolute paths; never `pkill -f` a pattern
  that matches your own command line (bracket trick: `utterl[y]`).

## Releasing a new version

Version lives in `Cargo.toml`. Tag-driven releases via `.github/workflows/`:

1. Update `CHANGELOG.md` if present, else note changes in the release body.
2. `git tag -a vX.Y.Z -m "..." && git push origin main vX.Y.Z`
3. `release.yml` builds Windows x64, Linux x64, macOS ARM64 + x64 and
   publishes them to the GitHub Release. `ci.yml` gates pushes/PRs
   (fmt, clippy -D warnings, tests, Windows compile check).
4. Never move a published tag. Unpublished tags may be deleted + recreated.

macOS builds are unsigned: users right-click > Open once, then grant
Microphone + Accessibility. Windows needs microphone permission on first run.
