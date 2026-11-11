use std::path::{Path, PathBuf};

use crate::error::{Error, Result};
use crate::naming;

/// Largest accepted UTC offset. Real-world zones are within UTC-12..UTC+14;
/// the symmetric ±14 hour limit also satisfies chrono's `FixedOffset` range.
pub const MAX_OFFSET_MINUTES: i32 = 14 * 60;

/// Default gzip level for log archives. Level 3 is a speed/size compromise;
/// level 1 is fastest and 6-9 produce smaller archives.
pub const DEFAULT_COMPRESS_LEVEL: u8 = 3;

/// Rotation limits. Time and size may be combined; whichever is reached first
/// wins.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rotation {
    /// Rotate when the current interval ends, in seconds.
    pub interval_secs: Option<u64>,
    /// Rotate when the file would grow past this many bytes.
    pub size_bytes: Option<u64>,
}

/// Fully validated runtime configuration.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Base path or `strftime(3)`-style pattern.
    pub logfile: PathBuf,
    pub rotation: Rotation,
    /// Minutes east of UTC used for interval alignment and naming.
    pub offset_minutes: i32,
    /// Use the machine's local time zone instead of a fixed UTC offset.
    pub local_time: bool,

    /// Update a hard link (or symlink fallback) to the current file.
    pub link: Option<PathBuf>,
    /// Program invoked after every file is opened. Receives the new filename
    /// and, on rotation, the previous filename.
    pub program: Option<PathBuf>,

    /// Open the first file immediately instead of waiting for the first byte.
    pub force_open: bool,
    /// Create missing parent directories.
    pub create_dirs: bool,
    /// Truncate the log path instead of rotating it.
    pub truncate: bool,
    /// Truncate an existing target when it is opened as a rotated file.
    pub truncate_rotated: bool,
    /// Diagnostic output on stderr.
    pub verbose: bool,
    /// Echo log data to stdout.
    pub echo: bool,
    /// Create a file for every interval even when no data arrives.
    pub create_empty: bool,
    /// Circular filenames: base, base.1, ... base.(n-1).
    pub circular: Option<usize>,

    /// Compress rotated files asynchronously with gzip.
    pub compress: bool,
    /// Optional gzip level (1-9). `None` uses [`DEFAULT_COMPRESS_LEVEL`].
    pub compress_level: Option<u8>,
    /// Keep at most this many rotated files created by this process.
    pub retain: Option<usize>,
}

impl Config {
    /// The base path as a `strftime` pattern, if it contains `%`.
    pub fn pattern(&self) -> Option<&str> {
        if naming::contains_percent(&self.logfile) {
            self.logfile.to_str()
        } else {
            None
        }
    }

    /// Effective gzip level, including the default when `-z` is enabled.
    pub fn effective_compress_level(&self) -> u8 {
        self.compress_level.unwrap_or(DEFAULT_COMPRESS_LEVEL)
    }

    /// Check invariants that are independent of argument order.
    pub fn validate(&self) -> Result<()> {
        if self.logfile.as_os_str().is_empty() {
            return Err(Error::usage("logfile path must not be empty"));
        }
        if self.rotation.interval_secs.is_none() && self.rotation.size_bytes.is_none() {
            return Err(Error::usage(
                "a rotation time or a maximum file size is required",
            ));
        }
        if self.rotation.interval_secs == Some(0) {
            return Err(Error::usage("rotation time must be greater than zero"));
        }
        if let Some(interval) = self.rotation.interval_secs {
            if interval > i64::MAX as u64 {
                return Err(Error::usage("rotation time is too large"));
            }
        }
        if self.rotation.size_bytes == Some(0) {
            return Err(Error::usage("maximum file size must be greater than zero"));
        }
        if !(-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&self.offset_minutes) {
            return Err(Error::usage(format!(
                "UTC offset must be between -{MAX_OFFSET_MINUTES} and {MAX_OFFSET_MINUTES} minutes"
            )));
        }
        if self.local_time && self.offset_minutes != 0 {
            return Err(Error::usage(
                "the UTC offset cannot be combined with -l/--local-time",
            ));
        }
        // Like Apache rotatelogs, -t takes precedence over -T and -n; the
        // other flags are accepted but do not change the truncate behavior.
        if let Some(n) = self.circular {
            if n == 0 {
                return Err(Error::usage("-n/--circular requires a positive count"));
            }
            if self.pattern().is_some() || naming::contains_percent(&self.logfile) {
                return Err(Error::usage(
                    "-n/--circular cannot be combined with a strftime logfile pattern",
                ));
            }
        }
        // -c is accepted with a size-only rotation, matching Apache; it is
        // inert because empty files are driven by a time interval.
        if self.compress {
            if self.truncate {
                return Err(Error::usage(
                    "-z/--compress cannot be combined with -t/--truncate",
                ));
            }
            if self.truncate_rotated {
                return Err(Error::usage(
                    "-z/--compress cannot be combined with -T/--truncate-rotated",
                ));
            }
            if self.circular.is_some() {
                return Err(Error::usage(
                    "-z/--compress cannot be combined with -n/--circular",
                ));
            }
        }
        if let Some(level) = self.compress_level {
            if !(1..=9).contains(&level) {
                return Err(Error::usage("--compress-level must be between 1 and 9"));
            }
            if !self.compress {
                return Err(Error::usage("--compress-level requires -z/--compress"));
            }
        }
        if self.retain.is_some() {
            if self.circular.is_some() {
                return Err(Error::usage(
                    "--retain cannot be combined with -n/--circular",
                ));
            }
            if self.truncate || self.truncate_rotated {
                return Err(Error::usage(
                    "--retain cannot be combined with -t/--truncate or -T/--truncate-rotated",
                ));
            }
        }
        if self.link.as_ref().is_some_and(|p| p.as_os_str().is_empty()) {
            return Err(Error::usage("link name must not be empty"));
        }
        if self
            .program
            .as_ref()
            .is_some_and(|p| p.as_os_str().is_empty())
        {
            return Err(Error::usage("program path must not be empty"));
        }
        if self.program.is_some() && (self.compress || self.retain.is_some()) {
            return Err(Error::usage(
                "-p/--program cannot be combined with -z/--compress or --retain: \
                 the post-rotate program may read the previous file asynchronously",
            ));
        }
        if self
            .link
            .as_ref()
            .is_some_and(|p| same_path(p, &self.logfile))
        {
            return Err(Error::usage(
                "link name must be different from the logfile path",
            ));
        }
        if naming::contains_percent(&self.logfile) {
            let pattern = self
                .pattern()
                .ok_or_else(|| Error::usage("strftime logfile patterns must be valid UTF-8"))?;
            naming::validate_pattern(pattern)?;
        }
        Ok(())
    }

    /// Print the effective configuration in the spirit of rotatelogs -v.
    pub fn dump(&self) {
        let interval = self
            .rotation
            .interval_secs
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_owned());
        let size = self
            .rotation
            .size_bytes
            .map(|v| v.to_string())
            .unwrap_or_else(|| "none".to_owned());

        eprintln!("rotalog configuration:");
        eprintln!("  logfile:              {}", self.logfile.display());
        eprintln!("  rotation interval:    {interval} seconds");
        eprintln!("  rotation size:        {size} bytes");
        eprintln!(
            "  time base:            {}",
            if self.local_time { "local" } else { "UTC" }
        );
        eprintln!("  UTC offset:           {} minutes", self.offset_minutes);
        eprintln!("  strftime pattern:     {}", self.pattern().is_some());
        eprintln!("  open immediately:     {}", self.force_open);
        eprintln!("  create directories:   {}", self.create_dirs);
        eprintln!("  truncate:             {}", self.truncate);
        eprintln!("  truncate rotated:     {}", self.truncate_rotated);
        eprintln!("  create empty:         {}", self.create_empty);
        eprintln!("  echo to stdout:       {}", self.echo);
        eprintln!(
            "  circular files:       {}",
            self.circular.map_or("none".to_owned(), |n| n.to_string())
        );
        eprintln!("  compress rotated:     {}", self.compress);
        eprintln!(
            "  compression level:    {}",
            if self.compress {
                self.effective_compress_level().to_string()
            } else {
                "n/a".to_owned()
            }
        );
        eprintln!(
            "  retain rotated:       {}",
            self.retain.map_or("all".to_owned(), |n| n.to_string())
        );
        eprintln!(
            "  link:                 {}",
            self.link
                .as_ref()
                .map_or("none".to_owned(), |p| p.display().to_string())
        );
        eprintln!(
            "  post-rotate program:  {}",
            self.program
                .as_ref()
                .map_or("none".to_owned(), |p| p.display().to_string())
        );
    }
}

fn same_path(a: &Path, b: &Path) -> bool {
    if a == b {
        return true;
    }
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base() -> Config {
        Config {
            logfile: PathBuf::from("access.log"),
            rotation: Rotation {
                interval_secs: Some(60),
                size_bytes: None,
            },
            ..Config::default()
        }
    }

    #[test]
    fn accepts_minimal_config() {
        base().validate().unwrap();
    }

    #[test]
    fn rejects_zero_interval_and_size() {
        let mut config = base();
        config.rotation.interval_secs = Some(0);
        assert!(config.validate().is_err());

        let mut config = base();
        config.rotation.interval_secs = None;
        config.rotation.size_bytes = Some(0);
        assert!(config.validate().is_err());
    }

    #[test]
    fn rejects_local_time_with_offset() {
        let mut config = base();
        config.local_time = true;
        config.offset_minutes = 60;
        assert!(config.validate().is_err());
    }

    #[test]
    fn truncate_takes_precedence_over_other_truncation_modes() {
        let mut config = base();
        config.truncate = true;
        config.truncate_rotated = true;
        config.validate().unwrap();

        config.circular = Some(3);
        config.validate().unwrap();
    }

    #[test]
    fn circular_still_rejects_strftime_pattern_with_truncate() {
        let mut config = base();
        config.truncate = true;
        config.circular = Some(3);
        config.logfile = PathBuf::from("access.%Y.log");
        assert!(config.validate().is_err());
    }

    #[test]
    fn circular_requires_literal_name() {
        let mut config = base();
        config.logfile = PathBuf::from("access.%Y.log");
        config.circular = Some(3);
        assert!(config.validate().is_err());
    }

    #[test]
    fn create_empty_with_size_only_is_accepted() {
        let mut config = base();
        config.rotation.interval_secs = None;
        config.rotation.size_bytes = Some(1024);
        config.create_empty = true;
        config.validate().unwrap();
    }

    #[test]
    fn retain_rejects_circular() {
        let mut config = base();
        config.circular = Some(3);
        config.retain = Some(2);
        assert!(config.validate().is_err());
    }

    #[test]
    fn program_rejects_compression_and_retention() {
        let mut config = base();
        config.program = Some(PathBuf::from("/bin/true"));
        config.compress = true;
        assert!(config.validate().is_err());

        config.compress = false;
        config.retain = Some(3);
        assert!(config.validate().is_err());
    }

    #[test]
    fn compression_level_requires_compress_and_valid_range() {
        let mut config = base();
        config.compress_level = Some(3);
        assert!(config.validate().is_err());

        config.compress = true;
        config.validate().unwrap();

        config.compress_level = Some(0);
        assert!(config.validate().is_err());

        config.compress_level = Some(10);
        assert!(config.validate().is_err());

        config.compress_level = None;
        assert_eq!(config.effective_compress_level(), DEFAULT_COMPRESS_LEVEL);
    }

    #[test]
    fn validates_strftime_pattern() {
        let mut config = base();
        config.logfile = PathBuf::from("access.%Y-%m-%d.log");
        config.validate().unwrap();

        config.logfile = PathBuf::from("access.%Q.log");
        assert!(config.validate().is_err());
    }
}
