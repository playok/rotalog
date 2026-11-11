use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use crate::config::{
    ArchiveFsync, Config, EchoFlush, MAX_COMPRESS_JOBS, MAX_OFFSET_MINUTES, Rotation,
};
use crate::error::{Error, Result};

pub const HELP: &str = r#"rotalog - rotating log writer for Apache httpd and other pipes

Usage:
  rotalog [OPTIONS] <logfile> <rotationtime|filesize> [<filesize>] [<offset>]

The logfile is a path or an strftime(3) pattern. A pattern is detected when the
path contains '%'. Rotation can be time-based, size-based, or both. File sizes
must use an uppercase B, K, M or G suffix (for example 5M).

Options must appear before the logfile, matching Apache's POSIX getopt parsing.
Use '--' to end option parsing.

Rotation and naming:
  -l, --local-time          Use local time instead of UTC for alignment/names
  -L, --link <path>         Point <path> at the current logfile (atomic update)
  -n, --circular <count>    Use circular names: base, base.1, ... base.N-1
  -t, --truncate            Truncate the logfile instead of rotating it
  -T, --truncate-rotated    Truncate rotated targets when they are re-opened
  -c, --create-empty        Create a file for each interval, even if empty
  -f, --force               Open the first logfile immediately

Data flow and maintenance:
  -e, --echo                Echo log data to stdout as well
      --echo-flush <immediate|buffered>
                            Echo flush policy (default immediate)
  -p, --program <path>      Run <path> with the new (and previous) filename
  -D, --create-dirs         Create missing parent directories
  -v, --verbose             Print configuration and file operations on stderr

Extensions over Apache rotatelogs:
  -z, --compress            Gzip rotated files asynchronously
      --compress-level <1-9> Gzip level for -z (default 3)
      --compress-jobs <N>   Parallel compression workers (default 1)
      --fsync-archives <always|data|never>
                            Archive fsync policy for -z (default always)
      --retain <count>      Keep only the newest <count> rotated files

Other:
  -h, --help                Show this help
  -V, --version             Show the version

Arguments:
  logfile                   Output path or strftime(3) pattern
  rotationtime              Seconds between rotations (integer)
  filesize                  Maximum size, with B/K/M/G suffix (for example 5M)
  offset                    Minutes east of UTC (-840..=840)

Exit status:
  0  success
  1  invalid usage
  2  runtime or I/O failure

Examples:
  rotalog /var/log/access.log 86400
  rotalog -l /var/log/access.%Y-%m-%d.log 86400
  rotalog /var/log/access.log 3600 5M -300
  rotalog -n 7 /var/log/access.log 86400
  rotalog -z --retain 14 /var/log/access.%Y%m%d.log 5M
"#;

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
