use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::config::{
    ArchiveFsync, Config, EchoFlush, MAX_COMPRESS_JOBS, MAX_OFFSET_MINUTES, Rotation,
};
use crate::error::{Error, Result};

pub const HELP: &str = concat!(
    r#"rotalog - rotating log writer for Apache httpd, Tomcat, and other pipes

Usage:
  rotalog [OPTIONS] <logfile> <rotationtime|filesize> [<filesize>] [<offset>]

Read stdin and write log files until EOF or SIGTERM/SIGINT. Rotation can be
based on time, size, or both. Options MUST precede the logfile; arguments after
it are positional values, not options. Use '--' to end option parsing.

Arguments:
  logfile
      Output path or strftime pattern (detected by '%'). Without a pattern,
      append the rotation interval's adjusted Unix timestamp to the path.
      Relative paths use the process working directory. Parent directories
      must exist unless -D is set. Date patterns must be valid UTF-8.

  rotationtime
      Positive integer interval in seconds: 60 = minute, 3600 = hour,
      86400 = day. Intervals align to time boundaries, not startup time.
      By default, time rotation happens on the first write after a boundary;
      use -c to create files even without input.

  filesize
      Positive size with an UPPERCASE B/K/M/G suffix, such as 512K or 100M.
      K/M/G are powers of 1024. Lowercase units are rejected.
      Combine time and size as: logfile 86400 100M. Rotate when either limit
      is reached. Rotation is checked before each write; a single input chunk
      larger than the limit is still written whole.

  offset
      Fixed minutes east of UTC, from -840 to 840; default 0 (UTC).
      Examples: 540 = UTC+09:00, -300 = UTC-05:00. Put this argument last.
      Cannot be combined with -l/--local-time.

Rotation and naming:
  -l, --local-time
      Use the system local time zone for interval boundaries and filenames
      instead of UTC. Daily/hourly boundaries follow local civil time and DST.
      This uses the process/system time zone, not Tomcat's JVM time-zone flag.

  -L, --link <path>
      Atomically update <path> to point at the current log after opening it.
      Prefer a hard link; fall back to a symbolic link if needed. The link path
      must differ from the logfile. Follow it with: tail -F <path>.

  -n, --circular <count>
      Use a positive number of circular slots: base, base.1, ... base.N-1.
      Start by appending to base; truncate slots when reused on later rotations.
      Cannot use a strftime logfile pattern, -z, or --retain.

  -t, --truncate
      Truncate the target at each rotation instead of preserving old contents.
      A literal path is reused; strftime patterns still expand normally.
      Takes precedence over -T and -n. Cannot use -z or --retain.

  -T, --truncate-rotated
      Truncate a target when opened for a later rotation, but append to the
      initial file. Cannot use -z or --retain. Ignored when -t is set.

  -c, --create-empty
      Create files at time boundaries even if no log data arrives.
      Without -f, wait one full interval before creating the first empty file.
      With -f, open the initial file immediately, then follow time boundaries.
      Accepted but has no effect for size-only rotation.

  -f, --force
      Open the initial log immediately at startup instead of waiting for input.
      Useful for checking the path and permissions before the first log write.

Data flow and diagnostics:
  -e, --echo
      Copy input to stdout as well as the logfile. Default: file output only.
      A stdout write failure (including a broken pipe) disables echo only;
      file logging continues. A slow stdout consumer can delay log processing.

      --echo-flush <immediate|buffered>
      Requires -e/--echo. Default: immediate, flushing stdout after every chunk.
      buffered batches up to 64 KiB or flushes on a write after 100 ms elapsed.
      Idle flushes are checked by the 200 ms housekeeping timer. Pending echo
      is also flushed on rotation and shutdown.

  -p, --program <path>
      Start a program after each new file is opened. Arguments are the new
      filename, followed by the previous filename when one exists.
      Executed directly, without a shell; use a wrapper for extra arguments
      or pipelines. Inherits stdin/stdout/stderr; avoid reading its stdin,
      which can consume the logging pipe. Cannot use -z or --retain.

  -D, --create-dirs
      Create missing parent directories for logfile and link paths, including
      directories expanded from date patterns. Default: do not create them.

  -v, --verbose
      Print effective configuration and file open/rotation/link/compression
      operations to stderr. Log contents are not echoed unless -e is also set.

Compression and retention (extensions over Apache rotatelogs):
  -z, --compress
      Gzip retired files asynchronously, adding .gz and removing the original
      after successful compression. The active file stays uncompressed, even
      at shutdown. Default: disabled. Cannot use -t, -T, -n, or -p.
      If maintenance capacity is full, leave that file uncompressed/unretained
      and report the skipped work. Shutdown waits for accepted jobs to finish.

      --compress-level <1-9>
      Requires -z/--compress. Default: 3. Level 1 favors speed; levels 6-9
      favor smaller archives at a higher CPU cost.

      --compress-jobs <N>
      Requires -z/--compress. Range: 1-32; default: 1 worker.
      More workers can reduce compression backlog but use more CPU/memory.
      Retention is applied in rotation order, not worker completion order.

      --fsync-archives <always|data|never>
      Requires -z/--compress. Default: always.
      always: sync_all on the archive before removing the original.
      data:   sync_data before removing the original.
      never:  skip fsync and rely on the OS page cache.
      data/never weaken durability guarantees for crashes or power loss.

      --retain <count>
      Keep only the newest <count> retired files tracked by this process.
      The active file is excluded; 0 removes all retired files.
      Does not scan archives from previous runs. Can be used without -z.
      Default: no retention limit. Cannot use -n, -t, -T, or -p.

Other:
  -h, --help
      Show this detailed help and exit successfully without opening log files.

  -V, --version
      Show the version and exit.

Date patterns:
  %Y = year, %m = month, %d = day; %H = hour, %M = minute, %S = second.
  Daily example: catalina.out.%Y-%m-%d.log -> catalina.out.2026-11-12.log
  Hourly example: access.%Y-%m-%d_%H.log. Use %% for a literal percent sign.
  The pattern does not set the interval; specify 86400/3600/60 separately.
  Coarse patterns use .1, .2 suffixes to preserve files on name collisions.

Exit status:
  0  clean shutdown (stdin EOF or SIGTERM/SIGINT)
  1  invalid command line or configuration
  2  runtime or I/O failure

Examples:
  # Daily rotation, UTC
  rotalog /var/log/access.log 86400
  # Daily Tomcat stdout/stderr pipe: local date, gzip, current-log link
  rotalog -l -f -z -L /var/log/tomcat/catalina.current /var/log/tomcat/catalina.out.%Y-%m-%d.log 86400
  # Time and size limits, with a fixed UTC offset
  rotalog /var/log/access.log 3600 5M -300
  # Seven circular files
  rotalog -n 7 /var/log/access.log 86400
  # Gzip and keep the newest 14 archives created by this process
  rotalog -z --retain 14 /var/log/access.%Y%m%d.log 86400

"#,
    "Author: ",
    env!("CARGO_PKG_AUTHORS"),
    "\nProject: ",
    env!("CARGO_PKG_REPOSITORY"),
    "\n",
);

#[derive(Debug)]
pub enum Invocation {
    Help,
    Version,
    Run(Config),
}

pub fn parse<I>(args: I) -> Result<Invocation>
where
    I: IntoIterator<Item = OsString>,
{
    Parser {
        args: args.into_iter().peekable(),
        cfg: Config::default(),
        positionals: Vec::new(),
        positional_started: false,
        dashdash: false,
    }
    .parse()
}

struct Parser<I>
where
    I: Iterator<Item = OsString>,
{
    args: std::iter::Peekable<I>,
    cfg: Config,
    positionals: Vec<OsString>,
    positional_started: bool,
    dashdash: bool,
}

impl<I> Parser<I>
where
    I: Iterator<Item = OsString>,
{
    fn parse(mut self) -> Result<Invocation> {
        while let Some(arg) = self.args.next() {
            // Apache rotatelogs uses POSIX getopt, which stops interpreting
            // options at the first positional argument. Anything after the
            // logfile (including something that looks like an option) is a
            // rotation limit or UTC offset, not an option.
            if !self.dashdash && !self.positional_started && arg == "--" {
                self.dashdash = true;
                continue;
            }

            if !self.dashdash && !self.positional_started && is_option(&arg) {
                let invocation = if arg.to_string_lossy().starts_with("--") {
                    self.parse_long(&arg)?
                } else {
                    self.parse_short(&arg)?
                };
                if let Some(invocation) = invocation {
                    return Ok(invocation);
                }
            } else {
                self.positionals.push(arg);
                self.positional_started = true;
            }
        }

        self.finish()
    }

    fn parse_long(&mut self, arg: &OsStr) -> Result<Option<Invocation>> {
        let text = arg
            .to_str()
            .ok_or_else(|| Error::usage("long options must be valid UTF-8"))?;
        let body = &text[2..];
        let (name, inline) = match body.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (body, None),
        };

        match name {
            "help" => return Ok(Some(Invocation::Help)),
            "version" => return Ok(Some(Invocation::Version)),
            "local-time" | "local" => self.set_flag(name, inline, |cfg| cfg.local_time = true)?,
            "force" => self.set_flag(name, inline, |cfg| cfg.force_open = true)?,
            "create-dirs" => self.set_flag(name, inline, |cfg| cfg.create_dirs = true)?,
            "truncate" => self.set_flag(name, inline, |cfg| cfg.truncate = true)?,
            "truncate-rotated" => self.set_flag(name, inline, |cfg| cfg.truncate_rotated = true)?,
            "verbose" => self.set_flag(name, inline, |cfg| cfg.verbose = true)?,
            "echo" => self.set_flag(name, inline, |cfg| cfg.echo = true)?,
            "echo-flush" => {
                let value = self.take_value("--echo-flush", inline)?;
                self.cfg.echo_flush = Some(parse_echo_flush(&value)?);
            }
            "create-empty" => self.set_flag(name, inline, |cfg| cfg.create_empty = true)?,
            "compress" => self.set_flag(name, inline, |cfg| cfg.compress = true)?,
            "compress-level" => {
                let value = self.take_value("--compress-level", inline)?;
                self.cfg.compress_level = Some(parse_compress_level(&value)?);
            }
            "compress-jobs" => {
                let value = self.take_value("--compress-jobs", inline)?;
                self.cfg.compress_jobs = Some(parse_compress_jobs(&value)?);
            }
            "fsync-archives" => {
                let value = self.take_value("--fsync-archives", inline)?;
                self.cfg.archive_fsync = Some(parse_archive_fsync(&value)?);
            }
            "link" => {
                let value = self.take_value("--link", inline)?;
                self.cfg.link = Some(PathBuf::from(value));
            }
            "program" => {
                let value = self.take_value("--program", inline)?;
                self.cfg.program = Some(PathBuf::from(value));
            }
            "circular" => {
                let value = self.take_value("--circular", inline)?;
                self.cfg.circular = Some(parse_count(&value, "--circular")?);
            }
            "retain" => {
                let value = self.take_value("--retain", inline)?;
                self.cfg.retain = Some(parse_count(&value, "--retain")?);
            }
            _ => {
                return Err(Error::usage(format!(
                    "unknown option '--{name}' (try --help)"
                )));
            }
        }

        Ok(None)
    }

    fn parse_short(&mut self, arg: &OsStr) -> Result<Option<Invocation>> {
        let text = arg
            .to_str()
            .ok_or_else(|| Error::usage("short options must be valid UTF-8"))?;
        let bytes = text.as_bytes();
        let mut index = 1;

        while index < bytes.len() {
            let byte = bytes[index];
            if !byte.is_ascii() {
                return Err(Error::usage(format!(
                    "unknown option '-{}' (try --help)",
                    text[index..].chars().next().unwrap_or('?')
                )));
            }
            let option = byte as char;
            index += 1;

            match option {
                'h' => return Ok(Some(Invocation::Help)),
                'V' => return Ok(Some(Invocation::Version)),
                'l' => self.cfg.local_time = true,
                'f' => self.cfg.force_open = true,
                'D' => self.cfg.create_dirs = true,
                't' => self.cfg.truncate = true,
                'T' => self.cfg.truncate_rotated = true,
                'v' => self.cfg.verbose = true,
                'e' => self.cfg.echo = true,
                'c' => self.cfg.create_empty = true,
                'z' => self.cfg.compress = true,
                'L' | 'p' | 'n' => {
                    let value = if index < bytes.len() {
                        let rest = OsString::from(&text[index..]);
                        index = bytes.len();
                        rest
                    } else {
                        self.args.next().ok_or_else(|| {
                            Error::usage(format!("option '-{option}' requires a value"))
                        })?
                    };

                    match option {
                        'L' => self.cfg.link = Some(PathBuf::from(value)),
                        'p' => self.cfg.program = Some(PathBuf::from(value)),
                        'n' => {
                            self.cfg.circular = Some(parse_count(&value, "-n")?);
                        }
                        _ => unreachable!(),
                    }
                }
                _ => {
                    return Err(Error::usage(format!(
                        "unknown option '-{option}' (try --help)"
                    )));
                }
            }
        }

        Ok(None)
    }

    fn set_flag<F>(&mut self, name: &str, inline: Option<&str>, apply: F) -> Result<()>
    where
        F: FnOnce(&mut Config),
    {
        if inline.is_some() {
            return Err(Error::usage(format!(
                "option '--{name}' does not take a value"
            )));
        }
        apply(&mut self.cfg);
        Ok(())
    }

    fn take_value(&mut self, name: &str, inline: Option<&str>) -> Result<OsString> {
        match inline {
            Some(value) => Ok(OsString::from(value)),
            None => self
                .args
                .next()
                .ok_or_else(|| Error::usage(format!("option '{name}' requires a value"))),
        }
    }

    fn finish(mut self) -> Result<Invocation> {
        if self.positionals.is_empty() {
            return Err(Error::usage("missing logfile path (try 'rotalog --help')"));
        }

        let logfile = PathBuf::from(self.positionals.remove(0));
        if self.positionals.is_empty() {
            return Err(Error::usage("missing rotation time or maximum file size"));
        }

        // The final positional may be a UTC offset. A plain integer can only be
        // an offset because sizes require a unit suffix.
        let mut offset_set = false;
        if self.positionals.len() >= 2 {
            if let Some(candidate) = self.positionals.last().and_then(|v| v.to_str()) {
                if !has_size_suffix(candidate) {
                    if let Ok(offset) = candidate.parse::<i32>() {
                        if !(-MAX_OFFSET_MINUTES..=MAX_OFFSET_MINUTES).contains(&offset) {
                            return Err(Error::usage(format!(
                                "UTC offset {offset} is out of range \
                                 (-{MAX_OFFSET_MINUTES}..={MAX_OFFSET_MINUTES} minutes); \
                                 file sizes require a B, K, M or G suffix"
                            )));
                        }
                        self.positionals.pop();
                        self.cfg.offset_minutes = offset;
                        offset_set = true;
                    }
                }
            }
        }

        if self.cfg.local_time && offset_set {
            return Err(Error::usage(
                "the UTC offset cannot be combined with -l/--local-time",
            ));
        }

        self.cfg.logfile = logfile;
        self.cfg.rotation = parse_rotation(&self.positionals)?;
        self.cfg.validate()?;
        Ok(Invocation::Run(self.cfg))
    }
}

fn parse_rotation(values: &[OsString]) -> Result<Rotation> {
    if values.is_empty() {
        return Err(Error::usage("missing rotation time or maximum file size"));
    }
    if values.len() > 2 {
        return Err(Error::usage(format!(
            "unexpected extra argument '{}'",
            values[2].to_string_lossy()
        )));
    }

    let mut rotation = Rotation::default();
    for value in values {
        let value = value
            .to_str()
            .ok_or_else(|| Error::usage("rotation time and file size must be valid UTF-8"))?;

        if let Some(size) = parse_size(value)? {
            if rotation.size_bytes.is_some() {
                return Err(Error::usage("rotation size parameter allowed only once"));
            }
            rotation.size_bytes = Some(size);
        } else {
            if rotation.interval_secs.is_some() {
                return Err(Error::usage("rotation time parameter allowed only once"));
            }
            let seconds = value.parse::<u64>().map_err(|_| {
                Error::usage(format!(
                    "invalid rotation time '{value}' \
                     (expected seconds or an uppercase size such as 5M)"
                ))
            })?;
            rotation.interval_secs = Some(seconds);
        }
    }

    Ok(rotation)
}

/// Parse `123`, `123B`, `5K`, `10M`, `1G`. Returns None when no unit suffix is
/// present, because a bare integer is a rotation time.
///
/// Only uppercase units are accepted, matching Apache's documented syntax.
/// Lowercase units are rejected instead of silently becoming a rotation time.
fn parse_size(value: &str) -> Result<Option<u64>> {
    let Some(last) = value.as_bytes().last().copied() else {
        return Ok(None);
    };
    let multiplier = match last {
        b'B' => 1_u64,
        b'K' => 1024,
        b'M' => 1024 * 1024,
        b'G' => 1024 * 1024 * 1024,
        _ => return Ok(None),
    };

    let digits = &value[..value.len() - 1];
    if digits.is_empty() {
        return Err(Error::usage(format!(
            "missing number in file size '{value}'"
        )));
    }
    let number = digits
        .parse::<u64>()
        .map_err(|_| Error::usage(format!("invalid file size '{value}'")))?;
    let bytes = number
        .checked_mul(multiplier)
        .ok_or_else(|| Error::usage(format!("file size '{value}' is too large")))?;
    Ok(Some(bytes))
}

fn has_size_suffix(value: &str) -> bool {
    matches!(
        value.as_bytes().last().copied(),
        Some(b'B' | b'K' | b'M' | b'G')
    )
}

fn parse_count(value: &OsStr, option: &str) -> Result<usize> {
    let text = value
        .to_str()
        .ok_or_else(|| Error::usage(format!("{option} requires a number")))?;
    text.parse::<usize>()
        .map_err(|_| Error::usage(format!("{option} requires a positive number")))
}

fn parse_compress_level(value: &OsStr) -> Result<u8> {
    let text = value
        .to_str()
        .ok_or_else(|| Error::usage("--compress-level requires a number from 1 to 9"))?;
    let level = text
        .parse::<u8>()
        .map_err(|_| Error::usage("--compress-level requires a number from 1 to 9"))?;
    if !(1..=9).contains(&level) {
        return Err(Error::usage("--compress-level must be between 1 and 9"));
    }
    Ok(level)
}

fn parse_archive_fsync(value: &OsStr) -> Result<ArchiveFsync> {
    match value.to_str() {
        Some("always") => Ok(ArchiveFsync::Always),
        Some("data") => Ok(ArchiveFsync::Data),
        Some("never") => Ok(ArchiveFsync::Never),
        _ => Err(Error::usage(
            "--fsync-archives must be one of: always, data, never",
        )),
    }
}

fn parse_compress_jobs(value: &OsStr) -> Result<usize> {
    let text = value
        .to_str()
        .ok_or_else(|| Error::usage("--compress-jobs requires a number"))?;
    let jobs = text
        .parse::<usize>()
        .map_err(|_| Error::usage("--compress-jobs requires a number"))?;
    if !(1..=MAX_COMPRESS_JOBS).contains(&jobs) {
        return Err(Error::usage(format!(
            "--compress-jobs must be between 1 and {MAX_COMPRESS_JOBS}"
        )));
    }
    Ok(jobs)
}

fn parse_echo_flush(value: &OsStr) -> Result<EchoFlush> {
    match value.to_str() {
        Some("immediate") => Ok(EchoFlush::Immediate),
        Some("buffered") => Ok(EchoFlush::Buffered),
        _ => Err(Error::usage(
            "--echo-flush must be one of: immediate, buffered",
        )),
    }
}

fn is_option(arg: &OsStr) -> bool {
    let bytes = arg.as_encoded_bytes();
    bytes.len() > 1 && bytes[0] == b'-'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_args(args: &[&str]) -> Result<Config> {
        match parse(args.iter().map(OsString::from))? {
            Invocation::Run(config) => Ok(config),
            other => panic!("expected run invocation, got {other:?}"),
        }
    }

    #[test]
    fn parses_time_rotation() {
        let config = parse_args(&["/var/log/access.log", "86400"]).unwrap();
        assert_eq!(config.logfile, PathBuf::from("/var/log/access.log"));
        assert_eq!(config.rotation.interval_secs, Some(86400));
        assert_eq!(config.rotation.size_bytes, None);
        assert_eq!(config.offset_minutes, 0);
    }

    #[test]
    fn parses_size_rotation_with_suffix() {
        let config = parse_args(&["access.log", "5M"]).unwrap();
        assert_eq!(config.rotation.interval_secs, None);
        assert_eq!(config.rotation.size_bytes, Some(5 * 1024 * 1024));
    }

    #[test]
    fn parses_time_size_and_offset() {
        let config = parse_args(&["access.log", "3600", "5M", "-300"]).unwrap();
        assert_eq!(config.rotation.interval_secs, Some(3600));
        assert_eq!(config.rotation.size_bytes, Some(5 * 1024 * 1024));
        assert_eq!(config.offset_minutes, -300);
    }

    #[test]
    fn parses_time_and_size_without_offset() {
        let config = parse_args(&["access.log", "3600", "5M"]).unwrap();
        assert_eq!(config.rotation.interval_secs, Some(3600));
        assert_eq!(config.rotation.size_bytes, Some(5 * 1024 * 1024));
        assert_eq!(config.offset_minutes, 0);
    }

    #[test]
    fn parses_size_before_time_with_offset() {
        // Apache's parser accepts the two limits in either order as long as a
        // final UTC offset makes the second bare integer unambiguous.
        let config = parse_args(&["access.log", "5M", "60", "60"]).unwrap();
        assert_eq!(config.rotation.interval_secs, Some(60));
        assert_eq!(config.rotation.size_bytes, Some(5 * 1024 * 1024));
        assert_eq!(config.offset_minutes, 60);
    }

    #[test]
    fn a_trailing_bare_integer_after_a_size_is_an_offset() {
        let config = parse_args(&["access.log", "5M", "60"]).unwrap();
        assert_eq!(config.rotation.interval_secs, None);
        assert_eq!(config.rotation.size_bytes, Some(5 * 1024 * 1024));
        assert_eq!(config.offset_minutes, 60);
    }

    #[test]
    fn parses_combined_short_flags_and_attached_values() {
        let config = parse_args(&["-lveD", "-n7", "access.log", "1G"]).unwrap();
        assert!(config.local_time);
        assert!(config.verbose);
        assert!(config.echo);
        assert!(config.create_dirs);
        assert_eq!(config.circular, Some(7));
        assert_eq!(config.rotation.size_bytes, Some(1024 * 1024 * 1024));
    }

    #[test]
    fn parses_attached_link_and_program_values() {
        let config =
            parse_args(&["-L/tmp/current.log", "-p/bin/true", "access.log", "60"]).unwrap();
        assert_eq!(config.link, Some(PathBuf::from("/tmp/current.log")));
        assert_eq!(config.program, Some(PathBuf::from("/bin/true")));
    }

    #[test]
    fn parses_combined_flags_with_attached_value() {
        let config = parse_args(&["-lvn3", "access.log", "60"]).unwrap();
        assert!(config.local_time);
        assert!(config.verbose);
        assert_eq!(config.circular, Some(3));
    }

    #[test]
    fn parses_long_options() {
        let config = parse_args(&[
            "--local-time",
            "--link=/tmp/current.log",
            "--retain",
            "14",
            "--compress",
            "--compress-level",
            "1",
            "--compress-jobs",
            "2",
            "--fsync-archives",
            "data",
            "access.%Y%m%d.log",
            "5M",
        ])
        .unwrap();
        assert!(config.local_time);
        assert_eq!(config.link, Some(PathBuf::from("/tmp/current.log")));
        assert_eq!(config.retain, Some(14));
        assert!(config.compress);
        assert_eq!(config.compress_level, Some(1));
        assert_eq!(config.compress_jobs, Some(2));
        assert_eq!(config.archive_fsync, Some(ArchiveFsync::Data));
    }

    #[test]
    fn compression_level_requires_compress() {
        assert!(parse_args(&["--compress-level", "3", "access.log", "60"]).is_err());
        assert!(parse_args(&["-z", "--compress-level=3", "access.log", "60"]).is_ok());
        assert!(parse_args(&["-z", "--compress-level=0", "access.log", "60"]).is_err());
        assert!(parse_args(&["-z", "--compress-level=10", "access.log", "60"]).is_err());
    }

    #[test]
    fn compress_jobs_requires_compress_and_valid_range() {
        assert!(parse_args(&["--compress-jobs", "2", "access.log", "60"]).is_err());
        assert!(parse_args(&["-z", "--compress-jobs=2", "access.log", "60"]).is_ok());
        assert!(parse_args(&["-z", "--compress-jobs=0", "access.log", "60"]).is_err());
        assert!(parse_args(&["-z", "--compress-jobs=33", "access.log", "60"]).is_err());
    }

    #[test]
    fn archive_fsync_requires_compress_and_valid_value() {
        assert!(parse_args(&["--fsync-archives", "never", "access.log", "60"]).is_err());
        assert!(parse_args(&["-z", "--fsync-archives=never", "access.log", "60"]).is_ok());
        assert!(parse_args(&["-z", "--fsync-archives=sometimes", "access.log", "60"]).is_err());
    }

    #[test]
    fn echo_flush_requires_echo_and_valid_value() {
        assert!(parse_args(&["--echo-flush", "buffered", "access.log", "60"]).is_err());
        assert!(parse_args(&["-e", "--echo-flush=buffered", "access.log", "60"]).is_ok());
        assert!(parse_args(&["-e", "--echo-flush=invalid", "access.log", "60"]).is_err());
    }

    #[test]
    fn help_and_version_win() {
        assert!(matches!(
            parse([OsString::from("--help")]).unwrap(),
            Invocation::Help
        ));
        assert!(matches!(
            parse([OsString::from("-V")]).unwrap(),
            Invocation::Version
        ));
    }

    #[test]
    fn rejects_missing_rotation() {
        assert!(parse_args(&["access.log"]).is_err());
    }

    #[test]
    fn rejects_unknown_option() {
        assert!(parse_args(&["-Q", "access.log", "60"]).is_err());
    }

    #[test]
    fn rejects_local_time_with_offset() {
        assert!(parse_args(&["-l", "access.log", "60", "60"]).is_err());
    }

    #[test]
    fn second_bare_integer_is_an_offset() {
        let config = parse_args(&["access.log", "60", "5"]).unwrap();
        assert_eq!(config.rotation.interval_secs, Some(60));
        assert_eq!(config.offset_minutes, 5);
    }

    #[test]
    fn negative_offset_is_positional() {
        let config = parse_args(&["access.log", "3600", "-300"]).unwrap();
        assert_eq!(config.offset_minutes, -300);
    }

    #[test]
    fn options_after_the_logfile_are_not_options() {
        // POSIX getopt (used by Apache rotatelogs) stops at the first
        // positional. Anything after it is interpreted as a rotation/offset.
        assert!(parse_args(&["access.log", "60", "-l"]).is_err());
        assert!(parse_args(&["access.log", "-l", "60"]).is_err());
    }

    #[test]
    fn double_dash_ends_option_parsing() {
        let config = parse_args(&["--", "access.log", "60"]).unwrap();
        assert_eq!(config.logfile, PathBuf::from("access.log"));
        assert_eq!(config.rotation.interval_secs, Some(60));
    }

    #[test]
    fn rejects_duplicate_time_limits() {
        assert!(parse_args(&["access.log", "60", "60", "60"]).is_err());
    }

    #[test]
    fn rejects_two_size_limits() {
        assert!(parse_args(&["access.log", "5M", "10M"]).is_err());
        assert!(parse_args(&["access.log", "5M", "10M", "60"]).is_err());
    }

    #[test]
    fn rejects_invalid_size() {
        assert!(parse_args(&["access.log", "5X"]).is_err());
        assert!(parse_args(&["access.log", "0M"]).is_err());
    }

    #[test]
    fn only_uppercase_units_are_sizes() {
        assert_eq!(parse_size("1B").unwrap(), Some(1));
        assert_eq!(parse_size("1K").unwrap(), Some(1024));
        assert_eq!(parse_size("2M").unwrap(), Some(2 * 1024 * 1024));
        assert_eq!(parse_size("1G").unwrap(), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("5G").unwrap(), Some(5 * 1024 * 1024 * 1024));

        // Apache treats lowercase suffixes as part of a rotation time via
        // atoi(). We reject them explicitly instead of silently changing the
        // meaning of the argument.
        assert_eq!(parse_size("1b").unwrap(), None);
        assert_eq!(parse_size("1k").unwrap(), None);
        assert_eq!(parse_size("1m").unwrap(), None);
        assert_eq!(parse_size("1g").unwrap(), None);
        assert!(parse_args(&["access.log", "5m"]).is_err());
    }

    #[test]
    fn rejects_non_numeric_and_out_of_range_arguments() {
        assert!(parse_args(&["access.log", "60abc"]).is_err());
        assert!(parse_args(&["access.log", "60", "99999"]).is_err());
        assert!(parse_args(&["-n", "3x", "access.log", "60"]).is_err());
    }

    #[test]
    fn truncate_can_be_combined_like_the_original() {
        let config = parse_args(&["-t", "-T", "-n", "3", "access.log", "60"]).unwrap();
        assert!(config.truncate);
        assert!(config.truncate_rotated);
        assert_eq!(config.circular, Some(3));
    }
}
