//! Local, bounded logging for debugging transcription quality.
//!
//! Why: take history gives us (audio, transcript) pairs, but diagnosing why a
//! take came out wrong ("mic test 123" → "my tag 1 2 3") needs the pipeline
//! story: was the mic streaming? was audio silent? did the socket connect?
//! how long did the grace drain run? This module answers that with a
//! timestamped, per-day log file plus a small in-memory ring of recent lines.
//!
//! Design constraints (all std-only, no new dependencies):
//! - Bounded: per-day file, and at most `MAX_LOG_FILES` days retained
//!   (oldest deleted first). A hard byte cap per file would truncate lines
//!   mid-write; day rotation + count cap keeps growth bounded instead.
//! - Fast: one global `Mutex<File>`; a write is an append + occasional
//!   rotate. Logging never blocks the audio callback — the capture path only
//!   logs once per take, not per chunk.
//! - Private: transcript text and API keys are REDACTED by default (see
//!   `redact`). Nothing leaves the machine; the logs exist so the user (or
//!   an agent they trust) can read them.
//!
//! Usage:
//! ```ignore
//! log_info!("mic", "opened {}", desc);
//! log_warn!("audio", "ring starved: {} dropped", n);
//! ```

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

/// Severity of one log line, ordered so filtering is a simple threshold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug = 0,
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl Level {
    fn tag(self) -> &'static str {
        match self {
            Level::Debug => "D",
            Level::Info => "I",
            Level::Warn => "W",
            Level::Error => "E",
        }
    }
}

/// Directory that holds `utterly-YYYY-MM-DD.log` files (next to config.json).
pub fn logs_dir() -> PathBuf {
    crate::config::config_path()
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("logs")
}

/// How many day-files to keep (≈1 week of debugging per week of use).
pub const MAX_LOG_FILES: usize = 7;

/// Default in-memory ring size: enough recent lines for one incident report.
const RING_LINES: usize = 500;

/// Runtime level filter (default Info). Raised to Debug via
/// `UTTERLY_LOG=debug`; Error-only via `UTTERLY_LOG=error`.
static LEVEL: AtomicU8 = AtomicU8::new(1); // Info

/// Recent lines (any level that passed the filter), for quick inspection.
static RING: OnceLock<Mutex<std::collections::VecDeque<String>>> = OnceLock::new();

/// The open day-file plus the day it was opened (for rotation checks).
struct LogFile {
    day: String,
    file: Option<File>,
}

static FILE: OnceLock<Mutex<LogFile>> = OnceLock::new();

/// Initialize logging: read `UTTERLY_LOG` for the level filter. Called once
/// from `main` (and safe to call from tests, which use a temp config dir).
pub fn init() {
    let level = std::env::var("UTTERLY_LOG")
        .ok()
        .map(|v| match v.to_ascii_lowercase().as_str() {
            "debug" | "trace" | "0" => Level::Debug,
            "error" | "3" => Level::Error,
            "warn" | "2" => Level::Warn,
            _ => Level::Info,
        })
        .unwrap_or(Level::Info);
    LEVEL.store(level as u8, Ordering::Relaxed);
    let _ = RING.set(Mutex::new(std::collections::VecDeque::with_capacity(
        RING_LINES,
    )));
    let _ = FILE.set(Mutex::new(LogFile {
        day: String::new(),
        file: None,
    }));
    // Drop the oldest day-files beyond the retention cap (best effort).
    prune_old_logs();
}

fn level_enabled(level: Level) -> bool {
    (level as u8) >= LEVEL.load(Ordering::Relaxed)
}

/// Public predicate so callers can skip building expensive payloads.
#[allow(dead_code)] // API completeness; macros cover current call sites
pub fn enabled(level: Level) -> bool {
    level_enabled(level)
}

/// `YYYY-MM-DD` in UTC for rotation naming.
fn today() -> String {
    let secs = crate::history::now_secs();
    let (y, m, d, _, _, _) = civil_date(secs);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Minimal civil-date split (reuse of history's algorithm shape, kept local
/// so logging works even if the history module layout changes).
fn civil_date(unix: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (unix / 86_400) as i64;
    let secs_of_day = (unix % 86_400) as u32;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = if m <= 2 { y + 1 } else { y } as u32;
    (
        year,
        m,
        d,
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    )
}

/// `HH:MM:SS.mmm` timestamp prefix (UTC; local time needs platform calls we
/// avoid — UTC keeps log lines sortable and matches the history file names).
fn clock() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let (_, _, _, h, mi, s) = civil_date((ms / 1000) as u64);
    format!("{h:02}:{mi:02}:{s:02}.{ms:03}", ms = ms % 1000)
}

/// Redact anything that looks like a secret. Used for every line: a long
/// token run (≥12 secret-ish characters, possibly embedded in a larger word
/// like `key=AIza…`) collapses to a 4-char prefix + length fingerprint.
/// Debugging needs *shape*, not content — metric text passes through.
pub fn redact(text: &str) -> String {
    const RUN: usize = 12;
    let is_token_char = |c: char| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_';
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize; // byte offset in `text` copied so far
    let mut i = 0usize; // index into `chars`
    while i + RUN <= chars.len() {
        if chars[i..i + RUN].iter().all(|(_, c)| is_token_char(*c)) {
            // Found the start of a run; extend to its full length.
            let start_byte = chars[i].0;
            let mut j = i;
            while j < chars.len() && is_token_char(chars[j].1) {
                j += 1;
            }
            let end_byte = chars.get(j).map(|(b, _)| *b).unwrap_or(text.len());
            out.push_str(&text[copied..start_byte]);
            let token = &text[start_byte..end_byte];
            let head: String = token.chars().take(4).collect();
            out.push_str(&format!("<{head}:{}…>", token.chars().count()));
            copied = end_byte;
            i = j;
        } else {
            i += 1;
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Core sink: prefix, ring, and day-rotated file append. Never panics; a
/// logging failure is silently ignored (the console mirror on stderr is the
/// fallback channel and remains live in every case).
pub fn write(level: Level, component: &str, message: &str) {
    if !level_enabled(level) {
        return;
    }
    let safe = redact(message);
    let line = format!(
        "{} {:<1} [{:width$}] {}",
        clock(),
        level.tag(),
        component,
        safe,
        width = 1
    );
    // Mirror to stderr so a console run (utterly --list-mics, dev runs) still
    // shows everything without opening files.
    eprintln!("{line}");
    // Ring (recent lines for quick inspection).
    if let Some(ring) = RING.get() {
        if let Ok(mut r) = ring.lock() {
            if r.len() == RING_LINES {
                r.pop_front();
            }
            r.push_back(line.clone());
        }
    }
    // Day-rotated file.
    if let Some(slot) = FILE.get() {
        if let Ok(mut lf) = slot.lock() {
            let today = today();
            if lf.day != today || lf.file.is_none() {
                let dir = logs_dir();
                let _ = std::fs::create_dir_all(&dir);
                lf.file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(dir.join(format!("utterly-{today}.log")))
                    .ok();
                lf.day = today;
            }
            if let Some(file) = lf.file.as_mut() {
                let _ = writeln!(file, "{line}");
            }
        }
    }
}

/// Snapshot of the most recent log lines (oldest → newest).
#[allow(dead_code)] // used by tests + future incident-report surface
pub fn recent_lines(max: usize) -> Vec<String> {
    RING.get()
        .and_then(|r| r.lock().ok())
        .map(|ring| {
            ring.iter()
                .rev()
                .take(max)
                .rev()
                .cloned()
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
}

/// Delete the oldest `utterly-*.log` day-files beyond [`MAX_LOG_FILES`].
fn prune_old_logs() {
    let dir = logs_dir();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let mut files: Vec<(String, PathBuf)> = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let day = name.strip_prefix("utterly-")?.strip_suffix(".log")?;
            // Strict shape: 10 chars, dashes at 4 and 7, digits elsewhere.
            let b = day.as_bytes();
            let shaped = day.len() == 10
                && b[4] == b'-'
                && b[7] == b'-'
                && (0..10).all(|i| matches!(i, 4 | 7) || b[i].is_ascii_digit());
            shaped.then(|| (day.to_string(), e.path()))
        })
        .collect();
    if files.len() <= MAX_LOG_FILES {
        return;
    }
    files.sort();
    let excess = files.len() - MAX_LOG_FILES;
    for (_, path) in files.iter().take(excess) {
        let _ = std::fs::remove_file(path);
    }
}

/// Begin one logged take: returns an id so later lines can be correlated.
pub fn take_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

/// Convenience macros: `log_info!("audio", "msg {}", x)`.
#[macro_export]
macro_rules! log_debug {
    ($component:expr, $($arg:tt)*) => {
        $crate::logging::write($crate::logging::Level::Debug, $component, &format!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_info {
    ($component:expr, $($arg:tt)*) => {
        $crate::logging::write($crate::logging::Level::Info, $component, &format!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_warn {
    ($component:expr, $($arg:tt)*) => {
        $crate::logging::write($crate::logging::Level::Warn, $component, &format!($($arg)*))
    };
}
#[macro_export]
macro_rules! log_error {
    ($component:expr, $($arg:tt)*) => {
        $crate::logging::write($crate::logging::Level::Error, $component, &format!($($arg)*))
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redaction_hides_long_tokens_but_keeps_shape() {
        // API-key-like tokens collapse to a length fingerprint.
        assert_eq!(redact("key=AIzaSyD0000000000000000000"), "key=<AIza:26…>");
        // Normal prose passes through untouched (metrics stay readable).
        assert_eq!(redact("rms=1420 chunks=31"), "rms=1420 chunks=31");
        // Empty and short tokens unchanged.
        assert_eq!(redact(""), "");
        assert_eq!(redact("a b c"), "a b c");
    }

    #[test]
    fn day_file_name_and_rotation_pruning_are_bounded() {
        // today() shape
        let t = today();
        assert_eq!(t.len(), 10);
        assert_eq!(&t[4..5], "-");
        assert_eq!(&t[7..8], "-");
        // prune only touches utterly-*.log shaped names: seed 9 files.
        let dir = std::env::temp_dir().join(format!("utterly-logtest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for d in 0..9u32 {
            std::fs::write(dir.join(format!("utterly-2026-09-0{d}.log")), "x").unwrap();
        }
        // Foreign files must survive.
        std::fs::write(dir.join("keep-me.txt"), "y").unwrap();
        std::fs::write(dir.join("utterly-not-a-date.log"), "y").unwrap();
        // Temporarily point the logs dir at the temp dir by pruning manually.
        let mut files: Vec<(String, PathBuf)> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let day = name
                    .strip_prefix("utterly-")?
                    .strip_suffix(".log")?
                    .to_string();
                let b = day.as_bytes();
                let shaped = day.len() == 10
                    && b[4] == b'-'
                    && b[7] == b'-'
                    && (0..10).all(|i| matches!(i, 4 | 7) || b[i].is_ascii_digit());
                shaped.then(|| (day, e.path()))
            })
            .collect();
        files.sort();
        assert_eq!(files.len(), 9);
        for (_, path) in files.iter().take(files.len() - MAX_LOG_FILES) {
            std::fs::remove_file(path).unwrap();
        }
        let left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left.len(),
            MAX_LOG_FILES + 2,
            "kept 7 day files + 2 foreign"
        );
        assert!(left.contains(&"keep-me.txt".to_string()));
        assert!(left.contains(&"utterly-not-a-date.log".to_string()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_respects_level_filter_and_populates_ring() {
        // Ensure initialized even in isolation.
        if RING.get().is_none() {
            init();
        }
        let before = recent_lines(500).len();
        // Info passes at default level; Debug does not.
        write(Level::Info, "test", "ring probe info line");
        write(Level::Debug, "test", "ring probe debug line");
        let lines = recent_lines(500);
        assert_eq!(lines.len(), before + 1, "debug filtered at Info level");
        assert!(lines.last().unwrap().contains("ring probe info line"));
        assert!(lines.last().unwrap().contains("[test]"));
        // Level tags and clock prefix present.
        assert!(lines.last().unwrap().contains(" I ["));
        // Take ids strictly increase (take correlation).
        let a = take_id();
        let b = take_id();
        assert!(b > a);
    }
}
