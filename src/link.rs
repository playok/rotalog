use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMPORARY_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically point `link` at `target`.
///
/// Apache rotatelogs creates a hard link. We try the same thing (and fall back
/// to a symlink when the link name is on another filesystem), but replace the
/// link through a temporary name so `tail -F` never observes a missing link.
pub fn update(link: &Path, target: &Path, create_dirs: bool, verbose: bool) -> io::Result<()> {
    if create_dirs {
        if let Some(parent) = link.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
    }

    // If the convenience link already names the same file, there is nothing
    // to replace. This is important for -t, where the link and the logfile
    // can resolve to the same path.
    if same_file(link, target) {
        return Ok(());
    }

    let temporary = temporary_path(link)?;
    remove_if_exists(&temporary)?;

    // Hard links accept relative targets; canonicalization is only needed for
    // the symlink fallback below.
    let hard_link_result = fs::hard_link(target, &temporary);
    let method = match hard_link_result {
        Ok(()) => "hard link",
        Err(hard_link_error) => {
            // A hard link cannot cross filesystems (or some network mounts).
            // A symlink is more broadly usable and still works with tail -F.
            #[cfg(unix)]
            {
                let absolute_target = fs::canonicalize(target)?;
                std::os::unix::fs::symlink(&absolute_target, &temporary).map_err(|symlink_error| {
                    io::Error::new(
                        symlink_error.kind(),
                        format!(
                            "hard link failed ({hard_link_error}); symlink also failed ({symlink_error})"
                        ),
                    )
                })?;
                "symlink"
            }
            #[cfg(not(unix))]
            {
                return Err(hard_link_error);
            }
        }
    };

    if let Err(error) = fs::rename(&temporary, link) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    if verbose {
        eprintln!(
            "rotalog: linked {} -> {} ({method})",
            link.display(),
            target.display()
        );
    }
    Ok(())
}

fn temporary_path(link: &Path) -> io::Result<PathBuf> {
    let file_name = link
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "link name has no file name"))?;

    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!(
        ".{}.{}.tmp",
        std::process::id(),
        TEMPORARY_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));

    let parent = link
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    Ok(parent.join(name))
}

fn remove_if_exists(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn same_file(first: &Path, second: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(first), Ok(second)) = (fs::metadata(first), fs::metadata(second)) {
            return first.dev() == second.dev() && first.ino() == second.ino();
        }
    }

    let Ok(first) = fs::canonicalize(first) else {
        return false;
    };
    let Ok(second) = fs::canonicalize(second) else {
        return false;
    };
    first == second
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_and_replaces_link() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first.log");
        let second = dir.path().join("second.log");
        let link = dir.path().join("current.log");
        fs::write(&first, b"first").unwrap();
        fs::write(&second, b"second").unwrap();

        update(&link, &first, false, false).unwrap();
        assert_eq!(fs::read(&link).unwrap(), b"first");

        update(&link, &second, false, false).unwrap();
        assert_eq!(fs::read(&link).unwrap(), b"second");
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }

    #[test]
    fn creates_link_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.log");
        let link = dir.path().join("nested/dir/current.log");
        fs::write(&target, b"data").unwrap();

        update(&link, &target, true, false).unwrap();
        assert_eq!(fs::read(&link).unwrap(), b"data");
    }

    #[test]
    fn leaves_a_link_that_already_points_at_the_target_alone() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.log");
        fs::write(&target, b"data").unwrap();

        update(&target, &target, false, false).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"data");
    }
}
