//! The page moves a reader can ask the bounded history for.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryMove {
    Older,
    Newer,
    Beginning,
    Latest,
    /// Undo the most recent page movement without issuing another read.
    Cancel,
}
