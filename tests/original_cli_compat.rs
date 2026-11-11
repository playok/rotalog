//! Differential CLI compatibility tests against the original Apache
//! `rotatelogs` binary.
//!
//! The tests locate an original binary via `ROTATELOGS_ORACLE` or a few common
//! install paths. When none is available the tests print a skip message and
//! succeed, so `cargo test` works on machines without Apache.
//!
//! For every command line in the matrix we compare:
//!   * whether both binaries accept or reject it,
//!   * the effective rotation configuration printed by `-v`,
//!   * for a few representative cases, the files and stdout produced.
//!
//! Deliberately stricter or extended behavior (malformed `atoi()` input,
//! lowercase units, `-h`/`-V`, long options, `-z`, `--retain`) lives in
//! separate tests at the bottom and is documented in README.md.

use std::env;
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const OURS: &str = env!("CARGO_BIN_EXE_rotalog");

#[derive(Debug, PartialEq, Eq)]
struct CommonConfig {
    interval: u64,
    size: u64,
    offset_secs: i64,
    localtime: bool,
    strftime: bool,
    force: bool,
    create_dirs: bool,
    verbose: bool,
    create_empty: bool,
    logfile: String,
    program: Option<String>,
}

struct Case {
    name: &'static str,
    args: &'static [&'static str],
    requires: &'static [&'static str],
}

const CASES: &[Case] = &[
    Case {
        name: "time only",
        args: &["log", "60"],
        requires: &[],
    },
    Case {
        name: "one second",
        args: &["log", "1"],
        requires: &[],
    },
    Case {
        name: "one day",
        args: &["log", "86400"],
        requires: &[],
    },
    Case {
        name: "size bytes",
        args: &["log", "5B"],
        requires: &[],
    },
    Case {
        name: "size kibibytes",
        args: &["log", "5K"],
        requires: &[],
    },
    Case {
        name: "size mebibytes",
        args: &["log", "5M"],
        requires: &[],
    },
    Case {
        name: "size gibibytes",
        args: &["log", "1G"],
        requires: &[],
    },
    Case {
        name: "time and size",
        args: &["log", "3600", "100M"],
        requires: &[],
    },
    Case {
        name: "time and size without offset",
        args: &["log", "3600", "5M"],
        requires: &[],
    },
    Case {
        name: "time and negative offset",
        args: &["log", "60", "-300"],
        requires: &[],
    },
    Case {
        name: "time and positive offset",
        args: &["log", "60", "300"],
        requires: &[],
    },
    Case {
        name: "time and zero offset",
        args: &["log", "60", "0"],
        requires: &[],
    },
    Case {
        name: "size and offset",
        args: &["log", "5M", "60"],
        requires: &[],
    },
    Case {
        name: "size then time then offset",
        args: &["log", "5M", "60", "60"],
        requires: &[],
    },
    Case {
        name: "time then size then offset",
        args: &["log", "60", "5M", "60"],
        requires: &[],
    },
    Case {
        name: "local time",
        args: &["-l", "log", "60"],
        requires: &[],
    },
    Case {
        name: "local time with size",
        args: &["-l", "log", "5M"],
        requires: &[],
    },
    Case {
        name: "force open",
        args: &["-f", "log", "60"],
        requires: &[],
    },
    Case {
        name: "create directories",
        args: &["-D", "log", "60"],
        requires: &[],
    },
    Case {
        name: "echo",
        args: &["-e", "log", "60"],
        requires: &[],
    },
    Case {
        name: "truncate",
        args: &["-t", "log", "60"],
        requires: &[],
    },
    Case {
        name: "truncate rotated",
        args: &["-T", "log.%d", "3600"],
        requires: &["-T"],
    },
    Case {
        name: "circular",
        args: &["-n", "3", "log", "60"],
        requires: &[],
    },
    Case {
        name: "circular attached value",
        args: &["-n3", "log", "60"],
        requires: &[],
    },
    Case {
        name: "link and program",
        args: &["-L", "link", "-p", "/bin/true", "log", "60"],
        requires: &[],
    },
    Case {
        name: "attached link value",
        args: &["-Llink", "log", "60"],
        requires: &[],
    },
    Case {
        name: "combined flags",
        args: &["-lveD", "log", "60"],
        requires: &[],
    },
    Case {
        name: "combined flags with attached value",
        args: &["-lvn3", "log", "60"],
        requires: &[],
    },
    Case {
        name: "force, link, program",
        args: &["-f", "-L", "link", "-p", "/bin/true", "log", "60"],
        requires: &[],
    },
    Case {
        name: "truncate wins over T and n",
        args: &["-t", "-T", "-n", "3", "log", "60"],
        requires: &["-T"],
    },
    Case {
        name: "double dash",
        args: &["--", "log", "60"],
        requires: &[],
    },
    Case {
        name: "strftime pattern",
        args: &["log.%Y%m%d", "86400"],
        requires: &[],
    },
    Case {
        name: "create empty",
        args: &["-c", "log", "60"],
        requires: &["-c"],
    },
    Case {
        name: "create empty with size only",
        args: &["-c", "log", "5M"],
        requires: &["-c"],
    },
    // Rejected by both implementations.
    Case {
        name: "no arguments",
        args: &[],
        requires: &[],
    },
    Case {
        name: "missing rotation",
        args: &["log"],
        requires: &[],
    },
    Case {
        name: "too many arguments",
        args: &["log", "60", "5M", "60", "60"],
        requires: &[],
    },
    Case {
        name: "duplicate time",
        args: &["log", "60", "60", "60"],
        requires: &[],
    },
    Case {
        name: "duplicate size",
        args: &["log", "5M", "5M"],
        requires: &[],
    },
    Case {
        name: "duplicate size with offset",
        args: &["log", "5M", "5M", "60"],
        requires: &[],
    },
    Case {
        name: "duplicate size after time",
        args: &["log", "60", "5M", "5M"],
        requires: &[],
    },
    Case {
        name: "zero time",
        args: &["log", "0"],
        requires: &[],
    },
    Case {
        name: "zero size",
        args: &["log", "0B"],
        requires: &[],
    },
    Case {
        name: "local time with offset",
        args: &["-l", "log", "60", "60"],
        requires: &[],
    },
    Case {
        name: "circular zero",
        args: &["-n", "0", "log", "60"],
        requires: &[],
    },
    Case {
        name: "circular negative",
        args: &["-n", "-1", "log", "60"],
        requires: &[],
    },
    Case {
        name: "circular non-numeric",
        args: &["-n", "abc", "log", "60"],
        requires: &[],
    },
    Case {
        name: "circular with pattern",
        args: &["-n", "3", "log.%Y", "60"],
        requires: &[],
    },
    Case {
        name: "unknown short option",
        args: &["-q", "log", "60"],
        requires: &[],
    },
    Case {
        name: "missing link value",
        args: &["-L"],
        requires: &[],
    },
    Case {
        name: "missing program value",
        args: &["-p"],
        requires: &[],
    },
];

fn find_oracle() -> Option<PathBuf> {
    if let Some(path) = env::var_os("ROTATELOGS_ORACLE") {
        let path = PathBuf::from(path);
        return (path.is_file() && is_apache_rotatelogs(&path)).then_some(path);
    }

    for candidate in [
        "/usr/sbin/rotatelogs",
        "/usr/local/sbin/rotatelogs",
        "/opt/homebrew/sbin/rotatelogs",
        "/opt/homebrew/opt/httpd/bin/rotatelogs",
        "/usr/local/bin/rotatelogs",
    ] {
        let path = PathBuf::from(candidate);
        if path.is_file() && is_apache_rotatelogs(&path) {
            return Some(path);
        }
    }

    if let Some(path) = env::var_os("PATH") {
        for directory in env::split_paths(&path) {
            let candidate = directory.join("rotatelogs");
            if candidate.is_file() && is_apache_rotatelogs(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Avoid the failure mode where a `rotatelogs -> rotalog` compatibility
/// symlink is found on PATH and the differential test compares rotalog to
/// itself. Apache's `-v` output has a distinctive header.
fn is_apache_rotatelogs(path: &Path) -> bool {
    if let (Ok(candidate), Ok(ours)) = (fs::canonicalize(path), fs::canonicalize(OURS)) {
        if candidate == ours {
            return false;
        }
    }

    let directory = match tempfile::tempdir() {
        Ok(directory) => directory,
        Err(_) => return false,
    };
    let output = Command::new(path)
        .args(["-v", "log", "60"])
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stderr).contains("Rotation time interval:"),
        Err(_) => false,
    }
}

fn oracle_supports(oracle: &Path, flag: &str) -> bool {
    let directory = tempfile::tempdir().expect("temp dir");
    Command::new(oracle)
        .args([flag, "-v", "log", "60"])
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn run(binary: &Path, args: &[&str], cwd: &Path) -> Output {
    let mut full_args = vec!["-v"];
    full_args.extend_from_slice(args);
    Command::new(binary)
        .args(&full_args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("run rotatelogs")
}

fn parse_oracle(stderr: &str) -> Option<CommonConfig> {
    let mut interval = None;
    let mut size = None;
    let mut offset_secs = None;
    let mut localtime = None;
    let mut strftime = None;
    let mut force = None;
    let mut create_dirs = None;
    let mut verbose = None;
    let mut create_empty = None;
    let mut logfile = None;
    let mut program = None;

    for line in stderr.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "Rotation time interval" => interval = value.parse::<u64>().ok(),
            "Rotation size interval" => size = value.parse::<u64>().ok(),
            "Rotation time UTC offset" => offset_secs = value.parse::<i64>().ok(),
            "Rotation based on localtime" => localtime = parse_yes_no(value),
            "Rotation file date pattern" => strftime = parse_yes_no(value),
            "Rotation file forced open" => force = parse_yes_no(value),
            "Create parent directories" => create_dirs = parse_yes_no(value),
            "Rotation verbose" => verbose = parse_yes_no(value),
            "Rotation create empty logs" => create_empty = parse_yes_no(value),
            "Rotation file name" => logfile = Some(file_name(value)),
            "Post-rotation prog" => {
                program = if value == "not used" {
                    None
                } else {
                    Some(value.to_owned())
                }
            }
            _ => {}
        }
    }

    Some(CommonConfig {
        interval: interval?,
        size: size?,
        offset_secs: offset_secs?,
        localtime: localtime?,
        strftime: strftime?,
        force: force?,
        create_dirs: create_dirs?,
        // Older builds omit the -c line when APR_FILES_AS_SOCKETS is absent.
        verbose: verbose?,
        create_empty: create_empty.unwrap_or(false),
        logfile: logfile?,
        program,
    })
}

fn parse_ours(stderr: &str) -> Option<CommonConfig> {
    let mut interval = None;
    let mut size = None;
    let mut offset_secs = None;
    let mut localtime = None;
    let mut strftime = None;
    let mut force = None;
    let mut create_dirs = None;
    let mut create_empty = None;
    let mut logfile = None;
    let mut program = None;

    for line in stderr.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "rotation interval" => {
                interval = Some(strip_number(value, "none", " seconds")?);
            }
            "rotation size" => {
                size = Some(strip_number(value, "none", " bytes")?);
            }
            "time base" => localtime = Some(value == "local"),
            "strftime pattern" => strftime = parse_true_false(value),
            "open immediately" => force = parse_true_false(value),
            "create directories" => create_dirs = parse_true_false(value),
            "create empty" => create_empty = parse_true_false(value),
            "logfile" => logfile = Some(file_name(value)),
            "post-rotate program" => {
                program = if value == "none" {
                    None
                } else {
                    Some(value.to_owned())
                }
            }
            "UTC offset" => {
                offset_secs = Some(value.strip_suffix(" minutes")?.parse::<i64>().ok()? * 60);
            }
            _ => {}
        }
    }

    Some(CommonConfig {
        interval: interval?,
        size: size?,
        offset_secs: offset_secs?,
        localtime: localtime?,
        strftime: strftime?,
        force: force?,
        create_dirs: create_dirs?,
        // -v is always prepended by the test harness.
        verbose: true,
        create_empty: create_empty?,
        logfile: logfile?,
        program,
    })
}

fn strip_number(value: &str, none: &str, suffix: &str) -> Option<u64> {
    if value == format!("{none}{suffix}") {
        return Some(0);
    }
    value.strip_suffix(suffix)?.parse::<u64>().ok()
}

/// Apache may normalize a relative logfile to an absolute path via
/// `apr_filepath_merge`. The compatibility matrix uses simple relative names,
/// so comparing the final path component is sufficient and portable.
fn file_name(value: &str) -> String {
    Path::new(value)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| value.to_owned())
}

fn parse_yes_no(value: &str) -> Option<bool> {
    match value {
        "yes" => Some(true),
        "no" => Some(false),
        _ => None,
    }
}

fn parse_true_false(value: &str) -> Option<bool> {
    match value {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[test]
fn our_binary_is_not_an_apache_oracle() {
    assert!(!is_apache_rotatelogs(Path::new(OURS)));
}

#[test]
fn cli_matches_the_original_rotatelogs() {
    let Some(oracle) = find_oracle() else {
        eprintln!(
            "skipping original CLI compatibility tests: set ROTATELOGS_ORACLE \
             or install Apache rotatelogs"
        );
        return;
    };

    for case in CASES {
        if !case
            .requires
            .iter()
            .all(|flag| oracle_supports(&oracle, flag))
        {
            eprintln!(
                "skipping case '{}': oracle lacks {:?}",
                case.name, case.requires
            );
            continue;
        }

        let oracle_dir = tempfile::tempdir().expect("temp dir");
        let ours_dir = tempfile::tempdir().expect("temp dir");

        let oracle_output = run(&oracle, case.args, oracle_dir.path());
        let ours_output = run(Path::new(OURS), case.args, ours_dir.path());

        assert_eq!(
            ours_output.status.success(),
            oracle_output.status.success(),
            "case '{}' acceptance differs\nargs: {:?}\nours stderr:\n{}\noracle stderr:\n{}",
            case.name,
            case.args,
            String::from_utf8_lossy(&ours_output.stderr),
            String::from_utf8_lossy(&oracle_output.stderr),
        );
        assert_eq!(
            ours_output.status.code(),
            oracle_output.status.code(),
            "case '{}' exit code differs for args {:?}",
            case.name,
            case.args,
        );

        if !oracle_output.status.success() {
            assert!(
                !ours_output.status.success(),
                "case '{}' should have been rejected",
                case.name
            );
            continue;
        }

        let oracle_config = parse_oracle(&String::from_utf8_lossy(&oracle_output.stderr))
            .unwrap_or_else(|| {
                panic!(
                    "could not parse oracle config for '{}':\n{}",
                    case.name,
                    String::from_utf8_lossy(&oracle_output.stderr)
                )
            });
        let ours_config =
            parse_ours(&String::from_utf8_lossy(&ours_output.stderr)).unwrap_or_else(|| {
                panic!(
                    "could not parse our config for '{}':\n{}",
                    case.name,
                    String::from_utf8_lossy(&ours_output.stderr)
                )
            });

        assert_eq!(
            ours_config, oracle_config,
            "case '{}' configuration differs\nargs: {:?}",
            case.name, case.args
        );
    }
}

/// The original parser uses `atoi()` and a byte scanner, so it silently
/// accepts malformed values. We deliberately reject these with a clear error.
#[test]
fn malformed_inputs_are_rejected_instead_of_silently_coerced() {
    let Some(oracle) = find_oracle() else {
        eprintln!("skipping malformed-input comparison: no original rotatelogs");
        return;
    };

    let cases: &[&[&str]] = &[
        &["log", "5m"],
        &["log", "5MB"],
        &["log", "60abc"],
        &["log", "60", "abc"],
        &["log", "60", "99999"],
        &["log", "60", "-l"],
        &["-n", "3x", "log", "60"],
        &["--unknown", "log", "60"],
    ];

    for args in cases {
        let oracle_dir = tempfile::tempdir().expect("temp dir");
        let ours_dir = tempfile::tempdir().expect("temp dir");
        let oracle_output = run(&oracle, args, oracle_dir.path());
        let ours_output = run(Path::new(OURS), args, ours_dir.path());

        assert!(
            oracle_output.status.success(),
            "expected original to accept {:?}; stderr:\n{}",
            args,
            String::from_utf8_lossy(&oracle_output.stderr)
        );
        assert!(
            !ours_output.status.success(),
            "expected rotalog to reject {:?}; stderr:\n{}",
            args,
            String::from_utf8_lossy(&ours_output.stderr)
        );
    }
}

/// These options are intentional extensions. They are not part of the
/// Apache CLI, so the oracle must reject them while we accept them.
#[test]
fn extensions_are_accepted_by_us_but_not_by_the_original() {
    let Some(oracle) = find_oracle() else {
        eprintln!("skipping extension comparison: no original rotatelogs");
        return;
    };

    let cases: &[&[&str]] = &[
        &["-h"],
        &["-V"],
        &["--help"],
        &["-z", "log", "60"],
        &["--retain", "2", "log", "60"],
    ];

    for args in cases {
        let oracle_dir = tempfile::tempdir().expect("temp dir");
        let ours_dir = tempfile::tempdir().expect("temp dir");
        let oracle_output = run(&oracle, args, oracle_dir.path());
        let ours_output = run(Path::new(OURS), args, ours_dir.path());

        assert!(
            !oracle_output.status.success(),
            "expected original to reject extension {:?}; stderr:\n{}",
            args,
            String::from_utf8_lossy(&oracle_output.stderr)
        );
        assert!(
            ours_output.status.success(),
            "expected rotalog to accept extension {:?}; stderr:\n{}",
            args,
            String::from_utf8_lossy(&ours_output.stderr)
        );
    }
}

/// Behavioral comparison for options that are not visible in `-v` output.
#[test]
fn circular_truncate_and_echo_behavior_matches_the_original() {
    let Some(oracle) = find_oracle() else {
        eprintln!("skipping behavioral comparison: no original rotatelogs");
        return;
    };

    let chunks: &[&[u8]] = &[b"alpha\n", b"bravo\n", b"charlie\n"];
    let cases: &[&[&str]] = &[
        &["-n", "2", "log", "1B"],
        &["-t", "log", "1B"],
        &["-n", "2", "-L", "current.log", "log", "1B"],
    ];

    for args in cases {
        let oracle_dir = tempfile::tempdir().expect("temp dir");
        let ours_dir = tempfile::tempdir().expect("temp dir");
        let oracle_output = run_chunks(&oracle, args, oracle_dir.path(), chunks);
        let ours_output = run_chunks(Path::new(OURS), args, ours_dir.path(), chunks);

        assert!(oracle_output.status.success(), "oracle failed for {args:?}");
        assert!(ours_output.status.success(), "rotalog failed for {args:?}");
        assert_eq!(
            ours_output.stdout, oracle_output.stdout,
            "echo output differs for {args:?}"
        );

        let oracle_files = sorted_files(oracle_dir.path());
        let ours_files = sorted_files(ours_dir.path());
        assert_eq!(
            ours_files, oracle_files,
            "files differ for {args:?}\nours: {ours_files:?}\noracle: {oracle_files:?}"
        );
        for name in &oracle_files {
            assert_eq!(
                fs::read(ours_dir.path().join(name)).unwrap(),
                fs::read(oracle_dir.path().join(name)).unwrap(),
                "content differs for {name} with {args:?}"
            );
        }
    }
}

fn run_chunks(binary: &Path, args: &[&str], cwd: &Path, chunks: &[&[u8]]) -> Output {
    let mut full_args = vec!["-e"];
    full_args.extend_from_slice(args);

    let mut child = Command::new(binary)
        .args(&full_args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rotatelogs");

    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut echoed = Vec::new();
    for chunk in chunks {
        child
            .stdin
            .as_mut()
            .expect("stdin is piped")
            .write_all(chunk)
            .expect("write stdin");
        let mut observed = vec![0_u8; chunk.len()];
        stdout.read_exact(&mut observed).expect("read echo");
        assert_eq!(observed, *chunk, "log stream was not echoed verbatim");
        echoed.extend_from_slice(&observed);
    }
    drop(child.stdin.take());
    stdout.read_to_end(&mut echoed).expect("drain stdout");
    drop(stdout);

    let mut output = child.wait_with_output().expect("wait for rotatelogs");
    output.stdout = echoed;
    output
}

fn sorted_files(directory: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .expect("read temp directory")
        .map(|entry| entry.expect("directory entry").file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}
