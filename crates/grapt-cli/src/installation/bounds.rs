//! One bounded tree budget for copying, permission checks and retirement.

// The payload permits 8192 files and at most twice as many total entries.
// Leave room for the Grapt binaries, receipts and plugin runtime as well.
pub(super) const TREE_ENTRIES: usize = 16_640;
pub(super) const TREE_DEPTH: usize = 20;
pub(super) const VERSION_ENTRIES: usize = TREE_ENTRIES * 64;
