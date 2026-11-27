//! Where a receipt is allowed to land.
//!
//! Carried from the corrected admission report's R3: a receipt root inside a
//! repository is refused. The reason is mechanical rather than tidy — the
//! direct-include inventory counts gitignored directories, so a file written
//! under a repository root while a battery runs reddens a gate on an otherwise
//! clean tree, and the run that produced the receipt is the run the receipt
//! then invalidates.
//!
//! Precedence is explicit argument, then `GRAPT_RECEIPT_ROOT`, then a
//! portable fallback outside any repository. The order is the law: an operator
//! who names a root on the command line has said something more specific than
//! an environment variable inherited from a shell three layers up, and the
//! environment must not outrank them.

use std::path::{Path, PathBuf};

/// Why a receipt root could not be settled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CustodyRefusal {
    /// The chosen root lies inside a repository working tree.
    ReceiptRootInsideRepository {
        /// The offending root, exactly as it was resolved.
        root: PathBuf,
    },
    /// No root was supplied and no portable fallback could be derived.
    NoPortableReceiptRoot,
    /// The settled root admitted the receipt but the write did not land.
    ///
    /// A separate variant rather than a silent `let _ = write`: a `serve` whose
    /// ready receipt never reached the disk has no durable evidence that it was
    /// ever up, and a supervisor polling for that artifact would wait forever
    /// while the daemon ran happily. Refusing before the host is composed is
    /// the honest answer.
    ReadyReceiptNotWritten {
        /// Where the receipt was going.
        path: PathBuf,
        /// What the filesystem said, as text — an `io::Error` is not `PartialEq`
        /// and this enum is compared by witnesses.
        reason: String,
    },
}

/// The name the ready receipt always takes under the settled root.
///
/// Fixed rather than derived from a bound or a timestamp: the artifact exists
/// so another process can find it without being told, and a name that varied
/// per run would make "poll for the ready receipt" impossible to write.
pub const READY_RECEIPT_FILE: &str = "serve-ready.json";

/// A repository marker. A directory containing one is a working tree root.
const REPOSITORY_MARKER: &str = ".git";

/// Whether `candidate` is inside a repository working tree.
///
/// Walks upward, because a receipt written three directories below a
/// repository root is still inside that repository. Checking only the
/// directory itself is the version of this rule that passes review and then
/// fails in the field.
pub fn is_inside_repository(candidate: &Path) -> bool {
    let mut cursor = Some(candidate);
    while let Some(directory) = cursor {
        if directory.join(REPOSITORY_MARKER).exists() {
            return true;
        }
        cursor = directory.parent();
    }
    false
}

/// Settle the receipt root, in precedence order, refusing an in-repository one.
///
/// `explicit` is the `--receipt-root` argument when the operator supplied one.
/// `from_env` is read from the process environment by the caller and passed in,
/// so this function stays a pure decision over two options and a fallback and
/// can be witnessed without mutating the environment of a running test binary.
pub fn receipt_root(
    explicit: Option<PathBuf>,
    from_env: Option<PathBuf>,
    portable_fallback: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, CustodyRefusal> {
    let root = explicit.or(from_env).or_else(portable_fallback);
    let root = match root {
        Some(root) => root,
        None => return Err(CustodyRefusal::NoPortableReceiptRoot),
    };
    if is_inside_repository(&root) {
        return Err(CustodyRefusal::ReceiptRootInsideRepository { root });
    }
    Ok(root)
}

/// The portable fallback: the platform temporary directory, never the
/// current working directory.
///
/// The working directory during a governed run is the candidate tree, which is
/// precisely the place [`receipt_root`] exists to keep receipts out of.
pub fn portable_receipt_root() -> Option<PathBuf> {
    let base = std::env::temp_dir();
    if base.as_os_str().is_empty() {
        return None;
    }
    Some(base.join("grapt-engined-receipts"))
}

/// Write the ready receipt under the already-settled root.
///
/// The root arrives settled — [`receipt_root`] has already refused an
/// in-repository one — so this function creates the directory and writes the
/// bytes and decides nothing about where they may land. Keeping the decision
/// and the effect apart is what lets the precedence law be witnessed without a
/// filesystem and this effect be witnessed without an environment.
pub fn persist_ready_receipt(root: &Path, json: &str) -> Result<PathBuf, CustodyRefusal> {
    let path = root.join(READY_RECEIPT_FILE);
    if let Err(error) = std::fs::create_dir_all(root) {
        return Err(CustodyRefusal::ReadyReceiptNotWritten {
            path,
            reason: error.to_string(),
        });
    }
    match std::fs::write(&path, json) {
        Ok(()) => Ok(path),
        Err(error) => Err(CustodyRefusal::ReadyReceiptNotWritten {
            path,
            reason: error.to_string(),
        }),
    }
}

impl CustodyRefusal {
    /// The operator-facing refusal text.
    pub fn refusal_text(&self) -> String {
        match self {
            CustodyRefusal::ReceiptRootInsideRepository { root } => format!(
                "receipt root {} lies inside a repository working tree; a receipt \
                 written into a candidate tree voids the run that produced it",
                root.display()
            ),
            CustodyRefusal::NoPortableReceiptRoot => {
                "no receipt root was supplied and no portable fallback could be derived".to_owned()
            }
            CustodyRefusal::ReadyReceiptNotWritten { path, reason } => format!(
                "the ready receipt could not be written to {}: {reason}; a serve with no \
                 durable ready receipt leaves no evidence that it was ever up",
                path.display()
            ),
        }
    }
}
