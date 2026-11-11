use std::io;
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::Duration;

static REAPER: OnceLock<Option<mpsc::Sender<Child>>> = OnceLock::new();

/// Run a post-rotate program without blocking the logger.
///
/// The program is executed directly (not through a shell), mirroring Apache's
/// use of `apr_proc_create`. It receives the new filename first and the
/// previous filename second when a rotation occurred. Children are reaped by a
/// single background thread so a hung program cannot create one thread per
/// rotation.
pub fn spawn(program: &Path, current: &Path, previous: Option<&Path>) -> io::Result<()> {
    let mut command = Command::new(program);
    command.arg(current);
    if let Some(previous) = previous {
        command.arg(previous);
    }
    command
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let child = command.spawn()?;
    match reaper() {
        Some(sender) => match sender.send(child) {
            Ok(()) => {}
            Err(error) => spawn_fallback(error.0),
        },
        None => spawn_fallback(child),
    }
    Ok(())
}

fn reaper() -> Option<&'static mpsc::Sender<Child>> {
    REAPER
        .get_or_init(|| {
            let (sender, receiver) = mpsc::channel::<Child>();
            let result = thread::Builder::new()
                .name("post-rotate-reaper".to_owned())
                .spawn(move || {
                    let mut children: Vec<Child> = Vec::new();
                    loop {
                        while let Ok(child) = receiver.try_recv() {
                            children.push(child);
                        }

                        children.retain_mut(|child| match child.try_wait() {
                            Ok(Some(_)) => false,
                            Ok(None) => true,
                            Err(_) => false,
                        });

                        if children.is_empty() {
                            match receiver.recv_timeout(Duration::from_millis(250)) {
                                Ok(child) => children.push(child),
                                Err(mpsc::RecvTimeoutError::Timeout) => {}
                                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                            }
                        } else {
                            thread::sleep(Duration::from_millis(100));
                        }
                    }

                    for mut child in children {
                        let _ = child.wait();
                    }
                });

            match result {
                Ok(_) => Some(sender),
                Err(error) => {
                    eprintln!("rotalog: could not start post-rotate reaper thread: {error}");
                    None
                }
            }
        })
        .as_ref()
}

fn spawn_fallback(mut child: Child) {
    if let Err(error) = thread::Builder::new()
        .name("post-rotate-reaper-fallback".to_owned())
        .spawn(move || {
            let _ = child.wait();
        })
    {
        eprintln!("rotalog: could not start fallback post-rotate reaper: {error}");
    }
}
