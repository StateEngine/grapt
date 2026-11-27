//! Capture one bounded private configuration, keeping the opened object stable.
use std::{
    fs::OpenOptions,
    io::{self, Read},
    path::Path,
};

pub fn read_owner_only_file(path: &Path, bound: usize) -> io::Result<Vec<u8>> {
    let before = std::fs::symlink_metadata(path)?;
    if !before.is_file() || before.file_type().is_symlink() || before.len() > bound as u64 {
        return Err(io::Error::other(
            "private configuration must be bounded, regular and unlinked",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000).share_mode(1);
    }
    let file = options.open(path)?;
    verify_owner_only_file(path, &file)?;
    let opened = file.metadata()?;
    let mut bytes = Vec::new();
    let capture = (bound as u64).checked_add(1).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "private configuration bound is invalid",
        )
    })?;
    (&file).take(capture).read_to_end(&mut bytes)?;
    if bytes.len() > bound {
        return Err(io::Error::other("private configuration exceeds its bound"));
    }
    verify_owner_only_file(path, &file)?;
    let after = file.metadata()?;
    if opened.len() != after.len()
        || opened.modified()? != after.modified()?
        || bytes.len() as u64 != after.len()
    {
        return Err(io::Error::other(
            "private configuration changed while reading",
        ));
    }
    Ok(bytes)
}

/// Verify a caller-owned handle without reopening the data stream. Lock-file
/// callers can allow write sharing while retaining strict owner-only authority.
/// Keep the handle alive throughout use; Windows callers must deny delete sharing.
pub fn verify_owner_only_file(path: &Path, file: &std::fs::File) -> io::Result<()> {
    #[cfg(not(unix))]
    super::verify_private_file(path, file)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let opened = file.metadata()?;
        let selected = std::fs::symlink_metadata(path)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("private configuration has no parent"))?;
        // Stat the pathname; reopening it for identity comparison could block
        // on a FIFO installed between the initial stat and this read.
        if !selected.is_file()
            || selected.file_type().is_symlink()
            || (opened.dev(), opened.ino()) != (selected.dev(), selected.ino())
            || opened.uid() != std::fs::metadata(parent)?.uid()
            || opened.permissions().mode() & 0o077 != 0
            || opened.nlink() != 1
        {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "configuration is not owner-only",
            ));
        }
    }
    #[cfg(windows)]
    super::windows::run(
        "file-private-check",
        path.to_str()
            .ok_or_else(|| io::Error::other("private configuration path is not Unicode"))?
            .as_bytes(),
    )?;
    #[cfg(windows)]
    super::verify_private_file(path, file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_snapshot_refuses_exposure_links_and_overflow() -> io::Result<()> {
        let _protection = super::super::ProtectionBatch::begin()?;
        let root =
            std::env::temp_dir().join(format!("grapt-private-snapshot-{}", std::process::id()));
        std::fs::create_dir(&root)?;
        let result = (|| {
            super::super::protect_directory(&root)?;
            let path = root.join("config");
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&path)?;
            std::io::Write::write_all(&mut file, b"private")?;
            super::super::protect_new_private_file(&path, &file)?;
            drop(file);
            assert_eq!(read_owner_only_file(&path, 7)?, b"private");
            assert!(read_owner_only_file(&path, 6).is_err());
            let mut options = OpenOptions::new();
            options.read(true).write(true);
            #[cfg(windows)]
            std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 3);
            let file = options.open(&path)?;
            verify_owner_only_file(&path, &file)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))?;
                assert!(read_owner_only_file(&path, 7).is_err());
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
                std::os::unix::fs::symlink(&path, root.join("alias"))?;
                assert!(read_owner_only_file(&root.join("alias"), 7).is_err());
            }
            #[cfg(windows)]
            {
                let system = std::env::var_os("SystemRoot")
                    .ok_or_else(|| io::Error::other("SystemRoot is absent"))?;
                let output =
                    std::process::Command::new(Path::new(&system).join("System32/icacls.exe"))
                        .arg(&path)
                        .args(["/grant", "*S-1-1-0:R"])
                        .output()?;
                assert!(output.status.success(), "fixture ACL grant failed");
                assert!(verify_owner_only_file(&path, &file).is_err());
                std::thread::scope(|scope| {
                    scope.spawn(|| assert!(verify_owner_only_file(&path, &file).is_err()));
                });
                let output =
                    std::process::Command::new(Path::new(&system).join("System32/icacls.exe"))
                        .arg(&path)
                        .args(["/remove:g", "*S-1-1-0"])
                        .output()?;
                assert!(output.status.success(), "fixture ACL restoration failed");
                verify_owner_only_file(&path, &file)?;
            }
            std::fs::hard_link(&path, root.join("hard"))?;
            assert!(verify_owner_only_file(&path, &file).is_err());
            drop(file);
            Ok(())
        })();
        let _removed = std::fs::remove_dir_all(root);
        result
    }
}

#[cfg(all(test, unix))]
mod path_race_tests {
    use super::*;

    static NEXT_SCRATCH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

    struct ScratchDir(std::path::PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> io::Result<Self> {
            let pid = std::process::id();
            loop {
                let index = NEXT_SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let path = std::env::temp_dir().join(format!("grapt-{name}-{pid}-{index}"));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Ok(Self(path)),
                    Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                    Err(error) => return Err(error),
                }
            }
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_fifo_replacement_refuses_without_reopening_the_selected_path() -> io::Result<()> {
        let root = ScratchDir::new("private-fifo")?;
        super::super::protect_directory(&root.0)?;
        let path = root.0.join("configuration");
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        super::super::protect_new_private_file(&path, &file)?;
        std::fs::remove_file(&path)?;
        let mut maker = std::process::Command::new("mkfifo").arg(&path).spawn()?;
        let mut status = None;
        for _ in 0..200 {
            if let Some(done) = maker.try_wait()? {
                status = Some(done);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        if !status.is_some_and(|status| status.success()) {
            let _killed = maker.kill();
            let _joined = maker.wait();
            return Err(io::Error::other("FIFO fixture creation failed"));
        }
        let (send, receive) = std::sync::mpsc::sync_channel(1);
        let check = std::thread::spawn(move || {
            let _sent = send.send(verify_owner_only_file(&path, &file).is_err());
        });
        assert_eq!(
            receive.recv_timeout(std::time::Duration::from_secs(2)),
            Ok(true),
            "private read reopened a blocking FIFO after its file check"
        );
        check
            .join()
            .map_err(|_| io::Error::other("FIFO fixture verifier failed"))?;
        Ok(())
    }
}
