use std::fmt::{Display, Write as _};
use std::path::{Path, PathBuf};

use chrono::format::{Item, StrftimeItems};
use chrono::{DateTime, Duration, FixedOffset, Local, LocalResult, Offset, TimeZone, Utc};

use crate::error::{Error, Result};

/// Rotation period start/end in UTC plus the adjusted timestamp and offset
/// needed to render the period start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Period {
    pub start_utc: i64,
    pub start_adjusted: i64,
    pub offset_seconds: i64,
    pub end_utc: i64,
}

/// Time base used for interval alignment and filename formatting.
#[derive(Debug, Clone, Copy)]
pub struct Zone {
    local: bool,
    offset_minutes: i32,
}

impl Zone {
    pub fn new(local: bool, offset_minutes: i32) -> Self {
        Self {
            local,
            offset_minutes,
        }
    }

    /// Seconds east of UTC at the given instant.
    ///
    /// Local time is resolved at `now` rather than once at startup so that a
    /// long-running logger follows daylight-saving transitions.
    pub fn offset_at(&self, now_utc: i64) -> i64 {
        if self.local {
            Utc.timestamp_opt(now_utc, 0)
                .single()
                .map(|dt| dt.with_timezone(&Local).offset().fix().local_minus_utc() as i64)
                .unwrap_or(0)
        } else {
            self.offset_minutes as i64 * 60
        }
    }

    /// Convert a UTC timestamp to the "adjusted" timestamp used by Apache
    /// rotatelogs: UTC plus the local/fixed offset.
    pub fn adjust(&self, now_utc: i64) -> i64 {
        now_utc.saturating_add(self.offset_at(now_utc))
    }

    /// Compute the rotation period containing `now_utc`.
    ///
    /// For local time we align to local civil time and convert both boundaries
    /// back to UTC. This keeps daily/hourly boundaries at local midnight/hour
    /// across DST transitions.
    pub fn period(&self, now_utc: i64, interval: u64) -> Result<Period> {
        assert!(interval > 0, "rotation interval must be positive");
        if self.local {
            civil_period(&Local, now_utc, interval)
        } else {
            let offset = self.offset_at(now_utc);
            let now_adjusted = now_utc.saturating_add(offset);
            let start_adjusted = align_down(now_adjusted, interval);
            let start_utc = start_adjusted.checked_sub(offset).ok_or_else(|| {
                Error::runtime("timestamp underflow while computing rotation period")
            })?;
            Ok(Period {
                start_utc,
                start_adjusted,
                offset_seconds: offset,
                end_utc: start_utc.saturating_add(interval as i64),
            })
        }
    }

    /// Render an adjusted timestamp with a strftime pattern.
    ///
    /// `adjusted_start` already contains the zone offset. `offset` is the
    /// offset that was in effect when the rotation was planned.
    pub fn format(&self, pattern: &str, adjusted_start: i64, offset: i64) -> Result<String> {
        let utc_seconds = adjusted_start
            .checked_sub(offset)
            .ok_or_else(|| Error::runtime("timestamp underflow while formatting logfile name"))?;
        let utc = Utc
            .timestamp_opt(utc_seconds, 0)
            .single()
            .ok_or_else(|| Error::runtime("timestamp is outside the supported range"))?;

        if self.local {
            render(utc.with_timezone(&Local), pattern)
        } else {
            let offset = i32::try_from(offset)
                .ok()
                .and_then(FixedOffset::east_opt)
                .ok_or_else(|| Error::runtime("invalid timezone offset"))?;
            render(utc.with_timezone(&offset), pattern)
        }
    }

    /// Render an adjusted timestamp with pre-parsed strftime items.
    pub fn format_items(
        &self,
        items: &[Item<'static>],
        adjusted_start: i64,
        offset: i64,
    ) -> Result<String> {
        let utc_seconds = adjusted_start
            .checked_sub(offset)
            .ok_or_else(|| Error::runtime("timestamp underflow while formatting logfile name"))?;
        let utc = Utc
            .timestamp_opt(utc_seconds, 0)
            .single()
            .ok_or_else(|| Error::runtime("timestamp is outside the supported range"))?;

        if self.local {
            render_items(utc.with_timezone(&Local), items)
        } else {
            let offset = i32::try_from(offset)
                .ok()
                .and_then(FixedOffset::east_opt)
                .ok_or_else(|| Error::runtime("invalid timezone offset"))?;
            render_items(utc.with_timezone(&offset), items)
        }
    }
}

/// A strftime pattern parsed once and reused across rotations.
///
/// The pattern is leaked into the process-lifetime static allocation so chrono's
/// borrowed `Item` values can be stored without self-referential unsafe code.
pub struct Formatter {
    pattern: &'static str,
    items: Vec<Item<'static>>,
}

impl std::fmt::Debug for Formatter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Formatter")
            .field("pattern", &self.pattern)
            .finish_non_exhaustive()
    }
}

impl Formatter {
    pub fn new(pattern: &str) -> Self {
        let pattern: &'static str = Box::leak(pattern.to_owned().into_boxed_str());
        let items = StrftimeItems::new(pattern).collect();
        Self { pattern, items }
    }

    pub fn pattern(&self) -> &str {
        self.pattern
    }

    pub fn format(&self, zone: &Zone, adjusted_start: i64, offset: i64) -> Result<String> {
        zone.format_items(&self.items, adjusted_start, offset)
    }
}

fn render<Tz>(dt: DateTime<Tz>, pattern: &str) -> Result<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let mut output = String::new();
    write!(output, "{}", dt.format(pattern))
        .map_err(|_| Error::runtime(format!("invalid strftime pattern {pattern:?}")))?;
    Ok(output)
}

fn render_items<Tz>(dt: DateTime<Tz>, items: &[Item<'static>]) -> Result<String>
where
    Tz: TimeZone,
    Tz::Offset: Display,
{
    let mut output = String::new();
    write!(output, "{}", dt.format_with_items(items.iter()))
        .map_err(|_| Error::runtime("invalid strftime pattern"))?;
    Ok(output)
}

/// Validate a strftime pattern before it is used at runtime.
pub fn validate_pattern(pattern: &str) -> Result<()> {
    for item in StrftimeItems::new(pattern) {
        if matches!(item, Item::Error) {
            return Err(Error::usage(format!(
                "invalid strftime conversion in logfile pattern {pattern:?}"
            )));
        }
    }
    Ok(())
}

/// Return true when a path contains a literal percent sign.
#[cfg(unix)]
pub fn contains_percent(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().contains(&b'%')
}

#[cfg(not(unix))]
pub fn contains_percent(path: &Path) -> bool {
    path.to_string_lossy().contains('%')
}

/// Round a value down to a multiple of `step` (works for negative values).
pub(crate) fn align_down(value: i64, step: u64) -> i64 {
    assert!(step > 0, "rotation step must be positive");
    let step = i64::try_from(step).expect("rotation step must fit in i64");
    value.div_euclid(step) * step
}

fn civil_period<Tz: TimeZone>(tz: &Tz, now_utc: i64, interval: u64) -> Result<Period> {
    let now = Utc
        .timestamp_opt(now_utc, 0)
        .single()
        .ok_or_else(|| Error::runtime("timestamp is outside the supported range"))?
        .with_timezone(tz);
    let wall_seconds = now.naive_local().and_utc().timestamp();
    let start_wall = align_down(wall_seconds, interval);
    let end_wall = start_wall.saturating_add(interval as i64);

    let start = local_from_wall(tz, start_wall)?;
    let end = local_from_wall(tz, end_wall)?;
    let offset = start.offset().fix().local_minus_utc() as i64;
    let start_utc = start.timestamp();

    Ok(Period {
        start_utc,
        start_adjusted: start_utc.saturating_add(offset),
        offset_seconds: offset,
        end_utc: end.timestamp(),
    })
}

fn local_from_wall<Tz: TimeZone>(tz: &Tz, wall_seconds: i64) -> Result<DateTime<Tz>> {
    let naive = DateTime::from_timestamp(wall_seconds, 0)
        .ok_or_else(|| Error::runtime("timestamp is outside the supported range"))?
        .naive_utc();

    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Ok(dt),
        LocalResult::Ambiguous(earliest, _) => Ok(earliest),
        LocalResult::None => {
            // A spring-forward gap can swallow a boundary such as 02:00.
            // Advance to the first valid local instant after the gap.
            for hours in 1..=3 {
                let shifted = naive + Duration::hours(hours);
                if let Some(dt) = tz.from_local_datetime(&shifted).earliest() {
                    return Ok(dt);
                }
            }
            Err(Error::runtime(
                "could not resolve local time around a DST transition",
            ))
        }
    }
}

/// Literal logfile name plus Apache's `.nnnnnnnnnn` timestamp suffix.
pub fn timestamp_path(root: &Path, adjusted_start: i64) -> PathBuf {
    append_suffix(root, &format!(".{adjusted_start:010}"))
}

/// Append a suffix to the file name itself, not to the parent directory.
pub fn append_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Circular naming: base, base.1, ... base.(n-1).
pub fn circular_path(root: &Path, current: Option<usize>, count: usize) -> (PathBuf, usize) {
    match current {
        None => (root.to_path_buf(), 0),
        Some(number) if number + 1 >= count => (root.to_path_buf(), 0),
        Some(number) => {
            let next = number + 1;
            (append_suffix(root, &format!(".{next}")), next)
        }
    }
}

/// Pick a name that does not exist yet by appending `.1`, `.2`, ...
///
/// This is intentionally used when a rotation computes the same name as the
/// current file, which Apache rotatelogs would silently overwrite.
pub fn unique_path(base: &Path) -> PathBuf {
    unique_path_from(base, 1).0
}

/// Like [`unique_path`], but starts scanning at `start` and returns the next
/// suffix to try. This lets the writer avoid rescanning `.1`..`.N` on every
/// size rotation.
pub fn unique_path_from(base: &Path, start: u64) -> (PathBuf, u64) {
    if !occupied(base) {
        return (base.to_path_buf(), 1);
    }
    unique_path_after(base, start)
}

/// Avoid a directory scan for the usual case: a fresh timestamp base.
pub fn unique_path_for_new_base(base: &Path) -> (PathBuf, u64) {
    if !occupied(base) {
        return (base.to_path_buf(), 1);
    }
    unique_path_after(base, next_suffix(base))
}

/// Like [`unique_path_from`], but assumes `base` itself is already occupied.
/// This avoids an extra `stat` when the writer already has a suffix cursor for
/// the same base name.
pub fn unique_path_after(base: &Path, start: u64) -> (PathBuf, u64) {
    let mut number = start.max(1);
    loop {
        let candidate = append_suffix(base, &format!(".{number}"));
        if !occupied(&candidate) {
            return (candidate, number.saturating_add(1));
        }
        number = number.saturating_add(1);
    }
}

/// Return the next numeric suffix for `base` by scanning its directory once.
///
/// This is used on the first size rotation after a restart (or when the base
/// name changes) so a process with thousands of existing archives does not have
/// to stat every `.1`..`.N` candidate.
pub fn next_suffix(base: &Path) -> u64 {
    let Some(name) = base.file_name().and_then(|name| name.to_str()) else {
        return 1;
    };
    let parent = base
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let prefix = format!("{name}.");
    let mut max = 0_u64;

    let Ok(entries) = std::fs::read_dir(parent) else {
        return 1;
    };
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Some(rest) = file_name.strip_prefix(&prefix) else {
            continue;
        };
        if !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()) {
            if let Ok(number) = rest.parse::<u64>() {
                max = max.max(number);
            }
        }
    }

    max.saturating_add(1)
}

fn occupied(path: &Path) -> bool {
    // symlink_metadata also catches dangling symlinks, which `exists` misses.
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn fresh_base_fast_path_preserves_collision_rules() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("fresh.log");
        fs::write(append_suffix(&base, ".9"), b"archive").unwrap();
        // An absent base is still usable even if old numbered archives exist.
        assert_eq!(unique_path_for_new_base(&base), (base.clone(), 1));
        fs::write(&base, b"current").unwrap();
        assert_eq!(
            unique_path_for_new_base(&base),
            (append_suffix(&base, ".10"), 11)
        );
        assert_eq!(fs::read(&base).unwrap(), b"current");
        #[cfg(unix)]
        {
            fs::remove_file(&base).unwrap();
            std::os::unix::fs::symlink(dir.path().join("absent"), &base).unwrap();
            assert_eq!(
                unique_path_for_new_base(&base).0,
                append_suffix(&base, ".10")
            );
        }
    }

    #[test]
    fn alignment_uses_floor_division() {
        assert_eq!(align_down(0, 60), 0);
        assert_eq!(align_down(59, 60), 0);
        assert_eq!(align_down(60, 60), 60);
        assert_eq!(align_down(61, 60), 60);
        assert_eq!(align_down(-1, 60), -60);
        assert_eq!(align_down(1_700_000_000, 3_600), 1_699_999_200);
    }

    #[test]
    #[should_panic(expected = "rotation step must be positive")]
    fn alignment_rejects_zero_step() {
        align_down(0, 0);
    }

    #[test]
    fn detects_percent_in_path() {
        assert!(contains_percent(Path::new("access.%Y.log")));
        assert!(!contains_percent(Path::new("access.log")));
    }

    #[test]
    fn validates_patterns() {
        validate_pattern("%Y-%m-%d-%H_%M_%S.log").unwrap();
        validate_pattern("100%% literal.log").unwrap();
        assert!(validate_pattern("broken-%Q.log").is_err());
        assert!(validate_pattern("trailing-%").is_err());
    }

    #[test]
    fn formats_utc_pattern() {
        let zone = Zone::new(false, 0);
        let offset = zone.offset_at(1_700_000_000);
        let rendered = zone
            .format("%Y-%m-%d %H:%M:%S", 1_700_000_000, offset)
            .unwrap();
        assert_eq!(rendered, "2023-11-14 22:13:20");
    }

    #[test]
    fn fixed_offset_period_uses_utc_boundaries() {
        let zone = Zone::new(false, 9 * 60);
        let period = zone.period(1_700_000_000, 3_600).unwrap();
        assert_eq!(period.start_utc, 1_699_999_200);
        assert_eq!(period.start_adjusted, 1_699_999_200 + 9 * 3600);
        assert_eq!(period.offset_seconds, 9 * 3600);
        assert_eq!(period.end_utc, 1_700_002_800);
    }

    #[test]
    fn local_civil_period_handles_spring_forward() {
        use chrono_tz::America::New_York;

        let now = New_York
            .with_ymd_and_hms(2024, 3, 10, 1, 30, 0)
            .earliest()
            .unwrap()
            .timestamp();
        let period = civil_period(&New_York, now, 86_400).unwrap();

        let start = New_York
            .with_ymd_and_hms(2024, 3, 10, 0, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();
        let end = New_York
            .with_ymd_and_hms(2024, 3, 11, 0, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();

        assert_eq!(period.start_utc, start);
        assert_eq!(period.offset_seconds, -5 * 3600);
        assert_eq!(period.end_utc, end);
        assert_eq!(end - start, 23 * 3600);
    }

    #[test]
    fn local_civil_period_handles_fall_back() {
        use chrono_tz::America::New_York;

        let now = New_York
            .with_ymd_and_hms(2024, 11, 3, 12, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();
        let period = civil_period(&New_York, now, 86_400).unwrap();

        let start = New_York
            .with_ymd_and_hms(2024, 11, 3, 0, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();
        let end = New_York
            .with_ymd_and_hms(2024, 11, 4, 0, 0, 0)
            .earliest()
            .unwrap()
            .timestamp();

        assert_eq!(period.start_utc, start);
        assert_eq!(period.offset_seconds, -4 * 3600);
        assert_eq!(period.end_utc, end);
        assert_eq!(end - start, 25 * 3600);
    }

    #[test]
    fn formats_fixed_offset_pattern() {
        let zone = Zone::new(false, 9 * 60);
        let offset = zone.offset_at(1_700_000_000);
        assert_eq!(offset, 9 * 3600);

        // The adjusted timestamp itself represents the wall clock.
        let rendered = zone
            .format("%Y-%m-%d %H:%M:%S %z", 1_700_000_000, offset)
            .unwrap();
        assert_eq!(rendered, "2023-11-14 22:13:20 +0900");
    }

    #[test]
    fn builds_timestamp_path() {
        let path = timestamp_path(Path::new("/var/log/access.log"), 1_700_000_000);
        assert_eq!(path, PathBuf::from("/var/log/access.log.1700000000"));
    }

    #[test]
    fn parsed_formatter_reuses_items() {
        let zone = Zone::new(false, 0);
        let formatter = Formatter::new("%Y-%m-%d %H:%M:%S");
        let first = formatter.format(&zone, 1_700_000_000, 0).unwrap();
        let second = formatter.format(&zone, 1_700_000_000, 0).unwrap();
        assert_eq!(first, "2023-11-14 22:13:20");
        assert_eq!(second, first);
    }

    #[test]
    fn circular_names_cycle() {
        let root = Path::new("access.log");
        assert_eq!(
            circular_path(root, None, 3),
            (PathBuf::from("access.log"), 0)
        );
        assert_eq!(
            circular_path(root, Some(0), 3),
            (PathBuf::from("access.log.1"), 1)
        );
        assert_eq!(
            circular_path(root, Some(1), 3),
            (PathBuf::from("access.log.2"), 2)
        );
        assert_eq!(
            circular_path(root, Some(2), 3),
            (PathBuf::from("access.log"), 0)
        );
        assert_eq!(
            circular_path(root, Some(0), 1),
            (PathBuf::from("access.log"), 0)
        );
    }

    #[test]
    fn unique_path_skips_existing_names() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("access.log");
        assert_eq!(unique_path(&base), base);

        std::fs::write(&base, b"current").unwrap();
        std::fs::write(dir.path().join("access.log.1"), b"old").unwrap();
        assert_eq!(unique_path(&base), dir.path().join("access.log.2"));
    }

    #[test]
    fn next_suffix_scans_existing_archives() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("access.log");
        for suffix in [".1", ".3", ".10", ".not-a-number"] {
            std::fs::write(append_suffix(&base, suffix), b"x").unwrap();
        }
        std::fs::write(dir.path().join("other.log.99"), b"x").unwrap();

        assert_eq!(next_suffix(&base), 11);
    }

    #[test]
    fn unique_path_from_returns_the_next_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("access.log");
        std::fs::write(&base, b"current").unwrap();
        std::fs::write(dir.path().join("access.log.1"), b"one").unwrap();
        std::fs::write(dir.path().join("access.log.2"), b"two").unwrap();

        let (path, next) = unique_path_from(&base, 1);
        assert_eq!(path, dir.path().join("access.log.3"));
        assert_eq!(next, 4);

        let (path, next) = unique_path_from(&base, 3);
        assert_eq!(path, dir.path().join("access.log.3"));
        assert_eq!(next, 4);

        let (path, next) = unique_path_after(&base, 3);
        assert_eq!(path, dir.path().join("access.log.3"));
        assert_eq!(next, 4);
    }
}
