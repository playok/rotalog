use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};

use flate2::Compression;
use flate2::write::GzEncoder;

use crate::config::ArchiveFsync;

static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Bound the number of rotated files waiting for compression. When the queue
/// is full we leave the file uncompressed instead of blocking the writer or
/// growing memory without limit.
const MAINTENANCE_QUEUE_DEPTH: usize = 16;

/// Shared buffer size for compression input and output.
const COMPRESSION_BUFFER_SIZE: usize = 128 * 1024;

enum Job {
    Retire(PathBuf),
    Shutdown,
}

/// Background worker for gzip compression and retention.
///
/// Rotations stay cheap: the writer only enqueues the retired path. Gzip work
/// and deletion happen on this thread, so a multi-gigabyte log never stalls the
/// pipe from httpd.
#[derive(Debug)]
pub struct Maintenance {
    sender: Option<SyncSender<Job>>,
    handle: Option<JoinHandle<()>>,
}

impl Maintenance {
    pub fn spawn(
        compress: bool,
        compress_level: u8,
        archive_fsync: ArchiveFsync,
        retain: Option<usize>,
        verbose: bool,
    ) -> io::Result<Self> {
        if !(1..=9).contains(&compress_level) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gzip compression level must be between 1 and 9",
            ));
        }
        let (sender, receiver) = mpsc::sync_channel::<Job>(MAINTENANCE_QUEUE_DEPTH);
        let handle = thread::Builder::new()
            .name("rotalog-maintenance".to_owned())
            .spawn(move || {
                let mut kept: VecDeque<PathBuf> = VecDeque::new();

                while let Ok(job) = receiver.recv() {
                    match job {
                        Job::Retire(path) => {
                            let final_path = if compress {
                                match compress_file(&path, compress_level, archive_fsync, verbose) {
                                    Ok(compressed) => compressed,
                                    Err(error) => {
                                        eprintln!(
                                            "rotalog: could not compress {}: {error}; \
                                             keeping the uncompressed file",
                                            path.display()
                                        );
                                        path
                                    }
                                }
                            } else {
                                path
                            };

                            if let Some(limit) = retain {
                                kept.push_back(final_path);
                                while kept.len() > limit {
                                    if let Some(oldest) = kept.pop_front() {
                                        if let Err(error) = fs::remove_file(&oldest) {
                                            if error.kind() != io::ErrorKind::NotFound {
                                                eprintln!(
                                                    "rotalog: could not remove retained file {}: {error}",
                                                    oldest.display()
                                                );
                                            }
                                        } else if verbose {
                                            eprintln!(
                                                "rotalog: removed retained file {}",
                                                oldest.display()
                                            );
                                        }
                                    }
                                }
                            }
                        }
                        Job::Shutdown => break,
                    }
                }
            })?;

        Ok(Self {
            sender: Some(sender),
            handle: Some(handle),
        })
    }

    /// Queue a file for compression and/or retention.
    pub fn retire(&self, path: PathBuf) {
        if let Some(sender) = &self.sender {
            match sender.try_send(Job::Retire(path)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    eprintln!(
                        "rotalog: maintenance queue is full; leaving a rotated file uncompressed"
                    );
                }
                Err(TrySendError::Disconnected(_)) => {
                    eprintln!("rotalog: maintenance worker stopped; rotated file was not queued");
                }
            }
        }
    }

    /// Drain queued work and wait for the worker. Called during shutdown.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(sender) = self.sender.take() {
            // The worker drains queued Retire jobs before observing Shutdown.
            // If the bounded queue is full, dropping the sender lets the
            // worker drain the queue and then exit on Disconnected.
            let _ = sender.try_send(Job::Shutdown);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        self.stop();
    }
}

fn compress_file(
    source: &Path,
    level: u8,
    archive_fsync: ArchiveFsync,
    verbose: bool,
) -> io::Result<PathBuf> {
    let destination = gzip_path(source);
    let (temporary, output) = open_temporary(&destination)?;

    let result = (|| -> io::Result<()> {
        let mut input = File::open(source)?;
        let metadata = input.metadata()?;
        let output = BufWriter::with_capacity(COMPRESSION_BUFFER_SIZE, output);
        let mut encoder = GzEncoder::new(output, Compression::new(level as u32));
        let mut buffer = vec![0_u8; COMPRESSION_BUFFER_SIZE];

        loop {
            let read = input.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            encoder.write_all(&buffer[..read])?;
        }

        let mut output = encoder.finish()?;
        output.flush()?;
        // The original is removed below; respect the configured durability
        // policy before that happens.
        match archive_fsync {
            ArchiveFsync::Always => output.get_ref().sync_all()?,
            ArchiveFsync::Data => output.get_ref().sync_data()?,
            ArchiveFsync::Never => {}
        }
        drop(output);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = metadata.permissions();
            permissions.set_mode(permissions.mode() & 0o777);
            fs::set_permissions(&temporary, permissions)?;
        }
        #[cfg(not(unix))]
        {
            fs::set_permissions(&temporary, metadata.permissions())?;
        }

        fs::rename(&temporary, &destination)?;
        if let Err(error) = fs::remove_file(source) {
            if error.kind() != io::ErrorKind::NotFound {
                eprintln!(
                    "rotalog: compressed {} but could not remove the original: {error}",
                    source.display()
                );
            }
        }
        Ok(())
    })();

    match result {
        Ok(()) => {
            if verbose {
                eprintln!(
                    "rotalog: compressed {} -> {}",
                    source.display(),
                    destination.display()
                );
            }
            Ok(destination)
        }
        Err(error) => {
            let _ = fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Open a compression temp file without following symlinks or reusing an
/// existing path. `create_new` maps to O_CREAT|O_EXCL on Unix.
fn open_temporary(destination: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..100 {
        let temporary = gzip_temporary_path(destination);
        match create_new_file(&temporary) {
            Ok(file) => return Ok((temporary, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique compression temporary file",
    ))
}

fn create_new_file(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

fn gzip_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".gz");
    PathBuf::from(name)
}

fn gzip_temporary_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(format!(
        ".tmp.{}.{}",
        std::process::id(),
        TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compresses_retired_files() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("access.log.1");
        fs::write(&first, b"hello world").unwrap();

        let maintenance = Maintenance::spawn(
            true,
            crate::config::DEFAULT_COMPRESS_LEVEL,
            ArchiveFsync::Always,
            None,
            false,
        )
        .unwrap();
        maintenance.retire(first.clone());
        maintenance.finish();

        assert!(!first.exists());
        let compressed = dir.path().join("access.log.1.gz");
        assert!(compressed.exists());

        let mut decoder = flate2::read::GzDecoder::new(File::open(compressed).unwrap());
        let mut text = String::new();
        std::io::Read::read_to_string(&mut decoder, &mut text).unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn retention_deletes_the_oldest_files() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (1..=3)
            .map(|n| {
                let path = dir.path().join(format!("access.log.{n}"));
                fs::write(&path, format!("data-{n}")).unwrap();
                path
            })
            .collect();

        let maintenance = Maintenance::spawn(
            false,
            crate::config::DEFAULT_COMPRESS_LEVEL,
            ArchiveFsync::Always,
            Some(2),
            false,
        )
        .unwrap();
        for path in &paths {
            maintenance.retire(path.clone());
        }
        maintenance.finish();

        assert!(!paths[0].exists());
        assert!(paths[1].exists());
        assert!(paths[2].exists());
    }

    #[cfg(unix)]
    #[test]
    fn compression_temporary_file_does_not_follow_a_symlink() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let victim = dir.path().join("victim");
        let temporary = dir.path().join("archive.gz.tmp.123");
        fs::write(&victim, b"safe").unwrap();
        symlink(&victim, &temporary).unwrap();

        let error = create_new_file(&temporary).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&victim).unwrap(), b"safe");
    }

    #[cfg(unix)]
    #[test]
    fn compression_strips_special_permission_bits() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("access.log.1");
        fs::write(&source, b"hello").unwrap();
        fs::set_permissions(&source, fs::Permissions::from_mode(0o4755)).unwrap();

        let maintenance = Maintenance::spawn(
            true,
            crate::config::DEFAULT_COMPRESS_LEVEL,
            ArchiveFsync::Always,
            None,
            false,
        )
        .unwrap();
        maintenance.retire(source.clone());
        maintenance.finish();

        let compressed = dir.path().join("access.log.1.gz");
        let mode = fs::metadata(compressed).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o755);
    }
}
