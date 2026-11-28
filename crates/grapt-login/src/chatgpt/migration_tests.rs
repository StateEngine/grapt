use super::*;

#[cfg(windows)]
#[tokio::test]
async fn direct_startup_finishes_interrupted_protection_before_another_generation(
) -> Result<(), Box<dyn std::error::Error>> {
    use super::super::{Session, Timestamp, Tokens};
    #[derive(Debug)]
    struct Fixed;
    impl grapt_types::ClockAuthority for Fixed {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(1_800_000_000_000)
        }
    }
    let profile = Profile::new("startup-interrupted-protection")?;
    crate::profile_security::protect_directory(&profile.0)?;
    let original = Tokens {
        access: "original-access".into(),
        refresh: "original-refresh".into(),
        account: "fixture-account".into(),
        expires_at: Timestamp::from_millis(8_000_000_000_000),
    };
    std::fs::write(profile.0.join(NAMES[0]), original.access.as_bytes())?;
    std::fs::write(profile.0.join(NAMES[1]), super::super::encode(&original))?;
    // Files as an earlier release left them: sealed by Windows DPAPI.
    let state = crate::profile_security::fixtures::dpapi_sealed(&super::super::encode(&original))?;
    let credential = crate::profile_security::fixtures::dpapi_sealed(original.access.as_bytes())?;
    let interrupted = std::panic::catch_unwind(|| {
        publish(&profile.0, [&credential, &state], |count| {
            assert_ne!(count, 1, "injected process interruption");
            Ok(())
        })
    });
    assert!(interrupted.is_err());
    let mut session = Session::open(&profile.0, &original.account)?.ok_or("missing sign-in")?;
    session
        .prepare_startup(
            &super::super::production_client()?,
            super::super::ISSUER,
            &Fixed,
        )
        .await?;
    assert!(
        !profile.0.join(JOURNAL).exists(),
        "startup retained rollback originals before renewal"
    );
    for name in NAMES {
        assert!(
            crate::profile_security::is_protected(&std::fs::read(profile.0.join(name))?),
            "startup left a plaintext sign-in file"
        );
    }
    let mut rotated = original;
    rotated.access = "new-generation-access".into();
    rotated.refresh = "new-generation-refresh".into();
    super::super::save(&profile.0, &rotated)?;
    recover(&profile.0)?;
    assert_eq!(super::super::load(&profile.0)?, Some(rotated));
    Ok(())
}
struct Profile(std::path::PathBuf);
impl Profile {
    fn new(case: &str) -> io::Result<Self> {
        let path =
            std::env::temp_dir().join(format!("grapt-upgrade-{case}-{}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn legacy(case: &str) -> io::Result<Profile> {
    let profile = Profile::new(case)?;
    crate::profile_security::protect_directory(&profile.0)?;
    write_new(&profile.0.join(NAMES[0]), b"original-access")?;
    write_new(&profile.0.join(NAMES[1]), b"original-oauth-json")?;
    Ok(profile)
}

fn assert_legacy(profile: &Profile) -> io::Result<()> {
    assert_eq!(
        read(&profile.0.join(NAMES[0]))?,
        Some(b"original-access".to_vec())
    );
    assert_eq!(
        read(&profile.0.join(NAMES[1]))?,
        Some(b"original-oauth-json".to_vec())
    );
    Ok(())
}

#[test]
fn both_files_are_restored_after_either_publication_fails() -> io::Result<()> {
    for cut in [0, 1, 2] {
        let profile = legacy(&format!("migration-returned-failure-{cut}"))?;
        let result = publish(
            &profile.0,
            [b"sealed-access", b"sealed-state"],
            |published| {
                if published == cut {
                    Err(io::Error::other("injected publication failure"))
                } else {
                    Ok(())
                }
            },
        );
        assert!(result.is_err());
        assert_legacy(&profile)?;
        assert!(!profile.0.join(JOURNAL).exists());
        recover(&profile.0)?;
        assert_legacy(&profile)?;
    }
    Ok(())
}

#[test]
fn interrupted_publication_recovers_the_original_pair_before_reuse() -> io::Result<()> {
    for cut in [0, 1, 2] {
        let profile = legacy(&format!("migration-interrupted-{cut}"))?;
        let interrupted = std::panic::catch_unwind(|| {
            let _ = publish(
                &profile.0,
                [b"sealed-access", b"sealed-state"],
                |published| {
                    assert_ne!(published, cut, "injected process interruption");
                    Ok(())
                },
            );
        });
        assert!(interrupted.is_err());
        assert!(profile.0.join(JOURNAL).join("ready").exists());
        recover(&profile.0)?;
        assert_legacy(&profile)?;
        assert!(!profile.0.join(JOURNAL).exists());
        // Recovery is repeatable and cannot resurrect the interrupted pair.
        recover(&profile.0)?;
        assert_legacy(&profile)?;
    }
    Ok(())
}

#[test]
fn committed_cleanup_never_restores_an_old_generation() -> io::Result<()> {
    let profile = legacy("migration-committed-cleanup")?;
    let journal = profile.0.join(JOURNAL);
    std::fs::create_dir(&journal)?;
    crate::profile_security::protect_directory(&journal)?;
    write_new(&journal.join("1.original"), b"stale-state")?;
    write_new(&journal.join("ready"), MARKER)?;
    commit(&journal)?;
    // One original has already been deleted: this is partial committed cleanup.
    recover(&profile.0)?;
    assert_legacy(&profile)?;
    assert!(!journal.exists());
    Ok(())
}

#[test]
fn unprepared_slots_are_removed_but_unknown_or_malformed_slots_are_preserved() -> io::Result<()> {
    let profile = legacy("migration-partial-preparation")?;
    let journal = profile.0.join(JOURNAL);
    std::fs::create_dir(&journal)?;
    crate::profile_security::protect_directory(&journal)?;
    write_new(&journal.join("0.original"), b"original-access")?;
    write_new(&journal.join("ready.new"), b"interrupted marker write")?;
    recover(&profile.0)?;
    assert_legacy(&profile)?;
    assert!(!journal.exists());

    std::fs::create_dir(&journal)?;
    crate::profile_security::protect_directory(&journal)?;
    write_new(&journal.join("unrelated"), b"preserve this file")?;
    assert!(recover(&profile.0).is_err());
    assert_eq!(
        read(&journal.join("unrelated"))?,
        Some(b"preserve this file".to_vec())
    );
    std::fs::remove_file(journal.join("unrelated"))?;
    write_new(&journal.join("ready"), b"malformed")?;
    assert!(recover(&profile.0).is_err());
    assert_legacy(&profile)?;
    Ok(())
}

#[test]
fn a_failed_preparation_changes_no_profile_content() -> io::Result<()> {
    let profile = legacy("migration-preparation-failure")?;
    // An oversized second original is refused before either publication.
    std::fs::write(profile.0.join(NAMES[1]), vec![b'x'; 18 * 1024 + 1])?;
    assert!(publish(&profile.0, [b"sealed-access", b"sealed-state"], |_| Ok(())).is_err());
    assert_eq!(
        read(&profile.0.join(NAMES[0]))?,
        Some(b"original-access".to_vec())
    );
    assert_eq!(
        std::fs::read(profile.0.join(NAMES[1]))?,
        vec![b'x'; 18 * 1024 + 1]
    );
    assert!(!profile.0.join(JOURNAL).exists());
    Ok(())
}

#[test]
fn successful_format_migration_preserves_the_sign_in_generation() -> io::Result<()> {
    let profile = Profile::new("migration-complete-generation")?;
    let tokens = super::super::Tokens {
        access: "fixture-access".to_owned(),
        refresh: "fixture-refresh".to_owned(),
        expires_at: super::super::Timestamp::from_millis(1_800_000_000_000),
        account: "fixture-account".to_owned(),
    };
    std::fs::write(
        profile.0.join(super::super::STATE_FILE),
        super::super::encode(&tokens),
    )?;
    std::fs::write(
        profile.0.join(super::super::CREDENTIAL_FILE),
        tokens.access.as_bytes(),
    )?;
    migrate(&profile.0, &tokens)?;
    let recovered = super::super::load(&profile.0)
        .map_err(|_| io::Error::other("migrated generation unreadable"))?
        .ok_or_else(|| io::Error::other("migrated generation absent"))?;
    assert_eq!(recovered.access, tokens.access);
    assert_eq!(recovered.refresh, tokens.refresh);
    for name in NAMES {
        let bytes = read(&profile.0.join(name))?.ok_or_else(invalid)?;
        #[cfg(windows)]
        assert!(crate::profile_security::is_protected(&bytes));
        #[cfg(not(windows))]
        assert!(!bytes.is_empty());
    }
    assert!(!profile.0.join(JOURNAL).exists());
    Ok(())
}

#[test]
fn a_failed_upgrade_retains_legacy_json_and_the_new_reader_can_retry() -> io::Result<()> {
    for cut in [0, 1, 2] {
        let profile = Profile::new(&format!("migration-legacy-readable-{cut}"))?;
        crate::profile_security::protect_directory(&profile.0)?;
        let tokens = super::super::Tokens {
            access: "fixture-access".to_owned(),
            refresh: "fixture-refresh".to_owned(),
            expires_at: super::super::Timestamp::from_millis(1_800_000_000_000),
            account: "fixture-account".to_owned(),
        };
        let original = super::super::encode(&tokens);
        write_new(&profile.0.join(NAMES[0]), tokens.access.as_bytes())?;
        write_new(&profile.0.join(NAMES[1]), &original)?;
        assert!(publish(
            &profile.0,
            [b"sealed-access", b"sealed-state"],
            |published| {
                if published == cut {
                    Err(io::Error::other("injected migration failure"))
                } else {
                    Ok(())
                }
            }
        )
        .is_err());
        let state = read(&profile.0.join(NAMES[1]))?.ok_or_else(invalid)?;
        assert_eq!(state, original);
        // The unchanged legacy decoder accepts these exact on-disk bytes.
        let old = super::super::decode(&state).map_err(|_| invalid())?;
        assert_eq!(old.refresh, tokens.refresh);
        assert_eq!(
            read(&profile.0.join(NAMES[0]))?,
            Some(tokens.access.as_bytes().to_vec())
        );
        migrate(&profile.0, &tokens)?;
        let new = super::super::load(&profile.0)
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        assert_eq!(new.refresh, tokens.refresh);
    }
    Ok(())
}

#[test]
fn process_crashes_leave_a_recoverable_private_original_pair() -> io::Result<()> {
    for cut in [0, 1, 2] {
        let profile = legacy(&format!("migration-crash-{cut}"))?;
        let status = std::process::Command::new(std::env::current_exe()?)
            .args([
                "--exact",
                "chatgpt::migration::tests::migration_process_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("GRAPT_MIGRATION_FIXTURE_ROOT", &profile.0)
            .env("GRAPT_MIGRATION_FIXTURE_CUT", cut.to_string())
            .status()?;
        assert_eq!(
            status.code(),
            Some(63),
            "the named migration crash did not execute"
        );
        recover(&profile.0)?;
        assert_legacy(&profile)?;
        recover(&profile.0)?;
        assert_legacy(&profile)?;
    }
    Ok(())
}

#[test]
#[ignore = "migration subprocess fixture; parent supplies its private profile and cut"]
fn migration_process_fixture() {
    let profile = std::env::var_os("GRAPT_MIGRATION_FIXTURE_ROOT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| panic!("missing migration fixture root"));
    let cut = std::env::var("GRAPT_MIGRATION_FIXTURE_CUT")
        .unwrap_or_else(|_| panic!("missing migration fixture cut"));
    let cut: usize = cut
        .parse()
        .unwrap_or_else(|_| panic!("invalid migration fixture cut"));
    assert!([0, 1, 2].contains(&cut), "invalid migration fixture cut");
    publish(&profile, [b"sealed-access", b"sealed-state"], |published| {
        if published == cut {
            std::process::exit(63);
        }
        Ok(())
    })
    .unwrap_or_else(|error| panic!("migration fixture: {error}"));
    panic!("migration fixture missed its crash cut");
}

#[cfg(windows)]
#[test]
fn a_foreign_writable_parent_cannot_cause_a_partially_migrated_profile() -> io::Result<()> {
    let parent = Profile::new("migration-foreign-parent")?;
    foreign_writable_parent(&parent.0)?;
    let profile = parent.0.join("signed-in-profile");
    std::fs::create_dir(&profile)?;
    let tokens = super::super::Tokens {
        access: "fixture-access".to_owned(),
        refresh: "fixture-refresh".to_owned(),
        expires_at: super::super::Timestamp::from_millis(1_800_000_000_000),
        account: "fixture-account".to_owned(),
    };
    let original = super::super::encode(&tokens);
    write_new(&profile.join(NAMES[0]), tokens.access.as_bytes())?;
    write_new(&profile.join(NAMES[1]), &original)?;
    match migrate(&profile, &tokens) {
        Ok(()) => {
            for name in NAMES {
                assert!(crate::profile_security::is_protected(
                    &read(&profile.join(name))?.ok_or_else(invalid)?
                ));
            }
            crate::profile_security::verify_directory(&profile)?;
            let current = super::super::load(&profile)
                .map_err(|_| invalid())?
                .ok_or_else(invalid)?;
            assert_eq!(current.refresh, tokens.refresh);
        }
        Err(_) => {
            assert_eq!(
                read(&profile.join(NAMES[0]))?,
                Some(tokens.access.as_bytes().to_vec())
            );
            assert_eq!(read(&profile.join(NAMES[1]))?, Some(original));
        }
    }
    Ok(())
}

#[cfg(windows)]
fn foreign_writable_parent(path: &Path) -> io::Result<()> {
    use std::process::{Command, Stdio};
    const SCRIPT: &str = r#"$ErrorActionPreference='Stop'
$PSModuleAutoLoadingPreference='None'
try {
    $path=[Console]::In.ReadToEnd()
    Import-Module -Name "$PSHOME\Modules\Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1" -ErrorAction Stop
    Import-Module -Name "$PSHOME\Modules\Microsoft.PowerShell.Management\Microsoft.PowerShell.Management.psd1" -ErrorAction Stop
    Import-Module -Name "$PSHOME\Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1" -ErrorAction Stop
    $acl=Get-Acl -LiteralPath $path
    $au=New-Object Security.Principal.SecurityIdentifier('S-1-5-11')
    $acl.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($au,'Modify','ContainerInherit,ObjectInherit','None','Allow')))
    Set-Acl -LiteralPath $path -AclObject $acl
    $rules=@((Get-Acl -LiteralPath $path).GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]))
    $modify=[Security.AccessControl.FileSystemRights]::Modify
    $inheritance=[Security.AccessControl.InheritanceFlags]'ContainerInherit,ObjectInherit'
    $matching=@($rules | Where-Object {
        $_.IdentityReference.Value -eq 'S-1-5-11' -and $_.AccessControlType -eq 'Allow' -and
        ($_.FileSystemRights -band $modify) -eq $modify -and
        $_.InheritanceFlags -eq $inheritance -and $_.PropagationFlags -eq 'None' -and -not $_.IsInherited
    })
    if($matching.Count -eq 0){throw 'fixture-parent-acl'}
} catch { exit 1 }
"#;
    let system = std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| io::Error::other("SystemRoot absent for native fixture"))?;
    let mut child = Command::new(system.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"))
        .env_clear()
        .env("SystemRoot", &system)
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            SCRIPT,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    let mut input = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("fixture stdin absent"))?;
    let path = path
        .to_str()
        .filter(|path| path.len() <= 4096)
        .ok_or_else(invalid)?;
    if let Err(error) = input.write_all(path.as_bytes()) {
        drop(input);
        let _ = child.kill();
        let _ = child.wait();
        return Err(error);
    }
    drop(input);
    let status = (|| {
        for _ in 0..750 {
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "native fixture parent ACL timed out",
        ))
    })();
    if status.is_err() {
        let _ = child.kill();
        let _ = child.wait();
    }
    assert!(
        status?.success(),
        "native fixture did not establish its foreign-write parent"
    );
    Ok(())
}
