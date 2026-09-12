//! Audio capture: fixed ring buffer + cpal, resampled to 16 kHz mono i16.
//!
//! RAM budget: RING_CAP = 160_000 samples (10 s @16kHz) = 320 KiB, allocated
//! ONCE at startup. Hot loop does O(chunk) work with zero allocation:
//! linear-interp resample (integer math), RMS energy gate, 100 ms chunking.

use std::sync::{
    mpsc::{self, Receiver, Sender},
    Arc, Mutex,
};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// 10 seconds of 16 kHz mono. Overwrite-oldest => never grows, never allocs.
pub const RING_CAP: usize = 160_000;
/// 100 ms chunks balance Live API latency vs syscall count (10 msgs/sec).
pub const CHUNK: usize = 1_600;
/// Silence gate: skip frames below this RMS so we don't stream pure silence.
pub const SILENCE_RMS: f32 = 120.0;

/// Lock-free-ish SPSC ring: single producer (audio callback), single consumer.
/// Overwrite-oldest on overflow — dictation cares about the last 10 s, not the first.
pub struct Ring {
    buf: Box<[i16]>,
    write: usize,
    len: usize,
}

impl Ring {
    pub fn new() -> Self {
        Self {
            buf: vec![0i16; RING_CAP].into_boxed_slice(),
            write: 0,
            len: 0,
        }
    }

    #[inline]
    pub fn push(&mut self, s: i16) {
        self.buf[self.write] = s;
        self.write = (self.write + 1) % RING_CAP;
        if self.len < RING_CAP {
            self.len += 1;
        }
    }

    #[inline]
    pub fn push_slice(&mut self, xs: &[i16]) {
        for &s in xs {
            self.push(s);
        }
    }

    /// Drain up to `out.len()` oldest samples. Returns count drained.
    /// Two `memcpy` segments (the ring may wrap) — no per-sample loop.
    pub fn drain(&mut self, out: &mut [i16]) -> usize {
        let n = out.len().min(self.len);
        let start = (self.write + RING_CAP - self.len) % RING_CAP;
        let first = (RING_CAP - start).min(n);
        out[..first].copy_from_slice(&self.buf[start..start + first]);
        out[first..n].copy_from_slice(&self.buf[..n - first]);
        self.len -= n;
        n
    }

    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Buffered sample count (for status telemetry).
    pub fn len(&self) -> usize {
        self.len
    }
}

/// RMS energy of a chunk — O(n), no alloc. Used for silence gate + UI meter.
pub fn rms(chunk: &[i16]) -> f32 {
    if chunk.is_empty() {
        return 0.0;
    }
    let mut acc: f64 = 0.0;
    for &s in chunk {
        let v = s as f64;
        acc += v * v;
    }
    (acc / chunk.len() as f64).sqrt() as f32
}

/// Pick a capture config: prefer mono + 16 kHz (often step=1.0, no resample
/// surprises), format I16 > F32 > U16. The device default (e.g. F32/48kHz
/// stereo on ALSA-pulse bridges) has been observed to deliver pure zeros on
/// some servers, while an explicit 16k/mono request flows fine.
fn pick_input_config(device: &cpal::Device) -> Option<cpal::SupportedStreamConfig> {
    use cpal::SampleFormat;
    let mut best: Option<(u32, cpal::SupportedStreamConfig)> = None;
    let fmts = [SampleFormat::I16, SampleFormat::F32, SampleFormat::U16];
    if let Ok(ranges) = device.supported_input_configs() {
        for range in ranges {
            let Some(fmt_idx) = fmts.iter().position(|f| *f == range.sample_format()) else {
                continue; // exotic format (S32/U8/…): skip, don't abort probing
            };
            let fmt_rank = fmt_idx as u32;
            // Skip absurd channel counts (keeps the mono mix trivial).
            if range.channels() == 0 || range.channels() > 8 {
                continue;
            }
            let rate = 16_000u32.clamp(range.min_sample_rate().0, range.max_sample_rate().0);
            let Some(cfg) = range.try_with_sample_rate(cpal::SampleRate(rate)) else {
                continue;
            };
            // Lower score wins: mono first, then closest rate to 16k, then format.
            let rate_penalty = rate.abs_diff(16_000) / 1000;
            let ch_penalty = (range.channels() as u32).saturating_sub(1) * 10_000;
            let score = ch_penalty + rate_penalty * 10 + fmt_rank;
            if best.as_ref().map(|(s, _)| score < *s).unwrap_or(true) {
                best = Some((score, cfg));
            }
        }
    }
    if let Some((_, cfg)) = best {
        return Some(cfg);
    }
    device.default_input_config().ok()
}

/// List input device names for the "choose mic" dropdown.
pub fn list_mics() -> Vec<String> {
    let host = cpal::default_host();
    let mut names = Vec::new();
    if let Ok(devs) = host.input_devices() {
        for d in devs {
            if let Ok(n) = d.name() {
                names.push(n);
            }
        }
    }
    names
}

pub struct Capture {
    _stream: cpal::Stream,
    /// Shared ring; audio thread pushes, dictation thread drains CHUNK at a time.
    pub ring: Arc<Mutex<Ring>>,
    /// 100 ms ready-chunks signaled here (carries RMS for the UI meter).
    pub ready_rx: Receiver<f32>,
    _ready_tx: Sender<f32>,
    /// What we opened, for diagnostics (e.g. watchdog reopen log).
    pub fmt_desc: String,
}

impl Capture {
    /// Open mic whose name contains `mic_want` (or default). Prefers an
    /// explicit 16 kHz mono config so the resampler usually runs at step 1.0.
    pub fn open(mic_want: &str) -> Result<Self, String> {
        let host = cpal::default_host();
        let device = if mic_want.is_empty() {
            host.default_input_device()
        } else {
            host.input_devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.name().map(|n| n.contains(mic_want)).unwrap_or(false))
                .or_else(|| host.default_input_device())
        }
        .ok_or_else(|| {
            "no input device found (check mic permission in system settings)".to_string()
        })?;

        let supported =
            pick_input_config(&device).ok_or_else(|| "no usable input config".to_string())?;
        eprintln!(
            "[utterly] mic: {:?} {}Hz {}ch",
            supported.sample_format(),
            supported.sample_rate().0,
            supported.channels()
        );
        let src_rate = supported.sample_rate().0;
        let channels = supported.channels() as usize;
        let dst_rate: u32 = 16_000;

        let ring = Arc::new(Mutex::new(Ring::new()));
        let (tx, rx) = mpsc::channel::<f32>();
        // Resample state lives INSIDE each match arm (each closure moves its own
        // copy): fractional position + pending output. Only one arm runs.
        let fmt = supported.sample_format();
        let cfg: cpal::StreamConfig = supported.config();
        // NOTE: do NOT force BufferSize::Fixed here. An explicit small buffer
        // was tried and produced ~1–2 s of audio followed by zeros forever on
        // ALSA-pulse bridges (arecord/parec unaffected); the device default
        // streams continuously (verified: hundreds of loud chunks per run).
        let step: f64 = src_rate as f64 / dst_rate as f64;

        let err_fn = |e| eprintln!("[utterly] audio error: {e}");
        let stream = match fmt {
            cpal::SampleFormat::F32 => {
                let ring_cb = ring.clone();
                let tx = tx.clone();
                let mut frac: f64 = 0.0;
                let mut out: Vec<i16> = Vec::with_capacity(CHUNK * 2);
                device.build_input_stream(
                    &cfg,
                    move |data: &[f32], _| {
                        push_resampled_f32(
                            data, channels, step, &mut frac, &mut out, &ring_cb, &tx,
                        );
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::I16 => {
                let ring_cb = ring.clone();
                let tx = tx.clone();
                let mut frac: f64 = 0.0;
                let mut out: Vec<i16> = Vec::with_capacity(CHUNK * 2);
                device.build_input_stream(
                    &cfg,
                    move |data: &[i16], _| {
                        push_resampled_i16(
                            data, channels, step, &mut frac, &mut out, &ring_cb, &tx,
                        );
                    },
                    err_fn,
                    None,
                )
            }
            cpal::SampleFormat::U16 => {
                let ring_cb = ring.clone();
                let tx = tx.clone();
                let mut frac: f64 = 0.0;
                let mut out: Vec<i16> = Vec::with_capacity(CHUNK * 2);
                device.build_input_stream(
                    &cfg,
                    move |data: &[u16], _| {
                        push_resampled_u16(
                            data, channels, step, &mut frac, &mut out, &ring_cb, &tx,
                        );
                    },
                    err_fn,
                    None,
                )
            }
            f => return Err(format!("unsupported sample format: {f:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;

        Ok(Self {
            _stream: stream,
            ring,
            ready_rx: rx,
            _ready_tx: tx.clone(),
            fmt_desc: format!("{:?} {}Hz {}ch", fmt, src_rate, channels),
        })
    }

    /// Non-blocking drain of one 100 ms chunk if available.
    /// Take one full 100 ms chunk if available. Returns None WITHOUT consuming
    /// anything when fewer than CHUNK samples are buffered: discarding
    /// partials here was silently losing ~all audio whenever capture ran
    /// slightly below consumption rate (caught live-testing: chunks_sent≈0
    /// despite a loud mic). Signals stay ~1:1 with flushes so the queue
    /// cannot grow unboundedly (≤ ring_cap/CHUNK entries).
    pub fn take_chunk(&mut self) -> Option<([i16; CHUNK], f32)> {
        let rlen = self.ring.lock().map(|r| r.len()).unwrap_or(usize::MAX);
        if rlen < CHUNK {
            return None;
        }
        // One signal per flush; if none is queued yet, retry next tick
        // (samples stay buffered — nothing is lost).
        if self.ready_rx.try_recv().is_err() {
            return None;
        }
        let mut chunk = [0i16; CHUNK];
        let mut ring = self.ring.lock().ok()?;
        let mut got = 0usize;
        while got < CHUNK {
            let n = ring.drain(&mut chunk[got..]);
            if n == 0 {
                break; // single consumer: unreachable; defensive
            }
            got += n;
        }
        if got < CHUNK {
            return None;
        }
        Some((chunk, rms(&chunk)))
    }
}

#[inline]
fn flush_out(out: &mut Vec<i16>, ring: &Arc<Mutex<Ring>>, tx: &Sender<f32>) {
    while out.len() >= CHUNK {
        let level = rms(&out[..CHUNK]);
        if let Ok(mut r) = ring.lock() {
            r.push_slice(&out[..CHUNK]);
        }
        out.drain(..CHUNK);
        // Silence gate is applied at SEND time (keep ring intact for UI),
        // but we still notify so the meter animates.
        let _ = tx.send(level);
    }
}

fn push_resampled_f32(
    data: &[f32],
    ch: usize,
    step: f64,
    frac: &mut f64,
    out: &mut Vec<i16>,
    ring: &Arc<Mutex<Ring>>,
    tx: &Sender<f32>,
) {
    // Mix to mono on the fly, then linear-interp resample.
    let frames = data.len() / ch.max(1);
    let mut mono: [f32; 4096] = [0.0; 4096];
    let n = frames.min(4096);
    for i in 0..n {
        let mut acc = 0.0;
        for c in 0..ch {
            acc += data[i * ch + c];
        }
        mono[i] = acc / ch.max(1) as f32;
    }
    let mut pos = *frac;
    while (pos as usize) + 1 < n {
        let i = pos as usize;
        let f = (pos - i as f64) as f32;
        let s = mono[i] * (1.0 - f) + mono[i + 1] * f;
        out.push((s.clamp(-1.0, 1.0) * 32767.0) as i16);
        pos += step;
        if out.len() >= CHUNK * 2 {
            flush_out(out, ring, tx);
        }
    }
    // Carry the fractional read position into the NEXT callback's fresh
    // buffer (new buffer sample 0 == old sample `frames`). May be slightly
    // negative (overlap) — `as usize` saturates to 0, which self-heals.
    // (A previous revision computed this against the truncated window and
    //  stalled the stream after the first callback: chunks_out stayed 0.
    //  Caught live-testing against the Transcribe API.)
    *frac = pos - frames as f64;
    if *frac >= n as f64 {
        *frac = 0.0;
    }
    flush_out(out, ring, tx);
}

fn push_resampled_i16(
    data: &[i16],
    ch: usize,
    step: f64,
    frac: &mut f64,
    out: &mut Vec<i16>,
    ring: &Arc<Mutex<Ring>>,
    tx: &Sender<f32>,
) {
    let frames = data.len() / ch.max(1);
    let mut pos = *frac;
    while (pos as usize) + 1 < frames {
        let i = pos as usize;
        let f = (pos - i as f64) as f32;
        let mut a = 0i32;
        let mut b = 0i32;
        for c in 0..ch {
            a += data[i * ch + c] as i32;
            b += data[(i + 1) * ch + c] as i32;
        }
        let chf = ch.max(1) as f32;
        let s = (a as f32 / chf) * (1.0 - f) + (b as f32 / chf) * f;
        out.push(s as i16);
        pos += step;
        if out.len() >= CHUNK * 2 {
            flush_out(out, ring, tx);
        }
    }
    *frac = pos - frames as f64;
    flush_out(out, ring, tx);
}

fn push_resampled_u16(
    data: &[u16],
    ch: usize,
    step: f64,
    frac: &mut f64,
    out: &mut Vec<i16>,
    ring: &Arc<Mutex<Ring>>,
    tx: &Sender<f32>,
) {
    let frames = data.len() / ch.max(1);
    let mut pos = *frac;
    while (pos as usize) + 1 < frames {
        let i = pos as usize;
        let f = (pos - i as f64) as f32;
        let cvt = |v: u16| (v as i32 - 32768) as f32;
        let mut a = 0.0;
        let mut b = 0.0;
        for c in 0..ch {
            a += cvt(data[i * ch + c]);
            b += cvt(data[(i + 1) * ch + c]);
        }
        let chf = ch.max(1) as f32;
        out.push(((a / chf) * (1.0 - f) + (b / chf) * f) as i16);
        pos += step;
        if out.len() >= CHUNK * 2 {
            flush_out(out, ring, tx);
        }
    }
    *frac = pos - frames as f64;
    flush_out(out, ring, tx);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_overwrites_oldest_and_stays_bounded() {
        let mut r = Ring::new();
        for i in 0..(RING_CAP + 500) {
            r.push((i % 32767) as i16);
        }
        assert_eq!(r.len, RING_CAP);
        let mut out = [0i16; CHUNK];
        let n = r.drain(&mut out);
        assert_eq!(n, CHUNK);
        // First surviving sample is #500.
        assert_eq!(out[0], 500);
    }

    #[test]
    fn rms_silence_is_zero() {
        assert_eq!(rms(&[0i16; 64]), 0.0);
        assert_eq!(rms(&[]), 0.0);
    }

    #[test]
    fn rms_full_scale_sine_ballpark() {
        // ±1000 square wave => RMS == 1000.
        let mut v = [0i16; 100];
        for (i, s) in v.iter_mut().enumerate() {
            *s = if i % 2 == 0 { 1000 } else { -1000 };
        }
        let r = rms(&v);
        assert!((r - 1000.0).abs() < 1.0, "rms={r}");
    }

    #[test]
    fn resampler_flows_across_callbacks() {
        // Regression test: the fractional-carry bug stalled the stream after
        // the first callback (chunks flushed stayed 0 forever). Feed several
        // realistic callbacks (1102 stereo f32 frames @48kHz, as observed via
        // cpal) and require continuous output at ~1/3 rate.
        let ring = Arc::new(Mutex::new(Ring::new()));
        let (tx, rx) = mpsc::channel::<f32>();
        let mut frac = 0.0f64;
        let mut out: Vec<i16> = Vec::new();
        let ch = 2usize;
        let step = 48000.0 / 16000.0;
        for _ in 0..5 {
            let mut data = vec![0.0f32; 1102 * ch];
            for i in 0..1102 {
                let s = (i as f32 * 0.1).sin() * 0.8;
                data[i * ch] = s;
                data[i * ch + 1] = s;
            }
            push_resampled_f32(&data, ch, step, &mut frac, &mut out, &ring, &tx);
        }
        let mut chunks = 0u32;
        while rx.try_recv().is_ok() {
            chunks += 1;
        }
        // Flushed samples live in the ring (nothing drains in this test).
        let total = ring.lock().unwrap().len + out.len();
        assert!(chunks >= 1, "at least one chunk flushed (stream stalled!)");
        assert!(total >= 1500, "total outputs={total}, expected ~1836");
    }
}
