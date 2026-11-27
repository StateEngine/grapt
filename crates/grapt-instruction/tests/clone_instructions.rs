//! Acceptance against the SANITIZED frozen clone: three-way blob addressing
//! classification pinned; delta referential integrity (every hash named by
//! an event exists in instruction_blob); delta fold matches
//! instruction_state.current_values for every session with state.
//! (Sanitization redacts 4 credential values; the classification numbers
//! below are the sanitized clone's — the raw clone differs only in which
//! rows violate, not how many.)

use grapt_instruction::{classify_blob, BlobAddressing, InstructionFold};
use grapt_types::{InstructionHash, InstructionKey, SessionId};
use serde_json::Value;
use std::collections::HashSet;
#[path = "../../../test-support/evidence.rs"]
mod evidence;

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run with --include-ignored on the evidence machine"]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn blob_addressing_and_delta_fold_hold_on_real_data() -> Result<(), Box<dyn std::error::Error>> {
    let path = evidence::frozen_clone();
    let conn =
        rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;

    // 1. Three-way content-address classification.
    let mut raw = 0u64;
    let mut canonical = 0u64;
    let mut violations = 0u64;
    let mut blob_hashes: HashSet<InstructionHash> = HashSet::new();
    {
        let mut stmt = conn.prepare("SELECT hash, value FROM instruction_blob")?;
        let mut rows = stmt.query([])?;
        while let Some(row) = rows.next()? {
            let hash: String = row.get(0)?;
            let value: String = row.get(1)?;
            let hash: InstructionHash = hash.parse()?;
            match classify_blob(hash, &value) {
                BlobAddressing::RawUtf8 => raw += 1,
                BlobAddressing::CanonicalJson => canonical += 1,
                BlobAddressing::Violation => violations += 1,
            }
            blob_hashes.insert(hash);
        }
    }
    eprintln!("BLOBS: raw={raw} canonical={canonical} violations={violations}");
    assert_eq!(raw + canonical + violations, 309);
    assert_eq!(raw, 283);
    assert_eq!(
        canonical, 26,
        "ALL structured values hash over canonical bytes"
    );
    assert_eq!(
        violations, 0,
        "the content-address law is clean on real data"
    );

    // 2. delta referential integrity + fold vs instruction_state.
    let log = grapt_store::EventLog::open_read_only(&path)?;
    let mut fold = InstructionFold::default();
    let mut dangling: Vec<String> = Vec::new();
    let mut decode_errors: Vec<String> = Vec::new();
    log.for_each_in_replay_order(|e| {
        if e.kind.name() == "session.instructions.updated" {
            if let Some(delta) = e.data.get("delta") {
                if let Value::Object(map) = delta {
                    for (key, hash) in map {
                        if let Value::String(h) = hash {
                            if h == "removed" {
                                continue;
                            }
                            match h.parse::<InstructionHash>() {
                                Ok(hash) if !blob_hashes.contains(&hash) => {
                                    dangling.push(format!("{key}: {h}"));
                                }
                                Ok(_) => {}
                                Err(error) => decode_errors.push(format!("{key}: {error}")),
                            }
                        }
                    }
                }
                // aggregate_id is a typed SessionId at the decode boundary now.
                if let Err(error) = fold.apply(e.aggregate_id.clone(), delta) {
                    decode_errors.push(format!("{}: {error}", e.id));
                }
            }
        }
    })?;
    assert!(
        decode_errors.is_empty(),
        "instruction decode failures: {decode_errors:?}"
    );
    assert!(
        dangling.is_empty(),
        "deltas referencing missing blobs: {dangling:?}"
    );

    let mut stmt = conn.prepare("SELECT session_id, current_values FROM instruction_state")?;
    let mut rows = stmt.query([])?;
    let mut compared = 0u64;
    let mut divergent = 0u64;
    while let Some(row) = rows.next()? {
        let session: String = row.get(0)?;
        let current_json: String = row.get(1)?;
        let table: Value = serde_json::from_str(&current_json)?;
        let session: SessionId = session.parse()?;
        let folded = fold.current.get(&session);
        if let (Value::Object(table_map), Some(folded_map)) = (&table, folded) {
            compared += 1;
            let matches = table_map.len() == folded_map.len()
                && table_map.iter().all(|(k, v)| {
                    InstructionKey::new(k)
                        .zip(
                            v.as_str()
                                .and_then(|hash| hash.parse::<InstructionHash>().ok()),
                        )
                        .is_some_and(|(key, hash)| folded_map.get(&key) == Some(&hash))
                });
            if !matches {
                divergent += 1;
            }
        }
    }
    eprintln!(
        "STATE: sessions={} compared={compared} divergent={divergent} deltas={}",
        fold.current.len(),
        fold.deltas_applied
    );
    assert_eq!(fold.current.len(), 253);
    assert_eq!(compared, 253);
    assert_eq!(divergent, 0);
    assert_eq!(fold.deltas_applied, 911);

    let invalid_entries: i64 = conn.query_row(
        "SELECT COUNT(*) FROM instruction_entry
         WHERE removed NOT IN (0, 1)
            OR (removed = 0 AND value IS NULL)
            OR (removed = 1 AND value IS NOT NULL)
            OR time_updated < time_created",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        invalid_entries, 0,
        "all frozen instruction entries satisfy the strict Rust union"
    );
    Ok(())
}
