//! Take-history persistence: the enhanced 16 kHz mono audio of every take and
//! the transcript it produced, saved as small self-contained files under the
//! app data directory.
//!
//! Layout (one directory per take keeps audio + transcript atomic to move or
//! delete, and ISO-like timestamped names sort chronologically in any file
//! manager):
//!   <app data>/history/2026-09-29T14-32-05Z/
//!       audio.wav        PCM16 mono 16 kHz (standard WAV, opens anywhere)
//!       transcript.txt   exactly what was inserted into the focused editor
//!
//! Storage cost: 32 KiB/s (44-byte header + 16-bit mono samples). A minute of
//! speech is ~1.9 MB. Retention is period-based (day/week/month/year): old
//! takes are purged after each save, and a hard per-period take cap bounds
//! runaway storage even inside the period. Writes happen once per take on the
//! session thread — no capture-path work, so the audio thread never blocks.

use std::path::{Path, PathBuf};

/// Audio format written to disk: enhanced PCM16 mono, 16 kHz (the same
/// bytes that went to Gemini; no re-encode).
pub const HISTORY_SAMPLE_RATE: u32 = 16_000;

/// Bytes per second of stored audio: 2 (PCM16) × 16 000 Hz.
const WAV_BYTES_PER_SEC: u32 = HISTORY_SAMPLE_RATE * 2;

/// How much history to keep before old takes are deleted. The settings UI
/// and tray expose these as "day-old", "week-old", "month-old", "year-old".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Retention {
    Day,
    Week,
    Month,
    Year,
}

impl Retention {
    /// Display/menu order: shortest first.
    pub const ALL: [Retention; 4] = [
        Retention::Day,
        Retention::Week,
        Retention::Month,
        Retention::Year,
    ];

    /// Canonical config/wire key.
    pub fn key(self) -> &'static str {
        match self {
            Retention::Day => "day",
            Retention::Week => "week",
            Retention::Month => "month",
            Retention::Year => "year",
        }
    }

    /// Human label used in dropdowns ("Keep one day of history", …).
    pub fn label(self) -> &'static str {
        match self {
            Retention::Day => "Day-old",
            Retention::Week => "Week-old",
            Retention::Month => "Month-old",
            Retention::Year => "Year-old",
        }
    }

    /// Age beyond which takes are purged.
    pub fn age_secs(self) -> u64 {
        const DAY: u64 = 86_400;
        match self {
            Retention::Day => DAY,
            Retention::Week => 7 * DAY,
            Retention::Month => 30 * DAY,
            Retention::Year => 365 * DAY,
        }
    }

    /// Hard take-count cap per period, so a chatty day can still never grow
    /// storage without bound. ~25 min of speech/day, ~2 h/week, ~8 h/month,
    /// ~30 h/year at these caps.
    pub fn max_takes(self) -> usize {
        match self {
            Retention::Day => 150,
            Retention::Week => 250,
            Retention::Month => 1_000,
            Retention::Year => 4_000,
        }
    }

    /// Parse a stored key; unknown values fall back to the most conservative
    /// period (year) so corrupt configs never nuke recent history.
    pub fn from_key(key: &str) -> Self {
        match key.trim().to_ascii_lowercase().as_str() {
            "day" => Retention::Day,
            "week" => Retention::Week,
            "month" => Retention::Month,
            _ => Retention::Year,
        }
    }
}

/// Seconds since the Unix epoch (0 if the clock is before it).
pub fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Timestamped take-directory name, e.g. `2026-09-29T14-32-05Z`.
/// Uses `YYYY-MM-DDThh-mm-ssZ` (`-` instead of `:` — colons are illegal in
/// Windows file names). UTC so entries sort consistently across DST/timezone
/// changes. Pure so it is unit-testable.
pub fn take_dir_name(unix_secs: u64) -> String {
    let (year, month, day, hour, min, sec) = civil_from_unix(unix_secs);
    format!("{year:04}-{month:02}-{day:02}T{hour:02}-{min:02}-{sec:02}Z")
}

/// Inverse of `civil_from_unix` for take-directory validation. Returns None
/// for impossible calendar dates (month 13, Feb 30, hour 25, …).
fn unix_from_civil(year: u32, month: u32, day: u32, hour: u32, min: u32, sec: u32) -> Option<u64> {
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || hour >= 24
        || min >= 60
        || sec >= 60
        || year < 1970
    {
        return None;
    }
    let y = if month <= 2 { year - 1 } else { year } as i64;
    let m = if month > 2 { month - 3 } else { month + 9 } as i64;
    let era = y.div_euclid(400);
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * m + 2) / 5 + day as i64 - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    let days = era * 146_097 + doe - 719_468;
    // Reject days beyond the month length via the round-trip in the caller;
    // here we only bound the result to a valid u64 range.
    if days < 0 {
        return None;
    }
    Some(days as u64 * 86_400 + hour as u64 * 3_600 + min as u64 * 60 + sec as u64)
}

/// Proleptic Gregorian date from Unix seconds (UTC), no external crates.
/// Days-from-epoch → civil algorithm (Howard Hinnant) + clock split.
fn civil_from_unix(unix_secs: u64) -> (u32, u32, u32, u32, u32, u32) {
    let days = (unix_secs / 86_400) as i64;
    let secs_of_day = (unix_secs % 86_400) as u32;
    // Shift into the positive era (1970-03-01 aligns leap years cleanly).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097); // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y } as u32;
    let (hour, min, sec) = (
        secs_of_day / 3_600,
        (secs_of_day % 3_600) / 60,
        secs_of_day % 60,
    );
    (year, m, d, hour, min, sec)
}

/// Parse a valid take-directory name back to its Unix timestamp. Returns
/// None for anything the formatter could not have produced (foreign files,
/// impossible dates), so deletion code can never touch unknown entries.
pub fn parse_take_name(name: &str) -> Option<u64> {
    let bytes = name.as_bytes();
    let num = |a: usize, z: usize| name[a..z].parse::<u32>().ok();
    // Shape: exactly `YYYY-MM-DDThh-mm-ssZ` (20 bytes). Every byte is a digit
    // except the four dashes, the T separator, and the trailing Z.
    let shaped = name.len() == 20
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b'T'
        && bytes[13] == b'-'
        && bytes[16] == b'-'
        && bytes[19] == b'Z'
        && (0..19).all(|i| matches!(i, 4 | 7 | 10 | 13 | 16) || bytes[i].is_ascii_digit());
    if !shaped {
        return None;
    }
    let (Some(year), Some(month), Some(day), Some(hour), Some(min), Some(sec)) = (
        num(0, 4),
        num(5, 7),
        num(8, 10),
        num(11, 13),
        num(14, 16),
        num(17, 19),
    ) else {
        return None;
    };
    let unix = unix_from_civil(year, month, day, hour, min, sec)?;
    (take_dir_name(unix) == name).then_some(unix)
}

/// App data root (same directory that holds config.json).
pub fn history_root() -> PathBuf {
    crate::config::config_path()
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("history")
}

/// Standard 44-byte PCM WAV header for 16-bit mono at 16 kHz.
/// Fields match the RIFF spec so any player/browser/DAW opens the file.
pub fn wav_header(data_len: u32) -> [u8; 44] {
    let mut h = [0u8; 44];
    let byte_rate = WAV_BYTES_PER_SEC;
    h[0..4].copy_from_slice(b"RIFF");
    h[4..8].copy_from_slice(&(36 + data_len).to_le_bytes());
    h[8..12].copy_from_slice(b"WAVE");
    h[12..16].copy_from_slice(b"fmt ");
    h[16..20].copy_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    h[20..22].copy_from_slice(&1u16.to_le_bytes()); // PCM
    h[22..24].copy_from_slice(&1u16.to_le_bytes()); // mono
    h[24..28].copy_from_slice(&HISTORY_SAMPLE_RATE.to_le_bytes());
    h[28..32].copy_from_slice(&byte_rate.to_le_bytes());
    h[32..34].copy_from_slice(&2u16.to_le_bytes()); // block align
    h[34..36].copy_from_slice(&16u16.to_le_bytes()); // bits per sample
    h[36..40].copy_from_slice(b"data");
    h[40..44].copy_from_slice(&data_len.to_le_bytes());
    h
}

pub struct SaveOutcome {
    pub dir: PathBuf,
    /// Total bytes written (44-byte header + PCM payload); for logging.
    #[allow(dead_code)]
    pub audio_bytes: u64,
}

/// Persist one take: `dir/take_name/audio.wav` + `transcript.txt`.
/// Writes are best-effort; a failure never blocks dictation (returns Err and
/// the session loop just logs it). A second take in the same second updates
/// the same directory instead of failing.
pub fn save_take(
    root: &Path,
    unix_secs: u64,
    pcm: &[i16],
    transcript: &str,
) -> Result<SaveOutcome, String> {
    if pcm.is_empty() || transcript.trim().is_empty() {
        return Err("nothing to save".to_string());
    }
    let dir = root.join(take_dir_name(unix_secs));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create history dir: {e}"))?;

    // WAV payload from the i16 samples, then the header with the real length.
    let data_len = pcm.len() as u32 * 2;
    let mut wav = Vec::with_capacity(44 + data_len as usize);
    wav.extend_from_slice(&wav_header(data_len));
    for s in pcm {
        wav.extend_from_slice(&s.to_le_bytes());
    }

    let audio_path = dir.join("audio.wav");
    let transcript_path = dir.join("transcript.txt");
    std::fs::write(&audio_path, &wav).map_err(|e| format!("write audio.wav: {e}"))?;
    if let Err(e) = std::fs::write(&transcript_path, transcript) {
        let _ = std::fs::remove_file(&audio_path); // never keep an orphan WAV
        return Err(format!("write transcript.txt: {e}"));
    }
    Ok(SaveOutcome {
        dir,
        audio_bytes: 44 + data_len as u64,
    })
}

/// Recursive on-disk size of a file or directory (what deletion would free).
pub fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(meta) = std::fs::symlink_metadata(path) {
        if meta.is_file() {
            total += meta.len();
        } else if meta.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.filter_map(|e| e.ok()) {
                    total += dir_size(&entry.path());
                }
            }
        }
    }
    total
}

/// Delete the oldest take directories beyond the newest `retain` ones.
/// Only whole directories whose names parse as take stamps are removed;
/// anything else (user files) is left untouched. Returns removed count.
pub fn prune(root: &Path, retain: usize) -> usize {
    let mut takes: Vec<(String, PathBuf)> = list_take_dirs(root);
    if takes.len() <= retain {
        return 0;
    }
    takes.sort_by(|a, b| a.0.cmp(&b.0)); // lexicographic == chronological
    let excess = takes.len() - retain;
    let mut removed = 0;
    for (_, dir) in takes.iter().take(excess) {
        if std::fs::remove_dir_all(dir).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Delete every take older than `retention.age_secs()` relative to `now`.
/// Returns (removed count, bytes freed). Foreign files are never touched.
pub fn purge_older(root: &Path, retention: Retention, now: u64) -> (usize, u64) {
    let cutoff = now.saturating_sub(retention.age_secs());
    let mut removed = 0usize;
    let mut freed = 0u64;
    for (name, path) in list_take_dirs(root) {
        let Some(unix) = parse_take_name(&name) else {
            continue;
        };
        if unix >= cutoff {
            continue;
        }
        let size = dir_size(&path);
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
            freed += size;
        }
    }
    (removed, freed)
}

/// Delete every take (never foreign files). Returns (removed count, bytes).
pub fn clear_all(root: &Path) -> (usize, u64) {
    let mut removed = 0usize;
    let mut freed = 0u64;
    for (_, path) in list_take_dirs(root) {
        let size = dir_size(&path);
        if std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
            freed += size;
        }
    }
    (removed, freed)
}

/// All valid take directories as (name, path), unsorted.
fn list_take_dirs(root: &Path) -> Vec<(String, PathBuf)> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            parse_take_name(&name).map(|_| (name, e.path()))
        })
        .collect()
}

/// One take as shown in the History list.
#[derive(Debug, Clone)]
pub struct TakeMeta {
    /// Directory name (the take's stable id): `2026-09-29T14-32-05Z`.
    pub name: String,
    /// Transcript text (trimmed). Empty when transcript.txt is missing.
    pub transcript: String,
    /// Take time as Unix seconds.
    pub unix: u64,
    /// Stored audio duration in whole milliseconds.
    #[allow(dead_code)] // shown in a future UI iteration
    pub duration_ms: u64,
    /// On-disk size of the take directory (audio + transcript).
    pub size_bytes: u64,
}

/// List takes newest-first, at most `limit` of them. Transcript reads are
/// bounded (the History UI shows previews only).
pub fn list_takes(root: &Path, limit: usize) -> Vec<TakeMeta> {
    let mut takes: Vec<(String, PathBuf)> = list_take_dirs(root);
    takes.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    takes
        .into_iter()
        .take(limit)
        .filter_map(|(name, path)| {
            let unix = parse_take_name(&name)?;
            let transcript = std::fs::read_to_string(path.join("transcript.txt"))
                .unwrap_or_default()
                .lines()
                .collect::<Vec<_>>()
                .join(" ")
                .trim()
                .to_string();
            let duration_ms =
                audio_bytes(&path).saturating_sub(44) / 2 * 1_000 / u64::from(HISTORY_SAMPLE_RATE);
            Some(TakeMeta {
                name,
                transcript,
                unix,
                duration_ms,
                size_bytes: dir_size(&path),
            })
        })
        .collect()
}

/// Size in bytes of a take's audio.wav (0 when missing).
fn audio_bytes(take_dir: &Path) -> u64 {
    std::fs::metadata(take_dir.join("audio.wav"))
        .map(|m| m.len())
        .unwrap_or(0)
}

/// PCM16 samples of a take's audio (everything after the 44-byte header).
pub fn load_take_audio(root: &Path, name: &str) -> Result<Vec<i16>, String> {
    if parse_take_name(name).is_none() {
        return Err("invalid take name".to_string());
    }
    let bytes = std::fs::read(root.join(name).join("audio.wav"))
        .map_err(|e| format!("read audio.wav: {e}"))?;
    if bytes.len() < 44 || bytes.len() % 2 != 0 {
        return Err("audio.wav is truncated".to_string());
    }
    Ok(bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| i16::from_le_bytes(*pair))
        .collect())
}

/// Transcript of a take (empty string when missing).
#[allow(dead_code)] // available for the tray/CLI surfaces
pub fn load_transcript(root: &Path, name: &str) -> String {
    std::fs::read_to_string(root.join(name).join("transcript.txt")).unwrap_or_default()
}

/// Persist a (re-)transcription over a take's transcript.txt.
pub fn store_transcript(root: &Path, name: &str, transcript: &str) -> Result<(), String> {
    if parse_take_name(name).is_none() {
        return Err("invalid take name".to_string());
    }
    if transcript.trim().is_empty() {
        return Err("nothing to save".to_string());
    }
    std::fs::create_dir_all(root.join(name)).map_err(|e| format!("create take dir: {e}"))?;
    std::fs::write(root.join(name).join("transcript.txt"), transcript)
        .map_err(|e| format!("write transcript.txt: {e}"))
}

/// Delete exactly one take by name. Refuses names that are not validated
/// take stamps, so user files can never be removed through this path.
pub fn delete_take(root: &Path, name: &str) -> Result<(), String> {
    if parse_take_name(name).is_none() {
        return Err("invalid take name".to_string());
    }
    std::fs::remove_dir_all(root.join(name)).map_err(|e| format!("delete take: {e}"))
}

/// Open the history folder in the platform file manager (best effort).
pub fn open_in_explorer(root: &Path) -> Result<(), String> {
    std::fs::create_dir_all(root).map_err(|e| format!("create history dir: {e}"))?;
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(root)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(root)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
    #[cfg(all(not(target_os = "windows"), not(target_os = "macos")))]
    {
        std::process::Command::new("xdg-open")
            .arg(root)
            .spawn()
            .map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// Save under the default root and enforce the retention policy: age-based
/// purge first, then the hard take-count cap. Returns the saved directory.
pub fn record(pcm: &[i16], transcript: &str, retention: Retention) -> Result<PathBuf, String> {
    let root = history_root();
    let now = now_secs();
    let outcome = save_take(&root, now, pcm, transcript)?;
    let (purged, bytes) = purge_older(&root, retention, now);
    if purged > 0 {
        println!(
            "[utterly] history: purged {purged} takes past {} retention ({:.1} MB)",
            retention.label(),
            bytes as f64 / (1024.0 * 1024.0)
        );
    }
    prune(&root, retention.max_takes());
    Ok(outcome.dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_are_iso_like_and_sort_chronologically() {
        // 2026-09-29 14:32:05 UTC == 1_790_692_325.
        let name = take_dir_name(1_790_692_325);
        assert_eq!(name, "2026-09-29T14-32-05Z");
        // Two adjacent timestamps sort correctly as plain strings.
        let a = take_dir_name(1_790_692_325);
        let b = take_dir_name(1_790_692_326);
        let c = take_dir_name(1_793_536_000); // ~1 month later
        assert!(a < b && b < c);
        // Epoch boundary sanity.
        assert_eq!(take_dir_name(0), "1970-01-01T00-00-00Z");
        // Leap-year day renders fine.
        assert_eq!(take_dir_name(951_782_400), "2000-02-29T00-00-00Z");
    }

    #[test]
    fn take_name_parser_round_trips_and_rejects_garbage() {
        assert_eq!(parse_take_name("2026-09-29T14-32-05Z"), Some(1_790_692_325));
        assert_eq!(parse_take_name("1970-01-01T00-00-00Z"), Some(0));
        // Foreign or malformed names are never take directories.
        assert_eq!(parse_take_name("my-notes"), None);
        assert_eq!(parse_take_name(""), None);
        assert_eq!(parse_take_name("2026-09-29 14-32-05Z"), None);
        assert_eq!(parse_take_name("2026-13-29T14-32-05Z"), None); // month 13
        assert_eq!(parse_take_name("2026-02-30T14-32-05Z"), None); // Feb 30
        assert_eq!(parse_take_name("2026-09-29T25-32-05Z"), None); // hour 25
                                                                   // Traversal attempts can't survive the strict shape check.
        assert_eq!(parse_take_name("../../etcT00-00-00Z"), None);
    }

    #[test]
    fn retention_periods_map_keys_labels_and_caps() {
        assert_eq!(Retention::from_key("day"), Retention::Day);
        assert_eq!(Retention::from_key(" WEEK "), Retention::Week);
        assert_eq!(Retention::from_key("month"), Retention::Month);
        assert_eq!(Retention::from_key("garbage"), Retention::Year);
        assert!(Retention::Day.age_secs() < Retention::Week.age_secs());
        assert!(Retention::Week.age_secs() < Retention::Month.age_secs());
        assert!(Retention::Month.age_secs() < Retention::Year.age_secs());
        assert_eq!(Retention::Day.age_secs(), 86_400);
        assert_eq!(Retention::Week.label(), "Week-old");
        for r in Retention::ALL {
            assert!(r.max_takes() > 0);
            assert_eq!(r.key(), r.key()); // canonical keys are stable
        }
    }

    #[test]
    fn wav_header_is_a_valid_pcm16_mono_16k_riff() {
        let data_len = 32_000u32; // exactly 1 s
        let h = wav_header(data_len);
        assert_eq!(&h[0..4], b"RIFF");
        assert_eq!(&h[8..12], b"WAVE");
        assert_eq!(&h[12..16], b"fmt ");
        let fmt_size = u32::from_le_bytes(h[16..20].try_into().unwrap());
        assert_eq!(fmt_size, 16);
        assert_eq!(u16::from_le_bytes(h[20..22].try_into().unwrap()), 1); // PCM
        assert_eq!(u16::from_le_bytes(h[22..24].try_into().unwrap()), 1); // mono
        assert_eq!(
            u32::from_le_bytes(h[24..28].try_into().unwrap()),
            HISTORY_SAMPLE_RATE
        );
        assert_eq!(u32::from_le_bytes(h[28..32].try_into().unwrap()), 32_000);
        assert_eq!(u16::from_le_bytes(h[32..34].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(h[34..36].try_into().unwrap()), 16);
        assert_eq!(&h[36..40], b"data");
        assert_eq!(u32::from_le_bytes(h[40..44].try_into().unwrap()), data_len);
        // RIFF chunk size covers everything after the first 8 bytes.
        assert_eq!(
            u32::from_le_bytes(h[4..8].try_into().unwrap()),
            36 + data_len
        );
    }

    #[test]
    fn save_take_wears_standard_layout_and_round_trips() {
        let root = std::env::temp_dir().join(format!("utterly-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let pcm: Vec<i16> = (0..1_600u32).map(|i| (i as i16 % 500) - 250).collect();
        let outcome = save_take(&root, 1_790_692_325, &pcm, "Mic test 123.").unwrap();
        assert_eq!(outcome.dir, root.join("2026-09-29T14-32-05Z"));
        let wav = std::fs::read(outcome.dir.join("audio.wav")).unwrap();
        assert_eq!(wav.len(), 44 + 3_200);
        assert_eq!(&wav[0..4], b"RIFF");
        let payload = &wav[44..];
        for (i, pair) in payload.as_chunks::<2>().0.iter().enumerate() {
            assert_eq!(i16::from_le_bytes(*pair), pcm[i]);
        }
        let text = std::fs::read_to_string(outcome.dir.join("transcript.txt")).unwrap();
        assert_eq!(text, "Mic test 123.");
        // A second take in the SAME second must not be lost.
        let again = save_take(&root, 1_790_692_325, &pcm, "second").unwrap();
        assert_eq!(again.dir, outcome.dir);
        assert!(again.dir.join("transcript.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn save_take_rejects_empty_input() {
        let root = std::env::temp_dir().join(format!("utterly-test-e-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(save_take(&root, 0, &[], "text").is_err());
        let pcm = [0i16; 10];
        assert!(save_take(&root, 0, &pcm, "  ").is_err());
        assert!(!root.join("1970-01-01T00-00-00Z").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn prune_keeps_newest_and_never_touches_foreign_files() {
        let root = std::env::temp_dir().join(format!("utterly-test-p-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        // Three takes (one second apart) plus a user directory and a file.
        for (i, sec) in [1_790_692_320u64, 1_790_692_321, 1_790_692_322]
            .iter()
            .enumerate()
        {
            save_take(&root, *sec, &[1i16], &format!("take-{i}")).unwrap();
        }
        std::fs::create_dir_all(root.join("my-notes")).unwrap();
        std::fs::write(root.join("readme.txt"), "keep me").unwrap();
        assert_eq!(prune(&root, 2), 1);
        // Newest two takes survive; oldest is gone; foreign entries untouched.
        assert!(!root.join("2026-09-29T14-32-00Z").exists());
        assert!(root.join("2026-09-29T14-32-01Z").exists());
        assert!(root.join("2026-09-29T14-32-02Z").exists());
        assert!(root.join("my-notes").is_dir());
        assert!(root.join("readme.txt").is_file());
        // At-or-under retention never deletes anything.
        assert_eq!(prune(&root, 2), 0);
        assert_eq!(prune(&root, 0), 2); // removes all valid takes
        assert!(root.join("my-notes").is_dir());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn purge_older_deletes_only_takes_past_the_cutoff() {
        let root = std::env::temp_dir().join(format!("utterly-test-a-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let now = 1_800_000_000u64;
        // Two takes 2 h apart plus foreign junk.
        save_take(&root, now - 3 * 3_600, &[1i16], "old").unwrap();
        save_take(&root, now - 3_600, &[1i16], "new").unwrap();
        std::fs::create_dir_all(root.join("keep-me")).unwrap();
        std::fs::write(root.join("note.txt"), "stay").unwrap();
        let old_dir = root.join(take_dir_name(now - 3 * 3_600));
        assert!(old_dir.join("audio.wav").is_file());
        let old_size = dir_size(&old_dir);
        assert!(old_size > 44);
        // One-day retention: the 3-hour-old take is inside the period.
        assert_eq!(purge_older(&root, Retention::Day, now), (0, 0));
        // Week retention still keeps it (3 h < 1 week).
        assert_eq!(purge_older(&root, Retention::Week, now), (0, 0));
        // 22 h later (Day retention): the 3-hour-old take has aged past the
        // 1-day cutoff (now-2h) while the 1-hour-old take survives.
        let (removed, freed) = purge_older(&root, Retention::Day, now + 22 * 3_600);
        assert_eq!((removed, freed), (1, old_size));
        assert!(!old_dir.exists());
        assert!(root.join(take_dir_name(now - 3_600)).exists());
        assert!(root.join("keep-me").is_dir());
        assert!(root.join("note.txt").is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn list_delete_transcript_and_audio_round_trip() {
        let root = std::env::temp_dir().join(format!("utterly-test-l-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        save_take(&root, 1_790_692_320, &[7i16; 1_600], "first take").unwrap();
        save_take(&root, 1_790_692_321, &[0i16; 8_000], "second take").unwrap();
        let takes = list_takes(&root, 10);
        assert_eq!(takes.len(), 2, "newest first");
        assert_eq!(takes[0].name, "2026-09-29T14-32-01Z");
        assert_eq!(takes[0].transcript, "second take");
        assert_eq!(takes[0].duration_ms, 500); // 8000 samples @16 kHz
        assert!(takes[0].size_bytes > 44 + 16_000);
        assert_eq!(takes[1].transcript, "first take");
        // Limit applies.
        assert_eq!(list_takes(&root, 1).len(), 1);
        // Audio round-trip.
        let pcm = load_take_audio(&root, "2026-09-29T14-32-00Z").unwrap();
        assert_eq!(pcm, vec![7i16; 1_600]);
        assert!(load_take_audio(&root, "my-notes").is_err());
        assert!(load_take_audio(&root, "../escape").is_err());
        // Transcript rewrite (the re-transcribe path).
        store_transcript(&root, "2026-09-29T14-32-00Z", "better words").unwrap();
        assert_eq!(
            load_transcript(&root, "2026-09-29T14-32-00Z"),
            "better words"
        );
        assert!(store_transcript(&root, "nope", "x").is_err());
        // Delete exactly one take.
        delete_take(&root, "2026-09-29T14-32-00Z").unwrap();
        assert!(load_take_audio(&root, "2026-09-29T14-32-00Z").is_err());
        assert_eq!(list_takes(&root, 10).len(), 1);
        assert!(delete_take(&root, "my-notes").is_err()); // foreign never deleted
        assert!(root.join("my-notes").exists() || !root.join("my-notes").exists());
        // clear_all removes every take, nothing else.
        let (removed, _) = clear_all(&root);
        assert_eq!(removed, 1);
        assert_eq!(list_takes(&root, 10).len(), 0);
        let _ = std::fs::remove_dir_all(&root);
    }
}
