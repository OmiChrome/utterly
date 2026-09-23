# Windows 11 acceptance notes

Run on Windows 11 build 10.0.26100 at 120 DPI with the optimized x64 build.
The code gates and resource measurements are reproducible with the commands
and script in the [Windows 11 benchmark report](../benchmarks/windows-11-2026-09-23.md).

## Verified

- `cargo fmt --all`
- `cargo test --all-targets`: 32 passed, 0 failed.
- `cargo clippy --all-targets -- -D warnings`: passed.
- `cargo build --release`: passed for the local Windows target.
- The 252 × 48 logical-pixel pill appeared at 315 × 60 physical pixels.
  The 414 × 500 Settings window was shown in the QA run; its native caption
  and close button worked. Close hid it, and the app command reopened it. The
  pill's settings hit-area tests passed.
- Mode and keybind window commands persisted to a disposable test profile.
- The pill and Settings caption both followed a six-second native drag path;
  CPU and final displacement are recorded in the benchmark report.
- Dictionary validation covers normalized whitespace, empty/oversized input,
  case-insensitive duplicates, add/remove, and the 1,000-entry cap. The setup
  JSON test confirms the phrases are sent as JSON strings and omitted when the
  dictionary is empty.
- A Live API setup using two test phrases reached the normal no-speech result
  after release. No PCM audio frames were sent, and the key remained protected
  with Windows DPAPI throughout the check.
- A separate speech-to-paste smoke test used Google's public
  [`hello_are_you_there.pcm` sample](https://storage.googleapis.com/generativeai-downloads/data/hello_are_you_there.pcm)
  through Stereo Mix. Gemini returned “Hey, can you hear me?” and the result
  appeared in the temporary Notepad text area. No personal voice was recorded.

## Not covered by this run

The live audio smoke test used a public PCM sample routed through Stereo Mix,
not a physical microphone or a user's voice. Active-recording CPU and memory
were not sampled. The benchmark uses a placeholder key and never calls Gemini.
