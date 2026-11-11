use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::thread::{self, JoinHandle};

use flate2::Compression;
use flate2::write::GzEncoder;

enum Job {
    Retire(PathBuf),
    Shutdown,
}

/// Background worker for gzip compression and retention.
///
/// Rotations stay cheap: the writer only enqueues the retired path. Gzip work
/// and deletion happen on this thread, so a multi-gigabyte log never stalls the
/// pipe from httpd.
pub struct Maintenance {
    sender: Option<Sender<Job>>,
    handle: Option<JoinHandle<()>>,
}

impl Maintenance {
    pub fn spawn(compress: bool, retain: Option<usize>, verbose: bool) -> io::Result<Self> {
        let (sender, receiver) = mpsc::channel::<Job>();
        let handle = thread::Builder::new()
            .name("rotatelogs-maintenance".to_owned())
            .spawn(move || {
                let mut kept: VecDeque<PathBuf> = VecDeque::new();

                while let Ok(job) = receiver.recv() {
                    match job {
                        Job::Retire(path) => {
                            let final_path = if compress {
                                match compress_file(&path, verbose) {
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
            if sender.send(Job::Retire(path)).is_err() {
                eprintln!("rotalog: maintenance worker stopped; rotated file was not queued");
            }
        }
    }

    /// Drain queued work and wait for the worker. Called during shutdown.
    pub fn finish(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(Job::Shutdown);
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

fn compress_file(source: &Path, verbose: bool) -> io::Result<PathBuf> {
    let destination = gzip_path(source);
    let temporary = gzip_temporary_path(&destination);

    let result = (|| -> io::Result<()> {
        let input = File::open(source)?;
        let metadata = input.metadata()?;
        let output = File::create(&temporary)?;
        let mut encoder = GzEncoder::new(BufWriter::new(output), Compression::default());
        io::copy(&mut BufReader::new(input), &mut encoder)?;
        let mut output = encoder.finish()?;
        output.flush()?;
        // The original is removed below; make sure the archive is durable
        // before that happens.
        output.get_ref().sync_all()?;
        drop(output);

        fs::set_permissions(&temporary, metadata.permissions())?;
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

fn gzip_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".gz");
    PathBuf::from(name)
}

fn gzip_temporary_path(destination: &Path) -> PathBuf {
    let mut name = destination.as_os_str().to_os_string();
    name.push(format!(".tmp.{}", std::process::id()));
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

        let maintenance = Maintenance::spawn(true, None, false).unwrap();
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

        let maintenance = Maintenance::spawn(false, Some(2), false).unwrap();
        for path in &paths {
            maintenance.retire(path.clone());
        }
        maintenance.finish();

        assert!(!paths[0].exists());
        assert!(paths[1].exists());
        assert!(paths[2].exists());
    }
}
