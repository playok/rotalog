use std::collections::{BTreeMap, VecDeque};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, TrySendError, bounded, unbounded};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::config::{ArchiveFsync, MAX_COMPRESS_JOBS};

static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Bound the number of rotated files waiting for compression. When the queue
/// is full we leave the file uncompressed instead of blocking the writer or
/// growing memory without limit.
const MAINTENANCE_QUEUE_DEPTH: usize = 16;

/// Shared buffer size for compression input and output.
const COMPRESSION_BUFFER_SIZE: usize = 128 * 1024;
const QUEUE_WARNING_INTERVAL: Duration = Duration::from_secs(5);

struct Job {
    sequence: u64,
    path: PathBuf,
    permit: Permit,
}

struct Completed {
    sequence: u64,
    path: PathBuf,
    _permit: Permit,
}

/// A permit follows a file through queued, running and out-of-order completion
/// states. It is returned only after retention (or when a failed worker drops it).
#[derive(Debug)]
struct Permit(Arc<AtomicUsize>);

impl Permit {
    fn acquire(outstanding: &Arc<AtomicUsize>, limit: usize) -> Option<Self> {
        let mut count = outstanding.load(Ordering::Relaxed);
        loop {
            if count >= limit {
                return None;
            }
            match outstanding.compare_exchange_weak(
                count,
                count + 1,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Some(Self(Arc::clone(outstanding))),
                Err(actual) => count = actual,
            }
        }
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Keep the permit in the reorder map until all earlier files have completed.
fn complete_in_order(
    completed: Completed,
    pending: &mut BTreeMap<u64, Completed>,
    next: &mut u64,
    kept: &mut VecDeque<PathBuf>,
    retain: Option<usize>,
    verbose: bool,
) {
    pending.insert(completed.sequence, completed);
    while let Some(completed) = pending.remove(next) {
        if let Some(limit) = retain {
            retain_path(kept, completed.path, limit, verbose);
        }
        *next = next.saturating_add(1);
        // completed._permit is dropped here, after retention.
    }
}

/// Background workers for gzip compression and retention.
///
/// Rotations stay cheap: the writer only enqueues the retired path. Compression
/// runs on a small worker pool while a coordinator applies retention in
/// rotation order, even when compression workers finish out of order.
#[derive(Debug)]
pub struct Maintenance {
    sender: Option<Sender<Job>>,
    sequence: Mutex<u64>,
    outstanding: Arc<AtomicUsize>,
    outstanding_limit: usize,
    dropped_jobs: AtomicU64,
    last_full_warning: Mutex<Option<Instant>>,
    workers: Vec<JoinHandle<()>>,
    retention: Option<JoinHandle<()>>,
}

impl Maintenance {
    pub fn spawn(
        compress: bool,
        compress_level: u8,
        archive_fsync: ArchiveFsync,
        retain: Option<usize>,
        compress_jobs: usize,
        verbose: bool,
    ) -> io::Result<Self> {
        if !(1..=9).contains(&compress_level) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "gzip compression level must be between 1 and 9",
            ));
        }
        if !(1..=MAX_COMPRESS_JOBS).contains(&compress_jobs) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "compression jobs must be between 1 and 32",
            ));
        }

        let (sender, receiver) = bounded::<Job>(MAINTENANCE_QUEUE_DEPTH);
        // Every result owns an admission permit, so this channel plus the
        // reorder map cannot exceed 16 + compress_jobs accepted files.
        let (completed_tx, completed_rx) = unbounded::<Completed>();

        let retention = thread::Builder::new()
            .name("rotalog-retention".to_owned())
            .spawn(move || {
                let mut kept: VecDeque<PathBuf> = VecDeque::new();
                let mut pending = BTreeMap::new();
                let mut next = 0_u64;

                while let Ok(completed) = completed_rx.recv() {
                    if retain.is_none() {
                        // Compression alone needs neither ordering nor a growing map.
                        continue;
                    }
                    complete_in_order(
                        completed,
                        &mut pending,
                        &mut next,
                        &mut kept,
                        retain,
                        verbose,
                    );
                }

                if !pending.is_empty() {
                    eprintln!(
                        "rotalog: {} compressed files were not retained because a worker stopped",
                        pending.len()
                    );
                }
            })?;

        let mut workers = Vec::with_capacity(compress_jobs);
        for index in 0..compress_jobs {
            let receiver = receiver.clone();
            let completed_tx = completed_tx.clone();
            let handle = thread::Builder::new()
                .name(format!("rotalog-compress-{index}"))
                .spawn(move || {
                    while let Ok(job) = receiver.recv() {
                        let final_path = if compress {
                            match compress_file(&job.path, compress_level, archive_fsync, verbose) {
                                Ok(compressed) => compressed,
                                Err(error) => {
                                    eprintln!(
                                        "rotalog: could not compress {}: {error}; \
                                         keeping the uncompressed file",
                                        job.path.display()
                                    );
                                    job.path
                                }
                            }
                        } else {
                            job.path
                        };

                        if completed_tx
                            .send(Completed {
                                sequence: job.sequence,
                                path: final_path,
                                _permit: job.permit,
                            })
                            .is_err()
                        {
                            break;
                        }
                    }
                })?;
            workers.push(handle);
        }
        drop(completed_tx);
        drop(receiver);

        Ok(Self {
            sender: Some(sender),
            sequence: Mutex::new(0),
            outstanding: Arc::new(AtomicUsize::new(0)),
            outstanding_limit: MAINTENANCE_QUEUE_DEPTH + compress_jobs,
            dropped_jobs: AtomicU64::new(0),
            last_full_warning: Mutex::new(None),
            workers,
            retention: Some(retention),
        })
    }

    /// Queue a file for compression and/or retention.
    pub fn retire(&self, path: PathBuf) {
        let Some(sender) = &self.sender else {
            return;
        };
        let mut sequence = match self.sequence.lock() {
            Ok(sequence) => sequence,
            Err(poisoned) => poisoned.into_inner(),
        };

        let Some(permit) = Permit::acquire(&self.outstanding, self.outstanding_limit) else {
            self.warn_full();
            return;
        };
        match sender.try_send(Job {
            sequence: *sequence,
            path,
            permit,
        }) {
            Ok(()) => {
                *sequence = sequence.saturating_add(1);
            }
            Err(TrySendError::Full(_)) => {
                self.warn_full();
            }
            Err(TrySendError::Disconnected(_)) => {
                eprintln!("rotalog: maintenance workers stopped; rotated file was not queued");
            }
        }
    }

    fn warn_full(&self) {
        let dropped = self.dropped_jobs.fetch_add(1, Ordering::Relaxed) + 1;
        let now = Instant::now();
        let mut last = match self.last_full_warning.lock() {
            Ok(last) => last,
            Err(poisoned) => poisoned.into_inner(),
        };
        if last.is_none_or(|last| now.duration_since(last) >= QUEUE_WARNING_INTERVAL) {
            eprintln!(
                "rotalog: maintenance capacity is full; skipping compression/retention \
                 for a rotated file (total dropped: {dropped})"
            );
            *last = Some(now);
        }
    }

    /// Drain queued work and wait for all workers. Called during shutdown.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let dropped = self.dropped_jobs.load(Ordering::Relaxed);
        if dropped > 0 {
            eprintln!(
                "rotalog: {dropped} rotated files were not compressed/retained because \
                 maintenance capacity was full"
            );
        }
        // Dropping the sender closes the job queue. Workers drain the queued
        // jobs and exit; their completed senders then close the coordinator.
        self.sender.take();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
        if let Some(handle) = self.retention.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Maintenance {
    fn drop(&mut self) {
        self.stop();
    }
}

fn retain_path(kept: &mut VecDeque<PathBuf>, path: PathBuf, limit: usize, verbose: bool) {
    kept.push_back(path);
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
                eprintln!("rotalog: removed retained file {}", oldest.display());
            }
        }
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
    fn compression_without_retention_finishes_for_each_worker_count() {
        for jobs in [1, 2, 4] {
            let dir = tempfile::tempdir().unwrap();
            let maintenance =
                Maintenance::spawn(true, 1, ArchiveFsync::Never, None, jobs, false).unwrap();
            for index in 0..8 {
                let path = dir.path().join(index.to_string());
                fs::write(&path, vec![index as u8; 4096]).unwrap();
                maintenance.retire(path);
            }
            maintenance.finish();
            for index in 0..8 {
                let file = File::open(dir.path().join(format!("{index}.gz"))).unwrap();
                let mut decoder = flate2::read::GzDecoder::new(file);
                let mut data = Vec::new();
                decoder.read_to_end(&mut data).unwrap();
                assert_eq!(data, vec![index as u8; 4096]);
            }
        }
    }

    #[test]
    fn stalled_first_completion_bounds_pending_and_preserves_retention_order() {
        for jobs in [1, 2, 4] {
            let limit = MAINTENANCE_QUEUE_DEPTH + jobs;
            let outstanding = Arc::new(AtomicUsize::new(0));
            let dir = tempfile::tempdir().unwrap();
            let mut completed = VecDeque::new();
            for sequence in 0..limit {
                let path = dir.path().join(sequence.to_string());
                fs::write(&path, b"log").unwrap();
                completed.push_back(Completed {
                    sequence: sequence as u64,
                    path,
                    _permit: Permit::acquire(&outstanding, limit).unwrap(),
                });
            }
            // Model a slow sequence 0 while all later work finishes.
            let first = completed.pop_front().unwrap();
            let mut pending = BTreeMap::new();
            let mut kept = VecDeque::new();
            let mut next = 0;
            for item in completed {
                complete_in_order(item, &mut pending, &mut next, &mut kept, Some(2), false);
                assert!(Permit::acquire(&outstanding, limit).is_none());
            }
            assert_eq!(pending.len(), limit - 1);
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), limit);
            complete_in_order(first, &mut pending, &mut next, &mut kept, Some(2), false);
            assert!(pending.is_empty());
            assert_eq!(next, limit as u64);
            assert_eq!(outstanding.load(Ordering::Relaxed), 0);
            assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
            assert!(dir.path().join((limit - 1).to_string()).exists());
            assert!(dir.path().join((limit - 2).to_string()).exists());
            assert!(Permit::acquire(&outstanding, limit).is_some());
        }
    }

    #[test]
    fn rejected_jobs_release_permits_without_consuming_sequence_numbers() {
        let (sender, receiver) = bounded(MAINTENANCE_QUEUE_DEPTH);
        let mut maintenance = Maintenance {
            sender: Some(sender),
            sequence: Mutex::new(0),
            outstanding: Arc::new(AtomicUsize::new(0)),
            outstanding_limit: MAINTENANCE_QUEUE_DEPTH + 2,
            dropped_jobs: AtomicU64::new(0),
            last_full_warning: Mutex::new(None),
            workers: Vec::new(),
            retention: None,
        };
        for _ in 0..MAINTENANCE_QUEUE_DEPTH + 10 {
            maintenance.retire(PathBuf::from("unused"));
        }
        assert_eq!(
            maintenance.outstanding.load(Ordering::Relaxed),
            MAINTENANCE_QUEUE_DEPTH
        );
        assert_eq!(
            *maintenance.sequence.lock().unwrap(),
            MAINTENANCE_QUEUE_DEPTH as u64
        );
        assert_eq!(maintenance.dropped_jobs.load(Ordering::Relaxed), 10);
        drop(receiver); // A disconnected channel can still own queued jobs.
        maintenance.retire(PathBuf::from("unused"));
        maintenance.stop(); // Dropping both channel ends releases queued permits.
        assert_eq!(maintenance.outstanding.load(Ordering::Relaxed), 0);
    }

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
            1,
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
            1,
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
            1,
            false,
        )
        .unwrap();
        maintenance.retire(source.clone());
        maintenance.finish();

        let compressed = dir.path().join("access.log.1.gz");
        let mode = fs::metadata(compressed).unwrap().permissions().mode() & 0o7777;
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn parallel_compression_preserves_retention_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4)
            .map(|index| {
                let path = dir.path().join(format!("access.log.{index}"));
                fs::write(&path, vec![b'x'; 1024 + index]).unwrap();
                path
            })
            .collect();

        let maintenance = Maintenance::spawn(
            true,
            crate::config::DEFAULT_COMPRESS_LEVEL,
            ArchiveFsync::Never,
            Some(2),
            2,
            false,
        )
        .unwrap();
        for path in &paths {
            maintenance.retire(path.clone());
        }
        maintenance.finish();

        assert!(!gzip_path(&paths[0]).exists());
        assert!(!gzip_path(&paths[1]).exists());
        assert!(gzip_path(&paths[2]).exists());
        assert!(gzip_path(&paths[3]).exists());
        assert!(paths.iter().all(|path| !path.exists()));
    }
}
