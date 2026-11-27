//! L1: the closed vocabulary pinned against an INDEPENDENT in-tree fixture.
//!
//! `tests/fixtures/durable-kinds.tsv` was authored from the oracle manifest
//! enumeration and the frozen-clone census, not generated from the enum, so
//! the two can disagree — and this test is what makes them agree in both
//! directions: every fixture row resolves to exactly one `EventKind`, every
//! `EventKind` appears in exactly one row, and each row's provenance label
//! matches the enum's.

use grapt_event::{EventKind, KindProvenance};

const FIXTURE: &str = include_str!("fixtures/durable-kinds.tsv");

fn provenance_label(provenance: KindProvenance) -> &'static str {
    match provenance {
        KindProvenance::Manifest => "manifest",
        KindProvenance::HistoricalLogOnly => "historical-log-only",
        KindProvenance::HistoricalDeleted => "historical-deleted",
        KindProvenance::RustNative => "rust-native",
    }
}

#[test]
fn the_vocabulary_matches_the_independent_fixture_in_both_directions() {
    let mut rows: Vec<(String, u32, String)> = Vec::new();
    for (index, line) in FIXTURE.lines().enumerate() {
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.split('\t');
        let name = parts.next().unwrap_or_default().to_owned();
        let version: u32 = parts
            .next()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(|| panic!("fixture line {} has no version", index + 1));
        let provenance = parts.next().unwrap_or_default().to_owned();
        assert!(
            parts.next().is_none(),
            "fixture line {} has extra columns",
            index + 1
        );
        rows.push((name, version, provenance));
    }

    assert_eq!(rows.len(), EventKind::ALL.len(), "fixture row count");

    // Fixture -> enum: every row resolves by exact (name, version) and
    // agrees on provenance.
    for (name, version, provenance) in &rows {
        let parsed = EventKind::parse(&format!("{name}.{version}")).unwrap_or_else(|e| {
            panic!("fixture row {name}.{version} is not in the vocabulary: {e}")
        });
        assert_eq!(
            provenance_label(parsed.provenance()),
            provenance,
            "provenance disagrees for {name}.{version}"
        );
    }

    // Enum -> fixture: every kind appears in exactly one row.
    for kind in EventKind::ALL {
        let matches = rows
            .iter()
            .filter(|(name, version, _)| name == kind.name() && *version == kind.version().value())
            .count();
        assert_eq!(matches, 1, "{kind} must appear in exactly one fixture row");
    }
}
