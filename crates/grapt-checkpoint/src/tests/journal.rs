use crate::journal::{
    read_hard_limited, validate_observed_length, ApplyingRestore, BeginDisposition,
    JournalDurability, RestoreJournal, RootKind, StoredRestore, TerminalRestore,
};
use crate::{JournalError, ReconciledState, MAX_JOURNAL_BYTES};
use std::fs::OpenOptions;
use std::io::{Cursor, Write};
use std::path::Path;
use std::process::Command;

use super::support::TestRoot;

fn applying() -> ApplyingRestore {
    ApplyingRestore {
        operation: "restore-journal".to_owned(),
        plan: "plan-journal".to_owned(),
        provider: "fixture-provider".to_owned(),
        version: "1".to_owned(),
        checkpoint: "checkpoint-1".to_owned(),
        root_kind: RootKind::Plain,
        root: "C:/disposable/journal".to_owned(),
        common_dir: None,
        git_dir: None,
        lanes: vec!["files".to_owned()],
        permission: "per_checkpoint".to_owned(),
        admitted_at_ms: 100,
        writes: 2,
        deletes: 1,
        bytes: 64,
    }
}

#[test]
fn append_only_journal_refuses_duplicate_begin_and_preserves_terminal(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("journal-terminal")?;
    let mut journal = root.journal()?;
    let applying = applying();
    assert_eq!(journal.begin(&applying)?, BeginDisposition::Begun);
    assert_eq!(journal.begin(&applying)?, BeginDisposition::AlreadyExists);
    assert!(matches!(
        journal.read(&applying.operation)?,
        Some(StoredRestore::Applying(ref read)) if read == &applying
    ));

    journal.settle(
        &applying.operation,
        &TerminalRestore {
            state: ReconciledState::Applied,
        },
    )?;
    assert!(matches!(
        journal.read(&applying.operation)?,
        Some(StoredRestore::Terminal { terminal, .. })
            if terminal.state == ReconciledState::Applied
    ));
    assert!(matches!(
        journal.settle(
            &applying.operation,
            &TerminalRestore {
                state: ReconciledState::Quarantined,
            }
        ),
        Err(JournalError::Conflict)
    ));
    Ok(())
}

#[test]
fn malformed_or_over_bound_journal_evidence_fails_loudly() -> Result<(), Box<dyn std::error::Error>>
{
    let root = TestRoot::new("journal-corrupt")?;
    let mut journal = root.journal()?;
    let record = applying();
    journal.begin(&record)?;
    let mut file = OpenOptions::new()
        .append(true)
        .open(journal.path(&record.operation))?;
    file.write_all(b"\n{not-json")?;
    file.sync_all()?;
    assert!(matches!(
        journal.read(&record.operation),
        Err(JournalError::Corrupt)
    ));

    let other_root = TestRoot::new("journal-over-bound")?;
    let mut other = other_root.journal()?;
    let mut invalid = applying();
    invalid.lanes.clear();
    assert!(matches!(
        other.begin(&invalid),
        Err(JournalError::OverBound)
    ));
    let mut json_entries = 0;
    for entry in std::fs::read_dir(other_root.path().join("journal"))? {
        let entry = entry?;
        if entry
            .path()
            .extension()
            .is_some_and(|value| value == "json")
        {
            json_entries += 1;
        }
    }
    assert_eq!(json_entries, 0);
    Ok(())
}

#[test]
fn a_growing_journal_is_hard_limited_and_rechecked() -> Result<(), Box<dyn std::error::Error>> {
    let mut growing = Cursor::new(vec![b'x'; MAX_JOURNAL_BYTES + 1]);
    assert!(matches!(
        read_hard_limited(&mut growing, MAX_JOURNAL_BYTES as u64),
        Err(JournalError::OverBound)
    ));
    assert!(matches!(
        validate_observed_length(b"stable", 7),
        Err(JournalError::Conflict)
    ));
    Ok(())
}

#[derive(Debug)]
struct RefusingDurability;

impl JournalDurability for RefusingDurability {
    fn sync_parent(&self, _root: &Path) -> Result<(), JournalError> {
        Err(JournalError::Unavailable)
    }
}

#[test]
fn parent_directory_sync_is_part_of_begin_before_effect_admission(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("journal-parent-sync")?;
    std::fs::create_dir_all(root.path().join("journal"))?;
    let mut journal = crate::FileRestoreJournal::open_with_durability(
        root.path().join("journal"),
        Box::new(RefusingDurability),
    )?;
    assert!(matches!(
        journal.begin(&applying()),
        Err(JournalError::Unavailable)
    ));
    Ok(())
}

#[test]
fn journal_root_must_be_preprovisioned_before_the_owner_opens(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("journal-root-provisioning")?;
    assert!(matches!(
        crate::FileRestoreJournal::open(root.path().join("absent-journal")),
        Err(JournalError::RootNotProvisioned)
    ));
    Ok(())
}

#[test]
fn one_durable_owner_excludes_competing_settlement() -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("journal-owner")?;
    let journal_root = root.path().join("journal");
    std::fs::create_dir_all(&journal_root)?;
    let mut owner = crate::FileRestoreJournal::open(&journal_root)?;
    owner.begin(&applying())?;
    assert!(matches!(
        crate::FileRestoreJournal::open(&journal_root),
        Err(JournalError::Busy)
    ));
    owner.settle(
        &applying().operation,
        &TerminalRestore {
            state: ReconciledState::Applied,
        },
    )?;
    drop(owner);
    let successor = crate::FileRestoreJournal::open(&journal_root)?;
    assert!(matches!(
        successor.read(&applying().operation)?,
        Some(StoredRestore::Terminal { .. })
    ));
    Ok(())
}

#[test]
fn applying_record_survives_process_crash_and_releases_owner(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("journal-process-crash")?;
    let journal_root = root.path().join("journal");
    std::fs::create_dir_all(&journal_root)?;
    let status = Command::new(std::env::current_exe()?)
        .arg("--exact")
        .arg("tests::journal::crash_writer_process")
        .arg("--ignored")
        .env("GRAPT_CHECKPOINT_CRASH_ROOT", &journal_root)
        .status()?;
    assert!(!status.success(), "crash witness child returned normally");

    let successor = crate::FileRestoreJournal::open(journal_root)?;
    assert!(matches!(
        successor.read(&applying().operation)?,
        Some(StoredRestore::Applying(_))
    ));
    Ok(())
}

#[test]
#[ignore = "subprocess crash-writer helper"]
fn crash_writer_process() -> Result<(), Box<dyn std::error::Error>> {
    let journal_root =
        std::env::var_os("GRAPT_CHECKPOINT_CRASH_ROOT").ok_or("crash witness root is absent")?;
    let mut journal = crate::FileRestoreJournal::open(journal_root)?;
    journal.begin(&applying())?;
    std::process::abort();
}

#[test]
fn journal_path_cannot_substitute_another_operation_record(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = TestRoot::new("journal-operation-substitution")?;
    let mut journal = root.journal()?;
    let record = applying();
    journal.begin(&record)?;
    let substituted_operation = "restore-substituted";
    std::fs::rename(
        journal.path(&record.operation),
        journal.path(substituted_operation),
    )?;
    assert!(matches!(
        journal.read(substituted_operation),
        Err(JournalError::Corrupt)
    ));
    Ok(())
}
