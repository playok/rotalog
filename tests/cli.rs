//! End-to-end tests: feed the built binary through stdin and inspect the
//! resulting log files on disk.
//!
//! Rotation boundaries are made deterministic by using size-based rotation and
//! by waiting for `-e` echo before sending the next chunk. This avoids relying
//! on sleeps or wall-clock alignment.

use std::fs;
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_rotalog");

fn run(args: &[&str], stdin: &[u8]) -> std::process::Output {
    let mut child = Command::new(BIN)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rotalog");
    child
        .stdin
        .as_mut()
        .expect("stdin is piped")
        .write_all(stdin)
        .expect("write stdin");
    drop(child.stdin.take());
    child.wait_with_output().expect("wait for rotalog")
}

/// Feed input as distinct messages.
///
/// A single pipe write can be delivered to the child in one `read`, which
/// would produce a single log message. To make rotation boundaries
/// deterministic the child is started with `-e` and the test waits until the
/// echoed bytes come back before sending the next chunk.
fn run_chunks(args: &[&str], chunks: &[&[u8]]) -> std::process::Output {
    let mut full_args = vec!["-e"];
    full_args.extend_from_slice(args);

    let mut child = Command::new(BIN)
        .args(&full_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rotalog");

    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut echoed = Vec::new();
    for chunk in chunks {
        child
            .stdin
            .as_mut()
            .expect("stdin is piped")
            .write_all(chunk)
            .expect("write stdin");
        let mut observed = vec![0u8; chunk.len()];
        stdout.read_exact(&mut observed).expect("read echo");
        assert_eq!(observed, *chunk, "log stream was not echoed verbatim");
        echoed.extend_from_slice(&observed);
    }
    drop(child.stdin.take());
    stdout.read_to_end(&mut echoed).expect("drain stdout");
    drop(stdout);

    let mut output = child.wait_with_output().expect("wait for rotalog");
    output.stdout = echoed;
    output
}

fn stderr(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn matching_files(directory: &Path, prefix: &str) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .expect("read temp directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
        })
        .collect();
    files.sort();
    files
}

fn all_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(directory)
        .expect("read temp directory")
        .map(|entry| entry.expect("directory entry").path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();
    files
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().expect("poll child") {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("child did not exit within {timeout:?}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn writes_to_a_timestamped_log_file() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run(&[log.to_str().unwrap(), "86400"], b"hello\n");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let files = matching_files(directory.path(), "access.log");
    assert_eq!(files.len(), 1, "expected exactly one log file");
    let name = files[0].file_name().unwrap().to_str().unwrap();
    assert_eq!(name.len(), "access.log.".len() + 10, "{name}");
    assert!(
        name.rsplit('.')
            .next()
            .unwrap()
            .chars()
            .all(|c| c.is_ascii_digit())
    );
    assert_eq!(fs::read(&files[0]).unwrap(), b"hello\n");
}

#[test]
fn rotates_through_circular_file_slots() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run_chunks(
        &["-n", "2", log.to_str().unwrap(), "1B"],
        &[b"alpha\n", b"bravo\n", b"charlie\n"],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    assert_eq!(fs::read(&log).unwrap(), b"charlie\n");
    assert_eq!(
        fs::read(directory.path().join("access.log.1")).unwrap(),
        b"bravo\n"
    );
    assert_eq!(matching_files(directory.path(), "access.log").len(), 2);
}

#[test]
fn truncate_mode_keeps_one_file_and_the_latest_write() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run_chunks(
        &["-t", log.to_str().unwrap(), "1B"],
        &[b"alpha\n", b"bravo\n", b"charlie\n"],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    assert_eq!(matching_files(directory.path(), "access.log").len(), 1);
    assert_eq!(fs::read(&log).unwrap(), b"charlie\n");
}

#[test]
fn echo_copies_the_stream_to_stdout() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run(&["-e", log.to_str().unwrap(), "86400"], b"copy me\n");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(output.stdout, b"copy me\n");
}

#[test]
fn buffered_echo_copies_the_stream_to_stdout() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run(
        &[
            "-e",
            "--echo-flush",
            "buffered",
            log.to_str().unwrap(),
            "86400",
        ],
        b"copy me too\n",
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(output.stdout, b"copy me too\n");
}

#[cfg(unix)]
#[test]
fn link_option_points_at_the_current_log() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");
    let link = directory.path().join("current.log");

    let output = run(
        &["-L", link.to_str().unwrap(), log.to_str().unwrap(), "86400"],
        b"linked\n",
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    assert_eq!(fs::read(&link).unwrap(), b"linked\n");
    let files = matching_files(directory.path(), "access.log.");
    assert_eq!(files.len(), 1);
    assert_eq!(
        fs::metadata(&link).unwrap().ino(),
        fs::metadata(&files[0]).unwrap().ino(),
        "expected a hard link on the same filesystem"
    );
}

#[cfg(unix)]
#[test]
fn postrotate_program_receives_current_and_previous_names() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");
    let calls = directory.path().join("postrotate.log");
    let script = directory.path().join("postrotate.sh");

    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n",
            calls.display()
        ),
    )
    .expect("write script");
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).expect("chmod script");

    let output = run_chunks(
        &[
            "-p",
            script.to_str().unwrap(),
            "-n",
            "2",
            log.to_str().unwrap(),
            "1B",
        ],
        &[b"alpha\n", b"bravo\n", b"charlie\n"],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    // The hook runs asynchronously; allow it to finish.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let lines = fs::read_to_string(&calls).unwrap_or_default();
        if lines.lines().count() >= 3 {
            let lines: Vec<&str> = lines.lines().collect();
            assert_eq!(lines.len(), 3, "unexpected hook invocations: {lines:?}");
            let mut argument_counts: Vec<usize> = lines
                .iter()
                .map(|line| line.split_whitespace().count())
                .collect();
            argument_counts.sort_unstable();
            assert_eq!(argument_counts, vec![1, 2, 2]);
            assert!(
                lines
                    .iter()
                    .all(|line| line.starts_with(directory.path().to_str().unwrap()))
            );
            break;
        }
        assert!(Instant::now() < deadline, "post-rotate hook never ran");
        thread::sleep(Duration::from_millis(25));
    }
}

#[test]
fn force_open_creates_an_empty_file() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run(&["-f", log.to_str().unwrap(), "3600"], b"");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let files = matching_files(directory.path(), "access.log");
    assert_eq!(files.len(), 1);
    assert!(fs::read(&files[0]).unwrap().is_empty());
}

#[test]
fn create_empty_mode_opens_a_log_without_input() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let mut child = Command::new(BIN)
        .args(["-c", log.to_str().unwrap(), "1"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rotalog");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let files = matching_files(directory.path(), "access.log");
        if !files.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "-c should create a log file without input"
        );
        thread::sleep(Duration::from_millis(50));
    }

    drop(child.stdin.take());
    let output = child.wait_with_output().expect("wait for rotalog");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
}

#[test]
fn create_empty_with_size_only_is_inert() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run(&["-c", log.to_str().unwrap(), "5M"], b"");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(
        matching_files(directory.path(), "access.log").len(),
        0,
        "-c without a time interval must not create an empty file"
    );
}

#[test]
fn create_path_builds_parent_directories() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("nested/deep/access.log");

    let output = run(&["-D", log.to_str().unwrap(), "86400"], b"nested\n");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let files = matching_files(&directory.path().join("nested/deep"), "access.log");
    assert_eq!(files.len(), 1);
    assert_eq!(fs::read(&files[0]).unwrap(), b"nested\n");
}

#[test]
fn create_path_works_with_strftime_directories() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("%Y/%m/access.log");

    let output = run(&["-D", log.to_str().unwrap(), "86400"], b"dated\n");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let year = fs::read_dir(directory.path())
        .expect("read temp dir")
        .next()
        .expect("year directory")
        .unwrap()
        .path();
    assert!(year.is_dir());
}

#[test]
fn strftime_names_are_expanded() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access-%Y%m%d.log");

    let output = run(&[log.to_str().unwrap(), "86400"], b"dated\n");
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let files = matching_files(directory.path(), "access-");
    assert_eq!(files.len(), 1);
    let name = files[0].file_name().unwrap().to_str().unwrap();
    assert!(
        name.starts_with("access-") && name.ends_with(".log"),
        "{name}"
    );
    assert_eq!(name[7..15].len(), 8);
    assert_eq!(fs::read(&files[0]).unwrap(), b"dated\n");
}

#[test]
fn coarse_strftime_size_rotations_do_not_overwrite_each_other() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access-%Y%m%d.log");

    let output = run_chunks(
        &[log.to_str().unwrap(), "1B"],
        &[b"alpha\n", b"bravo\n", b"charlie\n"],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let files = matching_files(directory.path(), "access-");
    assert_eq!(files.len(), 3, "files: {files:?}");
    let contents: Vec<Vec<u8>> = files.iter().map(|path| fs::read(path).unwrap()).collect();
    assert_eq!(
        contents,
        vec![
            b"alpha\n".to_vec(),
            b"bravo\n".to_vec(),
            b"charlie\n".to_vec()
        ]
    );
}

#[test]
fn size_limit_rotates_before_exceeding() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run_chunks(&[log.to_str().unwrap(), "10B"], &[b"123456", b"abcdef"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let files = matching_files(directory.path(), "access.log");
    assert_eq!(files.len(), 2, "files: {files:?}");
    let mut contents: Vec<Vec<u8>> = files.iter().map(|path| fs::read(path).unwrap()).collect();
    contents.sort();
    assert_eq!(
        contents,
        vec![b"123456".to_vec(), b"abcdef".to_vec()],
        "rotalog rotates before writing a chunk that would exceed the limit; \
         Apache rotatelogs rotates only after the limit is exceeded"
    );
}

#[test]
fn compress_and_retain_rotated_files() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run_chunks(
        &[
            "-z",
            "--compress-level",
            "1",
            "--retain",
            "1",
            log.to_str().unwrap(),
            "1B",
        ],
        &[b"alpha\n", b"bravo\n", b"charlie\n"],
    );
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let files = all_files(directory.path());
    let gzip_files: Vec<&PathBuf> = files
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "gz"))
        .collect();
    assert_eq!(gzip_files.len(), 1, "files: {files:?}");

    let current: Vec<&PathBuf> = files
        .iter()
        .filter(|path| !path.to_string_lossy().ends_with(".gz"))
        .collect();
    assert_eq!(current.len(), 1, "files: {files:?}");
    assert_eq!(fs::read(current[0]).unwrap(), b"charlie\n");
}

#[test]
fn verbose_prints_the_effective_configuration() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run(&["-v", log.to_str().unwrap(), "86400"], b"verbose\n");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let diagnostics = stderr(&output);
    assert!(diagnostics.contains("rotalog configuration"));
    assert!(diagnostics.contains("rotation interval"));
}

#[test]
fn rejects_invalid_configuration() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");
    let log = log.to_str().unwrap();

    let output = run(&[log, "0"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("greater than zero"));

    let output = run(&["-l", log, "3600", "60"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("cannot be combined with -l"));

    let output = run(&["-p", "/bin/true", "-z", log, "60"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("cannot be combined with -z"));

    let output = run(&["--compress-level", "3", log, "60"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("requires -z"));

    let output = run(&[log], b"");
    assert_eq!(output.status.code(), Some(1));

    let output = run(&["--not-an-option", log, "60"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("unknown option"));

    let bad_pattern = directory.path().join("access-%Q.log");
    let output = run(&[bad_pattern.to_str().unwrap(), "60"], b"");
    assert_eq!(output.status.code(), Some(1));
    assert!(stderr(&output).contains("invalid strftime"));
}

#[test]
fn accepts_time_and_size_together() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let output = run(&[log.to_str().unwrap(), "3600", "5M"], b"both\n");
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(matching_files(directory.path(), "access.log").len(), 1);
}

#[cfg(target_os = "linux")]
#[test]
fn write_failure_exits_with_status_2() {
    let output = run(&["-t", "/dev/full", "86400"], b"this cannot be written\n");
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stderr(&output).contains("write to logfile /dev/full"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn help_and_version_exit_successfully() {
    let mut previous_help = None;
    for flag in ["-h", "--help"] {
        let output = run(&[flag], b"");
        assert!(output.status.success());
        assert!(output.stderr.is_empty(), "help must be printed to stdout");
        let help = String::from_utf8(output.stdout).unwrap();
        for option in [
            "--local-time",
            "--link",
            "--circular",
            "--truncate",
            "--truncate-rotated",
            "--create-empty",
            "--force",
            "--echo",
            "--echo-flush",
            "--program",
            "--create-dirs",
            "--verbose",
            "--compress",
            "--compress-level",
            "--compress-jobs",
            "--fsync-archives",
            "--retain",
            "--help",
            "--version",
        ] {
            assert!(help.contains(option), "missing help for {option}");
        }
        for detail in [
            "Usage:",
            "Date patterns:",
            "Exit status:",
            "Examples:",
            "Requires -e/--echo",
            "Range: 1-32; default: 1 worker",
            "Does not scan archives from previous runs",
            "data/never weaken durability",
            "catalina.out.%Y-%m-%d.log",
            "Author: playok",
            "Project: https://github.com/playok/rotalog",
        ] {
            assert!(help.contains(detail), "missing help detail: {detail}");
        }
        if let Some(previous) = previous_help {
            assert_eq!(
                help, previous,
                "-h and --help must provide the same details"
            );
        }
        previous_help = Some(help);
    }

    let output = run(&["--version"], b"");
    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("rotalog"));
}

#[cfg(unix)]
#[test]
fn sigterm_flushes_current_data_and_exits_cleanly() {
    let directory = tempfile::tempdir().expect("temp dir");
    let log = directory.path().join("access.log");

    let mut child = Command::new(BIN)
        .args(["-e", log.to_str().unwrap(), "86400"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rotalog");

    let mut stdout = child.stdout.take().expect("stdout is piped");
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"before-signal\n")
        .unwrap();
    let mut echoed = vec![0_u8; b"before-signal\n".len()];
    stdout.read_exact(&mut echoed).expect("read echo");

    let status = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(status.success());

    let status = wait_with_timeout(&mut child, Duration::from_secs(5));
    assert!(status.success(), "stderr: {}", stderr_for(&mut child));

    let files = matching_files(directory.path(), "access.log");
    assert_eq!(files.len(), 1);
    assert_eq!(fs::read(&files[0]).unwrap(), b"before-signal\n");
}

#[cfg(unix)]
fn stderr_for(child: &mut Child) -> String {
    let mut text = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut text);
    }
    text
}

#[test]
fn bounded_stdin_queue_preserves_large_stream_across_rotations() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("bulk.log");
    let data: Vec<u8> = (0..4 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    let output = run(&[log.to_str().unwrap(), "1M"], &data);
    assert!(output.status.success(), "{}", stderr(&output));
    let files = matching_files(dir.path(), "bulk.log");
    assert!((4..=5).contains(&files.len()));
    let mut actual = Vec::new();
    for file in files {
        let chunk = fs::read(file).unwrap();
        assert!(chunk.len() <= 1024 * 1024);
        actual.extend(chunk);
    }
    assert_eq!(actual, data);
}

#[cfg(unix)]
#[test]
fn sigterm_with_backpressured_echo_keeps_written_and_echoed_bytes_equal() {
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("active.log");
    let input = dir.path().join("input.bin");
    let data: Vec<u8> = (0..8 * 1024 * 1024).map(|i| (i % 251) as u8).collect();
    fs::write(&input, &data).unwrap();
    let mut child = Command::new(BIN)
        .args(["-e", log.to_str().unwrap(), "86400"])
        .stdin(fs::File::open(input).unwrap())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdout = child.stdout.take().unwrap();
    // Reading only a prefix leaves the echo pipe under backpressure; the input
    // is much larger than the pipe and queue combined.
    let mut prefix = vec![0; 1024];
    stdout.read_exact(&mut prefix).unwrap();
    assert!(
        Command::new("kill")
            .args(["-TERM", &child.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let reader = thread::spawn(move || {
        stdout.read_to_end(&mut prefix).unwrap();
        prefix
    });
    assert!(wait_with_timeout(&mut child, Duration::from_secs(5)).success());
    let echoed = reader.join().unwrap();
    let files = matching_files(dir.path(), "active.log");
    assert_eq!(files.len(), 1);
    assert_eq!(fs::read(&files[0]).unwrap(), echoed);
    assert!(data.starts_with(&echoed));
}
