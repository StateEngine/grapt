//! Test recovery journals (SC-MAC-02b): a fixed clock and counting ids, in a
//! directory beside the runtime's private root and cleaned up with it.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use grapt_plugin::RecoveryJournal;
use grapt_types::{ClockAuthority, IdAuthority, IdToken, Timestamp};

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_788_220_800_000)
    }
}

/// Unique in the process, so two supervisors never share a launch nonce.
static NEXT: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct CountingIds;

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let next = NEXT.fetch_add(1, Ordering::Relaxed);
        IdToken::new(format!("t{}n{next:08}", std::process::id()))
            .unwrap_or_else(|| panic!("a non-empty token was refused"))
    }
}

pub fn recovery_root(private_root: &Path) -> PathBuf {
    private_root.with_extension("recovery")
}

pub fn recovery(private_root: &Path) -> RecoveryJournal {
    RecoveryJournal {
        root: recovery_root(private_root),
        clock: Arc::new(FixedClock),
        ids: Arc::new(CountingIds),
    }
}
