use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::config::Config;
use crate::error::{Error, IoContext, Result};
use crate::link;
use crate::maintenance::Maintenance;
use crate::naming::{self, Zone};
use crate::program;

/// How long to keep writing to the old file if opening a new one fails.
const ROTATION_RETRY_SECONDS: i64 = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reason {
    New,
    Time,
    Size,
}

impl Reason {
    fn as_str(self) -> &'static str {
        match self {
            Self::New => "open",
            Self::Time => "time interval",
            Self::Size => "size limit",
        }
    }
}

struct CurrentFile {
    path: PathBuf,
    file: File,
    size: u64,
}

/// The rotating writer itself. All methods take the current UTC timestamp so
/// rotation can be tested deterministically without sleeping.
pub struct Writer {
    cfg: Config,
    zone: Zone,
    current: Option<CurrentFile>,
    period_end: i64,
    file_number: Option<usize>,
    /// First timer deadline for -c when no file is open yet.
    empty_next: Option<i64>,
    retry_at: i64,
    maintenance: Option<Maintenance>,
    echo_enabled: bool,
}

impl Writer {
    pub fn new(cfg: Config) -> Result<Self> {
        cfg.validate()?;
        let zone = Zone::new(cfg.local_time, cfg.offset_minutes);
        let maintenance = if cfg.compress || cfg.retain.is_some() {
            Some(
                Maintenance::spawn(cfg.compress, cfg.retain, cfg.verbose)
                    .context("start maintenance worker")?,
            )
        } else {
            None
        };

        Ok(Self {
            cfg,
            zone,
            current: None,
            period_end: 0,
            file_number: None,
            empty_next: None,
            retry_at: 0,
            maintenance,
            echo_enabled: true,
        })
    }

    /// Create the initial file before any log data arrives (-f / -c).
    pub fn open_initial(&mut self, now_utc: i64) -> Result<()> {
        if self.current.is_none() {
            self.rotate(Reason::New, now_utc)?;
        }
        Ok(())
    }

    /// Append one chunk of log data, rotating first if necessary.
    pub fn write(&mut self, data: &[u8], now_utc: i64) -> Result<()> {
        if data.is_empty() {
            return Ok(());
        }

        self.prepare_write(data.len(), now_utc)?;

        {
            let current = self
                .current
                .as_mut()
                .expect("prepare_write guarantees an open file");
            current
                .file
                .write_all(data)
                .with_context(|| format!("write to logfile {}", current.path.display()))?;
            current.size = current.size.saturating_add(data.len() as u64);
        }

        if self.cfg.echo {
            self.echo(data);
        }
        Ok(())
    }

    /// Periodic housekeeping for -c (empty files).
    ///
    /// Apache waits one full interval after startup before creating the first
    /// empty file, then follows the normal interval boundaries. We mirror that
    /// with `empty_next` instead of opening the current interval immediately.
    pub fn tick(&mut self, now_utc: i64) -> Result<()> {
        if !self.cfg.create_empty {
            return Ok(());
        }
        let Some(interval) = self.cfg.rotation.interval_secs else {
            // -c is inert for size-only rotation, as in the original.
            return Ok(());
        };
        debug_assert!(interval > 0);

        let now_adjusted = self.zone.adjust(now_utc);
        if self.current.is_none() {
            let deadline = *self
                .empty_next
                .get_or_insert_with(|| now_adjusted.saturating_add(interval as i64));
            if now_adjusted >= deadline {
                self.empty_next = None;
                self.rotate(Reason::New, now_utc)?;
            }
            return Ok(());
        }

        if now_adjusted >= self.period_end {
            self.try_rotate(Reason::Time, now_utc)?;
        }
        Ok(())
    }

    /// Flush and close the current file, then let maintenance finish.
    pub fn finish(&mut self) -> Result<()> {
        let flush_result = match self.current.take() {
            Some(mut current) => current
                .file
                .flush()
                .with_context(|| format!("flush logfile {}", current.path.display())),
            None => Ok(()),
        };

        if let Some(maintenance) = self.maintenance.take() {
            maintenance.finish();
        }
        flush_result
    }

    fn prepare_write(&mut self, length: usize, now_utc: i64) -> Result<()> {
        if self.current.is_none() {
            self.rotate(Reason::New, now_utc)?;
            // Fall through: a restart may find an existing current-period
            // file that is already at its size limit. Rotating again here
            // avoids appending one more chunk past the configured maximum.
        }

        if let Some(interval) = self.cfg.rotation.interval_secs {
            debug_assert!(interval > 0);
            if self.zone.adjust(now_utc) >= self.period_end {
                return self.try_rotate(Reason::Time, now_utc);
            }
        }

        if let Some(limit) = self.cfg.rotation.size_bytes {
            let size = self.current.as_ref().expect("checked above").size;
            if size > 0 && size.saturating_add(length as u64) > limit {
                self.try_rotate(Reason::Size, now_utc)?;
            }
        }
        Ok(())
    }

    fn try_rotate(&mut self, reason: Reason, now_utc: i64) -> Result<()> {
        let now_adjusted = self.zone.adjust(now_utc);
        if now_adjusted < self.retry_at {
            return Ok(());
        }

        match self.rotate(reason, now_utc) {
            Ok(()) => {
                self.retry_at = 0;
                Ok(())
            }
            Err(error) if self.current.is_some() => {
                // Keep logging into the old file rather than losing records.
                let path = self
                    .current
                    .as_ref()
                    .map(|current| current.path.display().to_string())
                    .unwrap_or_default();
                eprintln!("rotalog: rotation failed; continuing with {path}: {error}");
                self.retry_at = now_adjusted.saturating_add(ROTATION_RETRY_SECONDS);
                Ok(())
            }
            Err(error) => Err(error),
        }
    }

    fn rotate(&mut self, reason: Reason, now_utc: i64) -> Result<()> {
        let previous = self.current.as_ref().map(|current| current.path.clone());
        let initial = previous.is_none();

        let offset = self.zone.offset_at(now_utc);
        let now_adjusted = now_utc.saturating_add(offset);

        let mut start = match self.cfg.rotation.interval_secs {
            Some(interval) => naming::align_down(now_adjusted, interval),
            None => now_adjusted,
        };
        let mut end = self
            .cfg
            .rotation
            .interval_secs
            .map(|interval| start.saturating_add(interval as i64))
            .unwrap_or(0);

        // A size-based rotation in the middle of an interval is named with the
        // actual rotation time, like Apache rotatelogs, rather than with the
        // interval start.
        if reason == Reason::Size
            && self.cfg.rotation.interval_secs.is_some()
            && start < self.period_end
        {
            start = now_adjusted;
        }

        // If we crossed a boundary because the process was busy/suspended,
        // recompute the interval end for the new start.
        if reason == Reason::Time && start >= self.period_end {
            end = start.saturating_add(self.cfg.rotation.interval_secs.unwrap_or(0) as i64);
        }

        let (path, truncate, next_file_number) =
            self.plan_target(reason, initial, start, offset, previous.as_deref())?;

        if self.cfg.verbose {
            eprintln!(
                "rotalog: opening {} ({}{})",
                path.display(),
                reason.as_str(),
                if truncate { ", truncate" } else { "" }
            );
        }

        let file = open_file(&path, truncate, self.cfg.create_dirs)
            .with_context(|| format!("open logfile {}", path.display()))?;
        let size = if truncate {
            0
        } else {
            file.metadata()
                .with_context(|| format!("inspect logfile {}", path.display()))?
                .len()
        };

        if let Some(link_path) = &self.cfg.link {
            if let Err(error) =
                link::update(link_path, &path, self.cfg.create_dirs, self.cfg.verbose)
            {
                // A broken convenience link must not stop the actual logging.
                eprintln!(
                    "rotalog: could not update link {}: {error}",
                    link_path.display()
                );
            }
        }

        if let Some(program_path) = &self.cfg.program {
            if let Err(error) = program::spawn(program_path, &path, previous.as_deref()) {
                eprintln!(
                    "rotalog: could not run post-rotate program {}: {error}",
                    program_path.display()
                );
            }
        }

        if let Some(old) = self.current.take() {
            let CurrentFile {
                path: old_path,
                file,
                size: _,
            } = old;
            // Close the old descriptor before a background compressor opens
            // or removes the file.
            drop(file);
            if Some(old_path.as_path()) != Some(path.as_path()) && self.cfg.circular.is_none() {
                if let Some(maintenance) = &self.maintenance {
                    maintenance.retire(old_path);
                }
            }
        }

        if self.cfg.verbose {
            match previous.as_deref() {
                Some(previous) => eprintln!(
                    "rotalog: rotated {} -> {} ({})",
                    previous.display(),
                    path.display(),
                    reason.as_str()
                ),
                None => eprintln!("rotalog: created {} ({})", path.display(), reason.as_str()),
            }
        }

        self.current = Some(CurrentFile { path, file, size });
        self.period_end = end;
        self.file_number = next_file_number;
        self.empty_next = None;
        Ok(())
    }

    fn plan_target(
        &self,
        reason: Reason,
        initial: bool,
        start: i64,
        offset: i64,
        previous: Option<&Path>,
    ) -> Result<(PathBuf, bool, Option<usize>)> {
        if self.cfg.truncate {
            // -t keeps a literal name as-is; strftime patterns are still
            // expanded so date-based names are respected.
            let path = if self.cfg.pattern().is_some() {
                self.render_name(start, offset)?
            } else {
                self.cfg.logfile.clone()
            };
            return Ok((path, true, None));
        }

        if let Some(count) = self.cfg.circular {
            let (path, number) = naming::circular_path(&self.cfg.logfile, self.file_number, count);
            // The initial file is appended to unless -t was requested (which
            // is rejected with -n). Every later file is truncated.
            return Ok((path, !initial, Some(number)));
        }

        let mut path = self.render_name(start, offset)?;
        if reason == Reason::Size {
            // A coarse strftime pattern can produce the same name several
            // times in one interval. Never append new records to an older
            // archive: pick the next free suffix instead.
            path = naming::unique_path(&path);
        } else if previous == Some(path.as_path()) {
            // Clock went backwards, or the pattern does not distinguish the
            // current period. Do not truncate the file we are writing to.
            path = naming::unique_path(&path);
        }

        let truncate = self.cfg.truncate_rotated && !initial;
        Ok((path, truncate, None))
    }

    fn render_name(&self, start: i64, offset: i64) -> Result<PathBuf> {
        match self.cfg.pattern() {
            Some(pattern) => {
                let rendered = self.zone.format(pattern, start, offset)?;
                if rendered.is_empty() {
                    return Err(Error::runtime(
                        "strftime logfile pattern produced an empty path",
                    ));
                }
                Ok(PathBuf::from(rendered))
            }
            None => Ok(naming::timestamp_path(&self.cfg.logfile, start)),
        }
    }

    fn echo(&mut self, data: &[u8]) {
        if !self.echo_enabled {
            return;
        }

        let mut stdout = io::stdout().lock();
        if let Err(error) = stdout.write_all(data).and_then(|()| stdout.flush()) {
            self.echo_enabled = false;
            if error.kind() != io::ErrorKind::BrokenPipe {
                eprintln!("rotalog: could not echo to stdout: {error}");
            }
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        if let Some(mut current) = self.current.take() {
            let _ = current.file.flush();
        }
        if let Some(maintenance) = self.maintenance.take() {
            maintenance.finish();
        }
    }
}

fn open_file(path: &Path, truncate: bool, create_dirs: bool) -> io::Result<File> {
    if create_dirs {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
    }

    let mut options = OpenOptions::new();
    options.create(true).write(true);
    if truncate {
        options.truncate(true);
    } else {
        options.append(true);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Rotation;
    use std::fs;

    const T0: i64 = 1_700_000_000;

    fn config(logfile: PathBuf) -> Config {
        Config {
            logfile,
            ..Config::default()
        }
    }

    fn entries(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn size_rotation_uses_timestamp_suffix() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(10),
        };

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"12345678", T0).unwrap();
        writer.write(b"abcdefgh", T0 + 1).unwrap();
        writer.finish().unwrap();

        let first = naming::timestamp_path(&logfile, T0);
        let second = naming::timestamp_path(&logfile, T0 + 1);
        assert_eq!(fs::read(first).unwrap(), b"12345678");
        assert_eq!(fs::read(second).unwrap(), b"abcdefgh");
    }

    #[test]
    fn same_second_size_rotation_never_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(10),
        };

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"12345678", T0).unwrap();
        writer.write(b"abcdefgh", T0).unwrap();
        writer.write(b"ABCDEFGH", T0).unwrap();
        writer.finish().unwrap();

        let first = naming::timestamp_path(&logfile, T0);
        let second = naming::append_suffix(&first, ".1");
        let third = naming::append_suffix(&first, ".2");
        assert_eq!(fs::read(first).unwrap(), b"12345678");
        assert_eq!(fs::read(second).unwrap(), b"abcdefgh");
        assert_eq!(fs::read(third).unwrap(), b"ABCDEFGH");
    }

    #[test]
    fn time_rotation_aligns_to_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: Some(60),
            size_bytes: None,
        };

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"a", T0).unwrap();
        writer.write(b"b", T0 + 30).unwrap();
        writer.write(b"c", T0 + 60).unwrap();
        writer.finish().unwrap();

        let start = naming::align_down(T0, 60);
        let next = start + 60;
        let first = naming::timestamp_path(&logfile, start);
        let second = naming::timestamp_path(&logfile, next);
        assert_eq!(fs::read(first).unwrap(), b"ab");
        assert_eq!(fs::read(second).unwrap(), b"c");
    }

    #[test]
    fn truncate_mode_takes_precedence_over_circular_and_truncate_rotated() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: Some(1),
            size_bytes: None,
        };
        cfg.truncate = true;
        cfg.truncate_rotated = true;
        cfg.circular = Some(3);

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"first", T0).unwrap();
        writer.write(b"second", T0 + 2).unwrap();
        writer.finish().unwrap();

        assert_eq!(fs::read(&logfile).unwrap(), b"second");
        assert_eq!(entries(dir.path()), vec!["access.log"]);
    }

    #[test]
    fn circular_names_cycle_and_truncate() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(1),
        };
        cfg.circular = Some(2);

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"a", T0).unwrap();
        writer.write(b"b", T0 + 1).unwrap();
        writer.write(b"c", T0 + 2).unwrap();
        writer.finish().unwrap();

        assert_eq!(fs::read(&logfile).unwrap(), b"c");
        assert_eq!(
            fs::read(naming::append_suffix(&logfile, ".1")).unwrap(),
            b"b"
        );
        assert_eq!(entries(dir.path()), vec!["access.log", "access.log.1"]);
    }

    #[test]
    fn truncate_rotated_truncates_reopened_period_file() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.%d.log");
        let day_start = naming::align_down(T0, 86_400);
        let next_day = day_start + 86_400;
        let zone = Zone::new(false, 0);
        let first = dir
            .path()
            .join(zone.format("access.%d.log", day_start, 0).unwrap());
        let second = dir
            .path()
            .join(zone.format("access.%d.log", next_day, 0).unwrap());
        fs::write(&second, b"stale").unwrap();

        let mut cfg = config(logfile);
        cfg.rotation = Rotation {
            interval_secs: Some(86_400),
            size_bytes: None,
        };
        cfg.truncate_rotated = true;

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"first", day_start + 10).unwrap();
        writer.write(b"second", next_day + 10).unwrap();
        writer.finish().unwrap();

        assert_eq!(fs::read(first).unwrap(), b"first");
        assert_eq!(fs::read(second).unwrap(), b"second");
    }

    #[test]
    fn coarse_pattern_size_rotations_get_unique_names() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.%Y%m%d.log");
        let mut cfg = config(logfile);
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(3),
        };

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"aa", T0).unwrap();
        writer.write(b"bb", T0 + 1).unwrap();
        writer.write(b"cc", T0 + 2).unwrap();
        writer.finish().unwrap();

        let names = entries(dir.path());
        assert_eq!(names.len(), 3);
        assert!(names[0].starts_with("access."));
        let contents: Vec<Vec<u8>> = names
            .iter()
            .map(|name| fs::read(dir.path().join(name)).unwrap())
            .collect();
        assert_eq!(
            contents,
            vec![b"aa".to_vec(), b"bb".to_vec(), b"cc".to_vec()]
        );
    }

    #[test]
    fn restart_does_not_append_past_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let first = naming::timestamp_path(&logfile, T0);
        fs::write(&first, b"12345678").unwrap();

        let mut cfg = config(logfile);
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(10),
        };

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"abcdefgh", T0).unwrap();
        writer.finish().unwrap();

        let rotated = naming::append_suffix(&first, ".1");
        assert_eq!(fs::read(&first).unwrap(), b"12345678");
        assert_eq!(fs::read(rotated).unwrap(), b"abcdefgh");
    }

    #[test]
    fn circular_restart_rotates_before_writing_past_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        fs::write(&logfile, b"12345678").unwrap();

        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(10),
        };
        cfg.circular = Some(3);

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"abcdefgh", T0).unwrap();
        writer.finish().unwrap();

        assert_eq!(fs::read(&logfile).unwrap(), b"12345678");
        assert_eq!(
            fs::read(naming::append_suffix(&logfile, ".1")).unwrap(),
            b"abcdefgh"
        );
    }

    #[test]
    fn link_follows_rotations() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let link_path = dir.path().join("current.log");
        let mut cfg = config(logfile);
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(10),
        };
        cfg.link = Some(link_path.clone());

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"12345678", T0).unwrap();
        assert_eq!(fs::read(&link_path).unwrap(), b"12345678");
        writer.write(b"abcdefgh", T0 + 1).unwrap();
        assert_eq!(fs::read(&link_path).unwrap(), b"abcdefgh");
        writer.finish().unwrap();
    }

    #[test]
    fn create_empty_opens_files_without_data() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: Some(60),
            size_bytes: None,
        };
        cfg.create_empty = true;

        let mut writer = Writer::new(cfg).unwrap();
        writer.tick(T0).unwrap();
        assert!(
            entries(dir.path()).is_empty(),
            "must wait one full interval"
        );
        writer.tick(T0 + 60).unwrap();
        writer.tick(T0 + 120).unwrap();
        writer.finish().unwrap();

        let first = naming::timestamp_path(&logfile, naming::align_down(T0 + 60, 60));
        let second = naming::timestamp_path(&logfile, naming::align_down(T0 + 120, 60));
        assert!(first.exists());
        assert!(second.exists());
        assert_eq!(fs::metadata(first).unwrap().len(), 0);
        assert_eq!(fs::metadata(second).unwrap().len(), 0);
    }

    #[test]
    fn create_empty_is_inert_without_a_time_interval() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile);
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(1024),
        };
        cfg.create_empty = true;

        let mut writer = Writer::new(cfg).unwrap();
        writer.tick(T0).unwrap();
        writer.tick(T0 + 3600).unwrap();
        writer.finish().unwrap();

        assert!(entries(dir.path()).is_empty());
    }

    #[test]
    fn compresses_and_retains_rotated_files() {
        let dir = tempfile::tempdir().unwrap();
        let logfile = dir.path().join("access.log");
        let mut cfg = config(logfile.clone());
        cfg.rotation = Rotation {
            interval_secs: None,
            size_bytes: Some(3),
        };
        cfg.compress = true;
        cfg.retain = Some(1);

        let mut writer = Writer::new(cfg).unwrap();
        writer.write(b"aa", T0).unwrap();
        writer.write(b"bb", T0 + 1).unwrap();
        writer.write(b"cc", T0 + 2).unwrap();
        writer.finish().unwrap();

        let first = naming::timestamp_path(&logfile, T0);
        let second = naming::timestamp_path(&logfile, T0 + 1);
        let current = naming::timestamp_path(&logfile, T0 + 2);

        assert!(!first.exists());
        assert!(!naming::append_suffix(&first, ".gz").exists());
        assert!(naming::append_suffix(&second, ".gz").exists());
        assert_eq!(fs::read(current).unwrap(), b"cc");
    }
}
