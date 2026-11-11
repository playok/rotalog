use std::io;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;

/// Run a post-rotate program without blocking the logger.
///
/// The program is executed directly (not through a shell), mirroring Apache's
/// use of `apr_proc_create`. It receives the new filename first and the
/// previous filename second when a rotation occurred. A small reaper thread
/// waits for the child so it cannot become a zombie.
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

    let mut child = command.spawn()?;
    if let Err(error) = thread::Builder::new()
        .name("post-rotate-reaper".to_owned())
        .spawn(move || {
            let _ = child.wait();
        })
    {
        // The child is already running; losing the reaper only risks a
        // zombie, so do not report the program itself as failed.
        eprintln!("rotalog: could not start post-rotate reaper thread: {error}");
    }
    Ok(())
}
