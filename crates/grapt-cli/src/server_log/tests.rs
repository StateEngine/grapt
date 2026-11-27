use super::*;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-server-log-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn launch_rotation_retains_three_logs_and_appends_below_the_threshold() -> io::Result<()> {
    let root = Scratch::new()?;
    for generation in 0..5 {
        let mut file = open(&root.0)?;
        file.write_all(&[generation])?;
        OpenOptions::new()
            .write(true)
            .open(path(&root.0))?
            .set_len(ROTATE_BYTES + 1)?;
    }
    let file = open(&root.0)?;
    assert_eq!(file.metadata()?.len(), 0);
    assert_eq!(std::fs::read(root.0.join("logs/server.log.1"))?[0], 4);
    assert_eq!(std::fs::read(root.0.join("logs/server.log.2"))?[0], 3);
    let logs = std::fs::read_dir(root.0.join("logs"))?.collect::<io::Result<Vec<_>>>()?;
    assert_eq!(
        logs.iter()
            .filter(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with("server.log"))
            .count(),
        3
    );
    drop(file);
    open(&root.0)?.write_all(b"first\n")?;
    open(&root.0)?.write_all(b"second\n")?;
    assert_eq!(std::fs::read(path(&root.0))?, b"first\nsecond\n");
    Ok(())
}

#[test]
fn launch_file_does_not_keep_the_preparation_lock() -> io::Result<()> {
    let root = Scratch::new()?;
    let mut first = open(&root.0)?;
    let mut second = open(&root.0)?;
    first.write_all(b"first\n")?;
    second.write_all(b"second\n")?;
    assert_eq!(std::fs::read(path(&root.0))?, b"first\nsecond\n");
    Ok(())
}

#[test]
fn blocked_rotation_paths_defer_retention_without_truncating_the_current_log() -> io::Result<()> {
    let root = Scratch::new()?;
    let file = open(&root.0)?;
    OpenOptions::new()
        .write(true)
        .open(path(&root.0))?
        .set_len(ROTATE_BYTES + 1)?;
    drop(file);
    std::fs::create_dir(root.0.join("logs/server.log.1"))?;
    let mut file = open(&root.0)?;
    file.write_all(b"still capturing\n")?;
    assert_eq!(
        std::fs::metadata(path(&root.0))?.len(),
        ROTATE_BYTES + 1 + 16
    );
    Ok(())
}

#[test]
fn a_second_launch_rotates_while_the_first_server_handle_is_alive() -> io::Result<()> {
    let root = Scratch::new()?;
    let mut first = open(&root.0)?;
    OpenOptions::new()
        .write(true)
        .open(path(&root.0))?
        .set_len(ROTATE_BYTES + 1)?;
    let mut second = open(&root.0)?;
    second.write_all(b"new\n")?;
    first.write_all(b"old\n")?;
    assert_eq!(std::fs::read(path(&root.0))?, b"new\n");
    assert_eq!(
        std::fs::metadata(root.0.join("logs/server.log.1"))?.len(),
        ROTATE_BYTES + 5
    );
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_foreign_handle_denying_delete_defers_rotation_and_keeps_capturing() -> io::Result<()> {
    use std::os::windows::fs::OpenOptionsExt;
    let root = Scratch::new()?;
    drop(open(&root.0)?);
    let blocker = OpenOptions::new()
        .write(true)
        .share_mode(0x0000_0003)
        .open(path(&root.0))?;
    blocker.set_len(ROTATE_BYTES + 1)?;
    let mut file = open(&root.0)?;
    file.write_all(b"kept\n")?;
    assert_eq!(file.metadata()?.len(), ROTATE_BYTES + 6);
    assert!(!root.0.join("logs/server.log.1").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn symlink_log_destination_is_refused_without_writing_the_target() -> io::Result<()> {
    let root = Scratch::new()?;
    std::fs::create_dir(root.0.join("logs"))?;
    let target = root.0.join("unrelated");
    std::fs::write(&target, b"preserved")?;
    std::os::unix::fs::symlink(&target, path(&root.0))?;
    assert!(open(&root.0).is_err());
    assert_eq!(std::fs::read(target)?, b"preserved");
    Ok(())
}

#[cfg(unix)]
#[test]
fn new_logs_are_owner_only() -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let root = Scratch::new()?;
    let file = open(&root.0)?;
    assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
    assert_eq!(
        std::fs::metadata(root.0.join("logs"))?.permissions().mode() & 0o777,
        0o700
    );
    Ok(())
}

#[test]
fn refusal_names_the_windows_log_with_native_separators() {
    let profile = Path::new(r"\\?\C:\Users\example\my profile");
    assert_eq!(
        failure_for_platform(profile, "profile service.conf is missing", true),
        r"profile service.conf is missing; server log: C:\Users\example\my profile\logs\server.log"
    );
}

#[test]
fn refusal_names_the_native_log_only_when_it_exists() -> io::Result<()> {
    let root = Scratch::new()?;
    let profile = root.0.join("my profile");
    assert_eq!(
        failure(&profile, "no model matched"),
        "no model matched",
        "a refusal cited a log inside a profile that does not exist"
    );
    std::fs::create_dir(&profile)?;
    assert_eq!(
        failure(&profile, "profile service.conf is missing"),
        "profile service.conf is missing",
        "a refusal before the first launch cited a log that does not exist"
    );
    std::fs::create_dir(profile.join("logs"))?;
    assert_eq!(
        failure(&profile, "setup refused"),
        "setup refused",
        "an empty logs directory was mistaken for a log file"
    );
    drop(open(&profile)?);
    assert_eq!(
        failure(&profile, "product refused to start"),
        format!(
            "product refused to start; server log: {}",
            path(&profile).display()
        )
    );
    Ok(())
}
