use std::io::{self, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use signal_hook::consts::{SIGINT, SIGTERM};

use crate::config::Config;
use crate::error::{Error, IoContext, Result};
use crate::writer::Writer;

const READ_CHUNK: usize = 64 * 1024;
const TICK: Duration = Duration::from_millis(200);
const QUEUE_DEPTH: usize = 64;

enum Input {
    Data(Vec<u8>),
    Eof,
    Error(io::Error),
}

/// Read stdin, write rotating files, and shut down cleanly on EOF or signals.
///
/// The reader thread provides backpressure through a bounded channel while the
/// main thread owns the rotation state. This keeps the writer single-threaded
/// (no lock contention around the file) and still lets a quiet pipe be ticked
/// for `-c` and graceful signal handling.
pub fn run(cfg: Config) -> Result<()> {
    let open_immediately = cfg.force_open;

    let mut writer = Writer::new(cfg)?;
    if open_immediately {
        writer.open_initial(now_seconds()?)?;
    }

    let shutdown = Arc::new(AtomicBool::new(false));
    signal_hook::flag::register(SIGTERM, Arc::clone(&shutdown))
        .context("register SIGTERM handler")?;
    signal_hook::flag::register(SIGINT, Arc::clone(&shutdown))
        .context("register SIGINT handler")?;

    let (sender, receiver) = mpsc::sync_channel(QUEUE_DEPTH);
    spawn_reader(sender)?;

    let loop_result = drive_loop(&mut writer, &receiver, &shutdown);
    let finish_result = writer.finish();

    loop_result?;
    finish_result
}

fn drive_loop(
    writer: &mut Writer,
    receiver: &Receiver<Input>,
    shutdown: &AtomicBool,
) -> Result<()> {
    loop {
        if shutdown.load(Ordering::Relaxed) {
            drain_queue(writer, receiver)?;
            return Ok(());
        }

        match receiver.recv_timeout(TICK) {
            Ok(Input::Data(data)) => writer.write(&data, now_seconds()?)?,
            Ok(Input::Eof) => return Ok(()),
            Ok(Input::Error(error)) => {
                return Err(Error::io("read from stdin", error));
            }
            Err(RecvTimeoutError::Timeout) => writer.tick(now_seconds()?)?,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        }
    }
}

/// On SIGTERM/SIGINT, write out records that already reached the queue before
/// shutting down. The reader thread may still be blocked on stdin; the process
/// exits once the writer is finished.
fn drain_queue(writer: &mut Writer, receiver: &Receiver<Input>) -> Result<()> {
    while let Ok(message) = receiver.try_recv() {
        match message {
            Input::Data(data) => writer.write(&data, now_seconds()?)?,
            Input::Eof => break,
            Input::Error(_) => break,
        }
    }
    Ok(())
}

fn spawn_reader(sender: SyncSender<Input>) -> Result<()> {
    thread::Builder::new()
        .name("stdin-reader".to_owned())
        .spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buffer = vec![0_u8; READ_CHUNK];

            loop {
                match stdin.read(&mut buffer) {
                    Ok(0) => {
                        let _ = sender.send(Input::Eof);
                        break;
                    }
                    Ok(count) => {
                        let chunk = buffer[..count].to_vec();
                        if sender.send(Input::Data(chunk)).is_err() {
                            break;
                        }
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                    Err(error) => {
                        let _ = sender.send(Input::Error(error));
                        break;
                    }
                }
            }
        })
        .context("start stdin reader thread")?;
    Ok(())
}

fn now_seconds() -> Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::runtime("system clock is before the Unix epoch"))?;
    i64::try_from(duration.as_secs())
        .map_err(|_| Error::runtime("system clock is outside the supported range"))
}
