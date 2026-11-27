#[path = "../../../test-support/evidence.rs"]
mod evidence;

use grapt_release::{fingerprint_file, inspect_status, rehearse};
use grapt_store::SystemClock;
use grapt_types::MigrationId;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
#[ignore = "requires the 406 MiB frozen evidence clone"]
fn frozen_clone_completes_migration_and_rollback_without_source_mutation() -> TestResult {
    let source = evidence::frozen_clone();
    let source_before = fingerprint_file(&source)?;
    let root = std::env::temp_dir().join(format!("grapt-r6mr-frozen-clone-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    let run_id = MigrationId::new("r6mr-frozen-clone").ok_or("migration id")?;
    let receipt = rehearse(&source, &root, run_id, "0.2.0", &SystemClock)?;
    assert_eq!(receipt.source_file_fingerprint(), &source_before.1);
    assert_eq!(
        receipt.rollback().domain_fingerprint(),
        receipt.source_domain_fingerprint()
    );
    assert_eq!(fingerprint_file(&source)?, source_before);
    let status = inspect_status(&root)?;
    assert!(status.ready_for_handoff);
    std::fs::remove_dir_all(root)?;
    Ok(())
}
