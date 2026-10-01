# Willow-inspired native Utterly implementation plan

**Goal:** Recreate the supplied Willow interaction and visual language in dark native Rust, using the user's `icon.png` and preserving live Gemini transcription.

**Architecture:** Keep the existing Rust/winit/softbuffer pill, Win32 settings, cpal audio and blocking WebSocket session. Use native Windows drawing, focus inspection, app icons and audio APIs. No browser, Electron, HTML or web UI runtime.

## Design and constraints

- Charcoal backgrounds, subtly lighter rounded cards, quiet borders, white text and violet accents. Use measured Willow font information where available, with a native installed-font fallback.
- Idle: 32 x 6 logical-pixel centered handle (about 40 x 8 physical pixels at 125% scale) above the active monitor's taskbar. Recording: rounded black capsule with focused-app icon, responsive white waveform and a readable live transcript surface. Release collapses the recording presentation immediately while finalization continues.
- Hold Ctrl+Win to record; releasing either modifier finishes. Preserve alternate shortcuts and existing config. New installations default to Ctrl+Win.
- Clipboard receives the final text even when there is no editable target. Paste only into the captured, still-valid editable target. Never insert into a newly focused unrelated app.
- Settings: General, Dictionary, System and Intelligence pages; working interaction sounds, audio ducking/restoration, notification muting, context awareness, auto dictionary, smart insertion, idle bubble and focused-app icon controls. Existing mic, key, mode and vocabulary controls remain reachable.
- Context features inspect limited focused-control text, exclude password fields and only run when enabled. Auto dictionary learns bounded candidate names into the existing local dictionary; no whole-screen scrape.
- Prior installer/RAM hard limits are removed: no size cap or memory budget is enforced anywhere, and release CI treats package size as an informational measurement only. Resource use is still measured, and idle redraws and animation stay bounded so the multi-platform GUI remains fast and lean.
- Existing modified source is user work: preserve it. Research assets are references; use Utterly branding in the shipped interface.

## Tasks and ownership

1. **Research and references (research worker):** Inspect public Willow website/help material and installed resources read-only. Copy all supplied screenshots into `docs/references/`, inspect video keyframes, extract available image/font/sound assets into a reference-only directory, save manifest/provenance and exact findings in `docs/willow-research.md`. Separate measured facts from inference; do not inspect account secrets or user data.
2. **Pill (UI worker, `src/ui.rs`):** Native compact idle handle, expanding capsule, microphone-driven waveform, transcript, focused app icon, processing animation, work-area/DPI positioning and reduced-motion consideration. Own rendering and animation tests. Consume existing PillUpdate; preferences arrive through a new `UiServices.preferences_rx: Receiver<config::Preferences>`. Use `output::focused_app_icon() -> Option<Vec<u8>>` (32x32 RGBA).
3. **Settings (settings worker, `src/settings.rs`):** Dark native sidebar/card interface with responsive layout, keyboard-operable controls and page navigation. Extend Snapshot with `preferences: config::Preferences`, `mic: String`, `has_api_key: bool`. Emit `MenuCmd::Preferences(config::Preferences)` plus existing commands. No fake controls.
4. **Core integration (primary agent, config/main/tray/hotkey/output/system):** Add persisted Preferences with fields `interaction_sounds`, `duck_audio`, `mute_notifications`, `context_awareness`, `auto_dictionary`, `smart_insertion`, `automatic_positioning`, `show_idle_bar`, `hide_app_icon`. Defaults: sounds/smart insertion/positioning/idle on, context/learning/ducking/mute/hide icon off. Implement modifier-only hold shortcut, safe insertion/clipboard fallback, context and audio effects. Wire all settings to runtime. Generate icon derivatives from icon.png.
5. **Verification:** Run existing baseline then focused tests for config migration, shortcut transitions, transcript revision/finalization, smart spacing, target-change fallback, UI geometry and audio restoration. Run cargo fmt/check/test/clippy and release build. Inspect real rendered settings/pill; exercise native controls and hotkey where tooling permits. Record measured executable size, RAM and CPU, and explicitly distinguish real service tests from simulations.

## Review focus

- Release Ctrl first versus Win first; repeat presses; Start-menu suppression; no stuck modifiers.
- Target app changed/closed, no text focus, password focus, clipboard unavailable.
- Final response delayed or disconnected after release; latest live text must survive.
- Mixed DPI, taskbar work area, small settings window, Unicode/long transcript.
- Every stop/error path restores ducked audio; disabled context features collect no text.

## Progress

- [x] Inspect repository and preserve pre-existing changes.
- [x] Establish design, interfaces and worker boundaries before delegation.
- [x] Research/assets and measured design report.
- [x] Native pill implementation.
- [x] Native settings implementation.
- [x] Core behavior integration.
- [x] Final review, automated checks, rebuilt visual verification, and evidence
  report.

Implementation and QA are complete. Physical Ctrl+Win hold/release, mixed-DPI
multi-monitor movement, and broad compatibility across third-party editable
controls remain unexercised; see the [QA report](qa/willow-native-2026-09-24.md).

Ruling: Proceed through research, implementation and testing under the user's explicit plan-then-delegate instruction; no additional design approval cycle is needed. Work in the current checkout to preserve and integrate the existing uncommitted implementation.
