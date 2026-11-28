use grapt_release::{fingerprint_file, inspect_status, rehearse, restore_backup_rehearsal};
use grapt_types::{ClockAuthority, MigrationId, Timestamp};
use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug, Deserialize)]
struct Action {
    ordinal: u64,
    action: String,
    expected: String,
}

#[derive(Debug, Deserialize)]
struct CorpusManifest {
    law_pack: LawPack,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct LawPack {
    revision: u64,
    sources: Vec<LawPin>,
    matrix: LawPin,
    release: LawPin,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct LawPin {
    path: String,
    sha256: String,
}

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_788_000_000_123)
    }
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/rust-reconstruction/r6-migration-rollback-v1")
}

fn legacy_database(path: &Path) -> TestResult {
    Connection::open(path)?.execute_batch(
        "CREATE TABLE event_sequence (aggregate_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, owner_id TEXT);
         CREATE TABLE event (id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL, created INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL);
         CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
         INSERT INTO event_sequence VALUES ('ses_corpus', 2, NULL);
         INSERT INTO event VALUES ('evt_1', 'ses_corpus', 0, 1, 'session.created.1', '{}'), ('evt_2', 'ses_corpus', 2, 2, 'session.updated.1', '{}');
         INSERT INTO kv VALUES ('theme', 'dark');",
    )?;
    Ok(())
}

fn json_lines(path: &Path) -> TestResult<Vec<Value>> {
    let text = std::fs::read_to_string(path)?;
    text.lines()
        .map(|line| serde_json::from_str(line).map_err(Into::into))
        .collect()
}

#[test]
fn the_language_neutral_corpus_replays_through_the_public_operator() -> TestResult {
    let fixture = fixture();
    let actions = std::fs::read_to_string(fixture.join("actions.jsonl"))?
        .lines()
        .map(serde_json::from_str::<Action>)
        .collect::<Result<Vec<_>, _>>()?;
    let root = std::env::temp_dir().join(format!("grapt-r6mr-corpus-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    let source = root.join("source.sqlite3");
    let run = root.join("run");
    legacy_database(&source)?;
    let source_before = fingerprint_file(&source)?;
    let run_id = MigrationId::new("r6mr-corpus").ok_or("run id")?;
    let mut durable = Vec::new();
    for (index, action) in actions.iter().enumerate() {
        assert_eq!(action.ordinal, u64::try_from(index)?.saturating_add(1));
        let outcome = match action.action.as_str() {
            "rehearse" | "resume" => {
                let _ = rehearse(&source, &run, run_id.clone(), "0.2.0", &FixedClock)?;
                let status = inspect_status(&run)?;
                json!({"ordinal":action.ordinal,"outcome":"ready-for-handoff","checkpoints":status.completed_checkpoints})
            }
            "restore" => {
                let destination = root.join("standalone-restored.sqlite3");
                let receipt = restore_backup_rehearsal(
                    &source,
                    &run.join("pre-mutation-rollback.sqlite3"),
                    &destination,
                )?;
                let rows: i64 = Connection::open_with_flags(
                    receipt.path(),
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
                )?
                .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
                json!({"ordinal":action.ordinal,"outcome":"rollback-restored","rows":rows})
            }
            "backup-absent" => {
                let absent_run = root.join("absent-run");
                let _ = rehearse(
                    &source,
                    &absent_run,
                    MigrationId::new("r6mr-absent").ok_or("id")?,
                    "0.2.0",
                    &FixedClock,
                )?;
                std::fs::remove_file(absent_run.join("pre-mutation-rollback.sqlite3"))?;
                let refused = rehearse(
                    &source,
                    &absent_run,
                    MigrationId::new("r6mr-absent").ok_or("id")?,
                    "0.2.0",
                    &FixedClock,
                )
                .is_err();
                assert!(refused);
                json!({"ordinal":action.ordinal,"outcome":"refused-backup-absent","source_unchanged":fingerprint_file(&source)? == source_before})
            }
            unknown => return Err(format!("unknown corpus action {unknown:?}").into()),
        };
        assert_eq!(outcome["outcome"], action.expected);
        durable.push(outcome);
    }
    assert_eq!(
        durable,
        json_lines(&fixture.join("expected/durable.jsonl"))?
    );
    let state = json!({
        "actions_run": actions.len(),
        "admitted": 3,
        "refused": 1,
        "ready_for_handoff": inspect_status(&run)?.ready_for_handoff,
        "source_unchanged": fingerprint_file(&source)? == source_before,
    });
    let expected_state: Value =
        serde_json::from_slice(&std::fs::read(fixture.join("expected/state.json"))?)?;
    assert_eq!(state, expected_state);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg_attr(
    not(feature = "frozen-fixture"),
    ignore = "reads the frozen fixture corpus: set SECODE_FIXTURES and enable frozen-fixture"
)]
#[test]
fn corpus_pins_the_complete_ordered_current_law_pack() -> TestResult {
    let manifest: CorpusManifest =
        serde_json::from_slice(&std::fs::read(fixture().join("manifest.json"))?)?;
    let current_path = current_law_pack_path();
    let current_bytes = std::fs::read(&current_path).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "current law-pack evidence is unavailable at {}: {error}",
                current_path.display()
            ),
        )
    })?;
    let current: LawPack = serde_json::from_slice(&current_bytes)?;
    assert_eq!(
        manifest.law_pack, current,
        "corpus law-pack pins differ by value, completeness, or source order"
    );
    Ok(())
}

fn current_law_pack_path() -> PathBuf {
    if let Some(path) = std::env::var_os("GRAPT_LAW_PACK") {
        return PathBuf::from(path);
    }
    if let Some(root) = std::env::var_os("SECODE_FIXTURES") {
        return PathBuf::from(root).join("law-pack.json");
    }
    if let Some(root) = std::env::var_os("REMOTE_COMPUTE_ARTIFACT_ROOT") {
        return PathBuf::from(root).join("fixtures/rust-reconstruction/law-pack.json");
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-reconstruction/law-pack.json")
}
