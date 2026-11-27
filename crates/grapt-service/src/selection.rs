//! Which engine the retained desktop is attached to.
//!
//! The frozen desktop persists a bare URL string under `defaultServerUrl` and
//! spawns its oracle sidecar when that key is unset. That string is the
//! whole of its selection state: there is no engine identity in it, and nothing
//! in the frozen tree records *which* engine answered. This enum is the typed
//! replacement for that absence, and it is closed on purpose — a third engine
//! is a reviewed law change, not a new string.

/// The engine a selection names.
///
/// `Retained` is the frozen oracle engine the desktop spawns for itself.
/// `Rust` is an engine reached at an operator-supplied endpoint. Nothing here
/// asserts either is running; that is what a [`crate::SelectionReceipt`] is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EngineSelection {
    /// The retained TypeScript engine, spawned as the desktop's own sidecar.
    Retained,
    /// The Rust engine, reached at a supplied endpoint.
    Rust,
}

impl EngineSelection {
    /// The stable label used in the durable representation and the corpus.
    ///
    /// Fixed rather than derived from the variant name so that renaming a
    /// variant cannot silently rewrite already-recorded durable rows.
    pub fn label(self) -> &'static str {
        match self {
            EngineSelection::Retained => "retained",
            EngineSelection::Rust => "rust",
        }
    }

    /// The inverse of [`label`](Self::label). Unknown text is refused rather
    /// than defaulted: a durable row naming an engine this build does not know
    /// is a fact we cannot interpret, and guessing would attribute a run to the
    /// wrong engine.
    pub fn from_label(label: &str) -> Option<Self> {
        match label {
            "retained" => Some(EngineSelection::Retained),
            "rust" => Some(EngineSelection::Rust),
            _ => None,
        }
    }

    /// Every admitted selection.
    ///
    /// Declaration order only — it is **not** the order the corpus records
    /// them, and nothing should read it that way. `actions.jsonl` records `rust`
    /// at ordinal 1 and `retained` at ordinal 2; this array exists so a test can
    /// enumerate the closed set, not to describe the fixture.
    pub const ALL: [EngineSelection; 2] = [EngineSelection::Retained, EngineSelection::Rust];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_round_trips_and_the_set_is_closed() {
        for selection in EngineSelection::ALL {
            assert_eq!(
                EngineSelection::from_label(selection.label()),
                Some(selection),
                "a label must name the selection it came from"
            );
        }
        assert_eq!(
            EngineSelection::ALL.len(),
            2,
            "adding an engine is a reviewed law change, not a new label"
        );
    }
}
