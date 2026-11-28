use super::*;

#[test]
fn plaintext_input_remains_bounded_and_an_envelope_never_falls_back() -> io::Result<()> {
    assert_eq!(open(b"legacy-token", 12)?, b"legacy-token");
    assert!(open(b"legacy-token", 11).is_err());
    for format in [DPAPI_ENVELOPE, KEYSTORE_ENVELOPE] {
        let mut damaged = format.to_vec();
        damaged.extend_from_slice(b"not a sealed blob");
        assert!(open(&damaged, 4096).is_err());
    }
    Ok(())
}

#[test]
fn protected_storage_round_trips_binary_input_with_independent_plaintext_bound() -> io::Result<()> {
    let secret = b"fixture-access\0fixture-refresh\xff";
    let stored = seal(secret)?;
    assert!(stored.len() <= stored_bound(secret.len()));
    assert_eq!(open(&stored, secret.len())?, secret);
    assert!(open(&stored, secret.len() - 1).is_err());
    #[cfg(windows)]
    {
        assert!(is_protected(&stored));
        assert!(!stored.windows(14).any(|bytes| bytes == b"fixture-access"));
        let mut corrupt = stored;
        if let Some(last) = corrupt.last_mut() {
            *last ^= 1;
        }
        assert!(open(&corrupt, 4096).is_err());
    }
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_dpapi_credential_opens_and_is_resealed_under_the_keystore() -> io::Result<()> {
    let secret = b"fixture-dpapi-secret";
    let legacy = fixtures::dpapi_sealed(secret)?;
    assert!(is_protected(&legacy));
    let opened = open(&legacy, secret.len())?;
    assert_eq!(opened, secret);
    let resealed = seal(&opened)?;
    assert!(resealed.starts_with(KEYSTORE_ENVELOPE));
    assert_eq!(open(&resealed, secret.len())?, secret);
    Ok(())
}

#[cfg(windows)]
#[test]
fn a_credential_whose_key_is_gone_names_that_reason() -> io::Result<()> {
    let orphan = fixtures::sealed_with_missing_key(b"fixture-orphan")?;
    let error = open(&orphan, 4096)
        .err()
        .ok_or_else(|| io::Error::other("opened"))?;
    assert!(is_key_missing(&error));
    assert_eq!(error.to_string(), KEY_MISSING);
    Ok(())
}

static NEXT_SCRATCH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct ScratchDir(std::path::PathBuf);

impl ScratchDir {
    fn new(name: &str) -> io::Result<Self> {
        Self::new_in(&std::env::temp_dir(), name)
    }

    fn new_in(base: &std::path::Path, name: &str) -> io::Result<Self> {
        let pid = std::process::id();
        loop {
            let index = NEXT_SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = base.join(format!("grapt-{name}-{pid}-{index}"));
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
fn scratch_dir_never_deletes_preexisting_directory() -> io::Result<()> {
    let parent = ScratchDir::new("login-scratch-parent")?;
    let next = NEXT_SCRATCH.load(std::sync::atomic::Ordering::Relaxed);
    let preexisting = parent
        .0
        .join(format!("grapt-sentinel-{}-{next}", std::process::id()));
    std::fs::create_dir(&preexisting)?;
    let sentinel = preexisting.join("keep.txt");
    std::fs::write(&sentinel, b"untouched")?;

    let claimed = ScratchDir::new_in(&parent.0, "sentinel")?;
    assert_ne!(claimed.0, preexisting);
    assert_eq!(std::fs::read(&sentinel)?, b"untouched");
    let claimed_path = claimed.0.clone();
    drop(claimed);
    assert!(!claimed_path.exists());
    assert_eq!(std::fs::read(&sentinel)?, b"untouched");
    Ok(())
}

#[test]
fn only_real_directories_can_be_protected() -> io::Result<()> {
    let root = ScratchDir::new("protection-fixture")?;
    protect_directory(&root.0)?;
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .open(root.0.join("renewal.lock"))?;
    protect_directory(&root.0)?;
    verify_directory(&root.0)?;
    drop(lock);
    let file = root.0.join("not-a-directory");
    std::fs::write(&file, b"fixture")?;
    assert!(protect_directory(&file).is_err());
    assert!(verify_directory(&file).is_err());
    Ok(())
}

#[test]
fn newly_created_private_record_gets_strict_user_authority() -> io::Result<()> {
    let root = ScratchDir::new("new-record")?;
    protect_directory(&root.0)?;
    let path = root.0.join("record.new");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let file = options.open(&path)?;
    protect_new_private_file(&path, &file)?;
    verify_private_file(&path, &file)?;
    let other = root.0.join("other.new");
    std::fs::write(&other, b"unrelated")?;
    assert!(protect_new_private_file(&other, &file).is_err());
    Ok(())
}

#[test]
fn child_failures_report_only_known_non_secret_codes() {
    for code in [
        "request",
        "seal",
        "open",
        "type",
        "owner",
        "foreign-write",
        "verification",
        "acl-read",
        "acl-write",
        "operation",
    ] {
        let error = child_refusal(format!("error:{code}").as_bytes());
        assert_eq!(failure_code(&error), code);
    }
    for output in [
        b"error:secret-sentinel".as_slice(),
        b"error:acl-write\nsecret-sentinel",
        b"",
    ] {
        let error = child_refusal(output);
        assert_eq!(failure_code(&error), "child-failed");
        assert!(!error.to_string().contains("secret-sentinel"));
    }
}

#[test]
fn a_private_lock_is_made_exclusively_and_reopened_only_empty_and_unlinked() -> io::Result<()> {
    let root = ScratchDir::new("private-lock")?;
    protect_directory(&root.0)?;
    let path = root.0.join("fixture.lock");
    let created = open_private_lock(&path).map_err(io::Error::from)?;
    created
        .try_lock()
        .map_err(|_| io::Error::other("fresh lock was not lockable"))?;
    drop(created);
    let reopened = open_private_lock(&path).map_err(io::Error::from)?;
    drop(reopened);
    std::fs::write(&path, b"not a lock")?;
    assert!(matches!(
        open_private_lock(&path),
        Err(LockFailure::NotEmpty)
    ));
    assert_eq!(
        std::fs::read(&path)?,
        b"not a lock",
        "a refused lock was changed"
    );
    #[cfg(unix)]
    {
        let target = root.0.join("elsewhere.lock");
        std::fs::write(&target, b"")?;
        let linked = root.0.join("linked.lock");
        std::os::unix::fs::symlink(&target, &linked)?;
        assert!(matches!(
            open_private_lock(&linked),
            Err(LockFailure::Open(_))
        ));
    }
    Ok(())
}
