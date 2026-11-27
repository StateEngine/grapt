//! Store-side witnesses for the bounded capture page behind an interest-bound
//! cursor, with the EVR mutation set's laws pinned to their one load-bearing
//! behaviour each.
//!
//! Every test here is a positive witness or a named refusal: a page that is
//! owed panics when it is refused, and a refusal that is owed panics when a
//! page is served. Nothing skips.

use std::path::{Path, PathBuf};
use std::process;
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering as AtomicOrdering};

use grapt_event::{AggregateId, KindError};
use grapt_store::{
    AppendInput, Appended, CapturePage, CaptureReadError, CaptureRow, EventStore, FencedWriter,
    MAX_CAPTURE_PAGE_BYTES, MAX_CAPTURE_PAGE_ROWS,
};
use grapt_types::{
    CaptureCoordinate, CaptureCursor, CaptureCursorError, CaptureInterest, EventFencing, EventId,
    EventSeq, EventVersion, SessionId, WriterEpoch,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

const ALPHABET: [u8; 64] = *b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

/// The test's own no-pad base64url encoder: a token the test builds is a
/// token the store's decoder must accept, and the tail bits it leaves are
/// always the zero bits the canonical decoder demands.
fn b64url(input: &[u8]) -> String {
    let mut out = String::new();
    let mut index = 0usize;
    while index + 3 <= input.len() {
        let triple = (u32::from(input[index]) << 16)
            | (u32::from(input[index + 1]) << 8)
            | u32::from(input[index + 2]);
        out.push(char::from(ALPHABET[(triple >> 18) as usize]));
        out.push(char::from(ALPHABET[((triple >> 12) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[((triple >> 6) & 0x3f) as usize]));
        out.push(char::from(ALPHABET[(triple & 0x3f) as usize]));
        index += 3;
    }
    match input.len() - index {
        1 => {
            let first = u32::from(input[index]);
            out.push(char::from(ALPHABET[(first >> 2) as usize]));
            out.push(char::from(ALPHABET[((first << 4) & 0x3f) as usize]));
        }
        2 => {
            let pair = (u32::from(input[index]) << 8) | u32::from(input[index + 1]);
            out.push(char::from(ALPHABET[(pair >> 10) as usize]));
            out.push(char::from(ALPHABET[((pair >> 4) & 0x3f) as usize]));
            out.push(char::from(ALPHABET[((pair << 2) & 0x3f) as usize]));
        }
        _ => {}
    }
    out
}

/// A version-1 all-interest token around one coordinate list, for the
/// malformed-token battery.
fn token(coordinates: &str) -> String {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(br#"{"version":1,"interest":"all","coordinates":["#);
    bytes.extend_from_slice(coordinates.as_bytes());
    bytes.extend_from_slice(b"}]}");
    b64url(&bytes)
}

static SCRATCH_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A unique scratch directory per test under the system temp dir.
fn scratch(name: &str) -> PathBuf {
    let index = SCRATCH_COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let path =
        std::env::temp_dir().join(format!("evr-event-feed-{name}-{}-{index}", process::id()));
    match std::fs::create_dir_all(&path) {
        Ok(()) => path,
        Err(error) => panic!("scratch directory {path:?} not created: {error}"),
    }
}

/// A test session identity as both the store's aggregate key and the
/// type-level id.
fn session(identity: &str) -> (AggregateId, SessionId) {
    let id = session_id(identity);
    (AggregateId::Session(id.clone()), id)
}

fn session_id(identity: &str) -> SessionId {
    match SessionId::from_str(identity) {
        Ok(id) => id,
        Err(error) => panic!("test identity {identity} is not a session id: {error}"),
    }
}

/// Elects a fenced writer for the identity's aggregate and hands back the
/// fence the appends run under.
fn elect(store: &mut EventStore, identity: &str) -> (AggregateId, FencedWriter) {
    let (aggregate, _id) = session(identity);
    match store.elect_writer(&aggregate) {
        Ok(_election) => {}
        Err(error) => panic!("election for {identity} failed: {error}"),
    }
    match store.fencing_token(&aggregate) {
        Ok(token) => (aggregate, token),
        Err(error) => panic!("fencing token for {identity} failed: {error}"),
    }
}

/// Appends one known-kind event under the fence and hands back its durable
/// identity and sequence.
fn append_one(
    store: &mut EventStore,
    aggregate: &AggregateId,
    token: &FencedWriter,
    identity: &str,
    index: usize,
    data: &Value,
) -> (String, EventSeq) {
    let event_id = format!("evt_{identity}_{index}");
    let id = match EventId::from_str(&event_id) {
        Ok(id) => id,
        Err(error) => panic!("event id {event_id} is not an event id: {error}"),
    };
    let version = match EventVersion::new(1) {
        Some(version) => version,
        None => panic!("event version 1 must be a version"),
    };
    let input = AppendInput {
        id: &id,
        aggregate: aggregate.clone(),
        writer: *token,
        kind_name: "session.tool.success",
        version,
        created_ms: 1_786_060_800_000 + index as i64,
        data,
    };
    match store.append(input, &mut []) {
        Ok((Appended::Committed { seq }, _notices)) => (event_id, EventSeq::new(seq)),
        Ok((Appended::Idempotent { .. }, _notices)) => panic!("event id {event_id} must be fresh"),
        Err(error) => panic!("append {event_id} refused: {error}"),
    }
}

/// Seeds `count` known-kind events for the identity, optionally under a
/// session-list projection row. The projection is staged through a second
/// raw connection against the same file — never by reopening the store,
/// which the read path's law keeps impossible.
fn seed(
    store: &mut EventStore,
    database: Option<&Path>,
    identity: &str,
    count: usize,
    projection: Option<(&str, Option<&str>)>,
) -> Vec<(String, EventSeq)> {
    let (aggregate, token) = elect(store, identity);
    let mut appended = Vec::new();
    for index in 0..count {
        appended.push(append_one(
            store,
            &aggregate,
            &token,
            identity,
            index,
            &json!({ "index": index }),
        ));
    }
    if let Some((directory, workspace)) = projection {
        let database = match database {
            Some(database) => database,
            None => panic!("projection staging needs a file-backed store"),
        };
        let raw = open_raw(database);
        let workspace_cell = workspace.map(str::to_owned);
        match raw.execute(
            "INSERT INTO session_list_projection (session_id, project_id, workspace_id, parent_id, directory, title, updated_ms, provenance_kind, provenance_digest, cutover_seq) VALUES (?1, 'prj_test', ?2, NULL, ?3, 't', 1_786_060_800_000, 'rust-event', ?4, -1)",
            params![identity, workspace_cell, directory, "0".repeat(64)],
        ) {
            Ok(1) => {}
            Ok(rows) => panic!("projection staging inserted {rows} rows, expected 1"),
            Err(error) => panic!("projection staging insert failed: {error}"),
        }
    }
    appended
}

/// Opens a governed store, panicking with the named refusal otherwise.
fn open(database: &Path) -> EventStore {
    match EventStore::open(database, "test") {
        Ok(store) => store,
        Err(error) => panic!("store {database:?} did not open: {error}"),
    }
}

/// A governed in-memory store, panicking with the named refusal otherwise.
fn open_memory() -> EventStore {
    match EventStore::open_in_memory() {
        Ok(store) => store,
        Err(error) => panic!("in-memory store did not open: {error}"),
    }
}

/// A raw rusqlite connection for staging rows the governed write path does
/// not own: projection rows, corruption, and the imported unknown kind.
fn open_raw(database: &Path) -> Connection {
    match Connection::open(database) {
        Ok(raw) => raw,
        Err(error) => panic!("raw connection to {database:?} failed: {error}"),
    }
}

/// A location interest, panicking with the named refusal otherwise.
fn location(directory: &str, workspace: Option<&str>) -> CaptureInterest {
    match CaptureInterest::location(directory.to_owned(), workspace.map(str::to_owned)) {
        Ok(interest) => interest,
        Err(error) => panic!("location interest for {directory} did not form: {error}"),
    }
}

/// A cursor over named coordinates, panicking with the named refusal
/// otherwise.
fn form_cursor(interest: &CaptureInterest, coordinates: Vec<CaptureCoordinate>) -> CaptureCursor {
    match CaptureCursor::new(interest.clone(), coordinates) {
        Ok(cursor) => cursor,
        Err(error) => panic!("cursor did not form: {error}"),
    }
}

/// Reads one page, panicking when the store refused a page that was owed.
fn read(
    store: &EventStore,
    interest: &CaptureInterest,
    cursor: &CaptureCursor,
    limit: usize,
) -> CapturePage {
    match store.read_capture_page(interest, cursor, limit) {
        Ok(page) => page,
        Err(error) => panic!("capture page refused where a page was owed: {error}"),
    }
}

/// Reads one page, panicking when the store served a page that was refused.
fn read_refusal(
    store: &EventStore,
    interest: &CaptureInterest,
    cursor: &CaptureCursor,
    limit: usize,
) -> CaptureReadError {
    match store.read_capture_page(interest, cursor, limit) {
        Ok(page) => {
            panic!(
                "capture page served {} rows where a refusal was owed",
                page.rows.len()
            )
        }
        Err(error) => error,
    }
}

/// Walks an interest from a fresh start until the cursor terminates, and
/// hands back every served aggregate id in order plus the cursor's full
/// carried coordinate set after the terminating page.
fn walk(
    store: &EventStore,
    interest: &CaptureInterest,
    limit: usize,
) -> (Vec<String>, Vec<CaptureCoordinate>) {
    let start = CaptureCursor::start(interest.clone());
    let mut cursor = start;
    let mut served_ids = Vec::new();
    let mut carried: Vec<CaptureCoordinate> = Vec::new();
    loop {
        let page = read(store, interest, &cursor, limit);
        for row in &page.rows {
            let coordinate = row.coordinate().clone();
            served_ids.push(coordinate.aggregate.to_string());
            match carried
                .iter()
                .position(|held| held.aggregate == coordinate.aggregate)
            {
                Some(index) => carried[index] = coordinate.clone(),
                None => carried.push(coordinate),
            }
        }
        match page.continuation {
            Some(next) => cursor = next,
            None => break,
        }
    }
    carried.sort_by(|left, right| left.aggregate.cmp(&right.aggregate));
    (served_ids, carried)
}

#[test]
fn multi_aggregate_pagination_has_no_gap_or_duplicate() {
    let mut store = open_memory();
    let _a = seed(&mut store, None, "ses_a", 3, None);
    let _b = seed(&mut store, None, "ses_b", 4, None);
    let _c = seed(&mut store, None, "ses_c", 2, None);
    let all = CaptureInterest::All;
    let (served, carried) = walk(&store, &all, 2);
    let mut expected = Vec::new();
    for _ in 0..3 {
        expected.push("ses_a".to_owned());
    }
    for _ in 0..4 {
        expected.push("ses_b".to_owned());
    }
    for _ in 0..2 {
        expected.push("ses_c".to_owned());
    }
    assert_eq!(served, expected);
    // Nine rows at two per page is exactly five pages.
    let mut pages = 0usize;
    let mut cursor = CaptureCursor::start(all.clone());
    loop {
        let page = read(&store, &all, &cursor, 2);
        pages += 1;
        match page.continuation {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(pages, 5);
    // The tail cursor is the walk's full carried-forward state, not a
    // hand-picked coordinate: from it, nothing remains.
    let tail = form_cursor(&all, carried);
    let end = read(&store, &all, &tail, 2);
    assert!(end.rows.is_empty());
    assert!(end.continuation.is_none());
    assert_eq!(end.page_bytes, 0);
}

#[test]
fn a_location_interest_paginates_only_its_location() {
    let root = scratch("location");
    let database = root.join("store.db");
    {
        let mut store = open(&database);
        let _a = seed(
            &mut store,
            Some(&database),
            "ses_a",
            2,
            Some(("/work/one", None)),
        );
        let _b = seed(
            &mut store,
            Some(&database),
            "ses_b",
            2,
            Some(("/work/two", None)),
        );
        let _c = seed(
            &mut store,
            Some(&database),
            "ses_c",
            2,
            Some(("/work/one", Some("ws_x"))),
        );
    }
    let store = open(&database);
    // An absent workspace matches every workspace in the directory.
    let one = location("/work/one", None);
    let (served, _carried) = walk(&store, &one, 2);
    assert_eq!(served, vec!["ses_a", "ses_a", "ses_c", "ses_c"]);
    let one_x = location("/work/one", Some("ws_x"));
    let (served_x, _carried_x) = walk(&store, &one_x, 2);
    assert_eq!(served_x, vec!["ses_c", "ses_c"]);
}

#[test]
fn the_cursor_excludes_its_own_rows_strictly() {
    let mut store = open_memory();
    let _a = seed(&mut store, None, "ses_a", 5, None);
    let all = CaptureInterest::All;
    let coordinate = CaptureCoordinate {
        aggregate: session_id("ses_a"),
        seq: EventSeq::new(2),
        fencing: EventFencing::Fenced {
            epoch: WriterEpoch::FIRST,
        },
    };
    let cursor = form_cursor(&all, vec![coordinate]);
    let page = read(&store, &all, &cursor, MAX_CAPTURE_PAGE_ROWS);
    let mut coordinates = Vec::new();
    for row in &page.rows {
        coordinates.push((
            row.coordinate().aggregate.to_string(),
            row.coordinate().seq.value(),
        ));
    }
    assert_eq!(
        coordinates,
        vec![("ses_a".to_owned(), 3), ("ses_a".to_owned(), 4)]
    );
    assert!(page.continuation.is_none());
    // From a fresh start the whole aggregate is still there: the exclusion
    // removes nothing the cursor had not already served.
    let (served, _carried) = walk(&store, &all, 2);
    assert_eq!(served, vec!["ses_a"; 5]);
}

#[test]
fn a_cursor_bound_to_a_different_location_is_refused() {
    let loc_a = location("/work/a", None);
    let loc_b = location("/work/b", None);
    let token = CaptureCursor::start(loc_a.clone()).encode();
    assert_eq!(
        CaptureCursor::decode(&token, &loc_b),
        Err(CaptureCursorError::WrongLocation),
    );
    assert_eq!(
        CaptureCursor::decode(&token, &CaptureInterest::All),
        Err(CaptureCursorError::WrongLocation),
    );
    let all_token = CaptureCursor::start(CaptureInterest::All).encode();
    assert_eq!(
        CaptureCursor::decode(&all_token, &loc_a),
        Err(CaptureCursorError::WrongLocation),
    );
}

#[test]
fn malformed_tokens_are_refused_with_their_named_reason() {
    let all = CaptureInterest::All;
    let valid = CaptureCursor::start(all.clone()).encode();
    let cases = vec![
        b64url(b"!!!"),
        format!("{valid}="),
        b64url(br#"{"version":1 ,"interest":"all","coordinates":[]}"#),
        b64url(br#"{"interest":"all","version":1,"coordinates":[]}"#),
        b64url(br#"{"version":1,"version":1,"interest":"all","coordinates":[]}"#),
        token(r#"{"aggregate":"ses_b","seq":0,"epoch":1},{"aggregate":"ses_a","seq":0,"epoch":1}"#),
        token(r#"{"aggregate":"ses_a","seq":0,"epoch":1},{"aggregate":"ses_a","seq":1,"epoch":1}"#),
        token(r#"{"aggregate":"campaign_a","seq":0,"epoch":1}"#),
        token(r#"{"aggregate":"ses_a","seq":0,"epoch":4294967296}"#),
    ];
    for case in &cases {
        assert!(
            matches!(
                CaptureCursor::decode(case, &all),
                Err(CaptureCursorError::Malformed { .. })
            ),
            "token must be refused as malformed, not decoded"
        );
    }
}

#[test]
fn a_wrong_version_cursor_is_distinct_from_malformed() {
    let all = CaptureInterest::All;
    let token = b64url(br#"{"version":2,"interest":"all","coordinates":[]}"#);
    assert_eq!(
        CaptureCursor::decode(&token, &all),
        Err(CaptureCursorError::WrongVersion { found: 2 }),
    );
}

#[test]
fn a_future_coordinate_is_refused() {
    let mut store = open_memory();
    let _a = seed(&mut store, None, "ses_a", 1, None);
    let all = CaptureInterest::All;
    let coordinate = CaptureCoordinate {
        aggregate: session_id("ses_a"),
        seq: EventSeq::new(1),
        fencing: EventFencing::Fenced {
            epoch: WriterEpoch::FIRST,
        },
    };
    let cursor = form_cursor(&all, vec![coordinate]);
    match read_refusal(&store, &all, &cursor, MAX_CAPTURE_PAGE_ROWS) {
        CaptureReadError::Cursor(CaptureCursorError::FutureCoordinate {
            aggregate,
            seq,
            head,
        }) => {
            assert_eq!(aggregate.to_string(), "ses_a");
            assert_eq!(seq, EventSeq::new(1));
            assert_eq!(head, Some(EventSeq::FIRST));
        }
        other => {
            panic!("a coordinate past the head must refuse as FutureCoordinate, not {other:?}")
        }
    }
    // A coordinate beyond every aggregate's head names no head at all.
    let ghost = CaptureCoordinate {
        aggregate: session_id("ses_b"),
        seq: EventSeq::new(0),
        fencing: EventFencing::LegacyUnfenced,
    };
    let ghost_cursor = form_cursor(&all, vec![ghost]);
    match read_refusal(&store, &all, &ghost_cursor, MAX_CAPTURE_PAGE_ROWS) {
        CaptureReadError::Cursor(CaptureCursorError::FutureCoordinate { head, .. }) => {
            assert_eq!(head, None);
        }
        other => panic!("a headless aggregate must refuse as FutureCoordinate, not {other:?}"),
    }
}

#[test]
fn a_stale_coordinate_is_refused() {
    let root = scratch("stale");
    let database = root.join("store.db");
    {
        let mut store = open(&database);
        let _a = seed(
            &mut store,
            Some(&database),
            "ses_a",
            1,
            Some(("/work/one", None)),
        );
        let _b = seed(
            &mut store,
            Some(&database),
            "ses_b",
            1,
            Some(("/work/two", None)),
        );
    }
    let store = open(&database);
    let one = location("/work/one", None);
    // The cursor carries a coordinate whose session lives in another
    // location: under this interest it is stale, and the page refuses.
    let coordinate = CaptureCoordinate {
        aggregate: session_id("ses_b"),
        seq: EventSeq::new(0),
        fencing: EventFencing::Fenced {
            epoch: WriterEpoch::FIRST,
        },
    };
    let cursor = form_cursor(&one, vec![coordinate]);
    match read_refusal(&store, &one, &cursor, MAX_CAPTURE_PAGE_ROWS) {
        CaptureReadError::Cursor(CaptureCursorError::Stale { aggregate }) => {
            assert_eq!(aggregate.to_string(), "ses_b");
        }
        other => panic!("a coordinate out of its location must refuse as Stale, not {other:?}"),
    }
}

#[path = "event_feed/refusals.rs"]
mod refusals;

#[test]
fn absent_is_an_empty_page_without_continuation() {
    // A fresh governed open creates the projection table empty: under any
    // interest an empty store answers with an empty page, and a location
    // with no session rows is empty, not absent.
    let store = open_memory();
    let all = CaptureInterest::All;
    for interest in [&all, &location("/nowhere", None)] {
        let page = read(
            &store,
            interest,
            &CaptureCursor::start(interest.clone()),
            64,
        );
        assert!(page.rows.is_empty());
        assert!(page.continuation.is_none());
        assert_eq!(page.page_bytes, 0);
    }
    let root = scratch("absent");
    let database = root.join("store.db");
    {
        let mut store = open(&database);
        let _a = seed(
            &mut store,
            Some(&database),
            "ses_a",
            1,
            Some(("/work/one", None)),
        );
    }
    let store = open(&database);
    let other = location("/work/other", None);
    let page = read(&store, &other, &CaptureCursor::start(other.clone()), 64);
    assert!(page.rows.is_empty());
    assert!(page.continuation.is_none());
    assert_eq!(page.page_bytes, 0);
}

#[test]
fn limit_exhaustion_yields_an_honest_continuation_and_resumes() {
    let mut store = open_memory();
    let _a = seed(&mut store, None, "ses_a", 10, None);
    let all = CaptureInterest::All;
    let mut cursor = CaptureCursor::start(all.clone());
    let mut sizes = Vec::new();
    let mut continuations = Vec::new();
    loop {
        let page = read(&store, &all, &cursor, 3);
        sizes.push(page.rows.len());
        continuations.push(page.continuation.is_some());
        match page.continuation {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(sizes, vec![3, 3, 3, 1]);
    assert_eq!(continuations, vec![true, true, true, false]);
    // The byte bound: a row that fits alone is served even when it would
    // overflow a non-empty page, and the bound never refuses a page's first
    // row.
    let d0 = json!({ "pad": "x".repeat(200_000) }).to_string().len();
    let d2 = json!({ "pad": "x".repeat(300_000) }).to_string().len();
    assert_eq!(d0, 200_010);
    assert_eq!(d2, 300_010);
    assert!(d0 < MAX_CAPTURE_PAGE_BYTES);
    assert!(MAX_CAPTURE_PAGE_BYTES < d2);
    let mut big = open_memory();
    let (aggregate, token) = elect(&mut big, "ses_b");
    for (index, pad) in [200_000usize, 200_000usize, 300_000usize]
        .iter()
        .copied()
        .enumerate()
    {
        append_one(
            &mut big,
            &aggregate,
            &token,
            "ses_b",
            index,
            &json!({ "pad": "x".repeat(pad) }),
        );
    }
    let mut cursor = CaptureCursor::start(all.clone());
    let mut sizes = Vec::new();
    let mut pages_bytes = Vec::new();
    let mut continuations = Vec::new();
    loop {
        let page = read(&big, &all, &cursor, MAX_CAPTURE_PAGE_ROWS);
        sizes.push(page.rows.len());
        pages_bytes.push(page.page_bytes);
        continuations.push(page.continuation.is_some());
        match page.continuation {
            Some(next) => cursor = next,
            None => break,
        }
    }
    assert_eq!(sizes, vec![1, 1, 1]);
    assert_eq!(pages_bytes, vec![d0, d0, d2]);
    assert_eq!(continuations, vec![true, true, false]);
}

#[test]
fn restart_preserves_the_same_page() {
    let root = scratch("restart");
    let database = root.join("store.db");
    {
        let mut store = open(&database);
        let _a = seed(&mut store, Some(&database), "ses_a", 4, None);
    }
    let described = |page: &CapturePage| -> Vec<(String, u64, String)> {
        page.rows
            .iter()
            .map(|row| {
                let coordinate = row.coordinate();
                let event_id = match row {
                    CaptureRow::Event { event, .. } => event.id.to_string(),
                    CaptureRow::SkippedUnknownKind { .. } => "skip".to_owned(),
                };
                (
                    coordinate.aggregate.to_string(),
                    coordinate.seq.value(),
                    event_id,
                )
            })
            .collect()
    };
    let all = CaptureInterest::All;
    let first = {
        let store = open(&database);
        read(&store, &all, &CaptureCursor::start(all.clone()), 2)
    };
    let seqs: Vec<u64> = first
        .rows
        .iter()
        .map(|row| row.coordinate().seq.value())
        .collect();
    assert_eq!(seqs, vec![0, 1]);
    let continuation = match &first.continuation {
        Some(token) => token.clone(),
        None => panic!("four events at two per page must carry a continuation"),
    };
    // A restarted store serves the same page from the same start, and the
    // continuation token it carries is the same token.
    let again = {
        let store = open(&database);
        read(&store, &all, &CaptureCursor::start(all.clone()), 2)
    };
    assert_eq!(described(&again), described(&first));
    assert_eq!(again.continuation.as_ref(), Some(&continuation));
    // And the walk resumes exactly where the token left off.
    let resumed = {
        let store = open(&database);
        read(&store, &all, &continuation, 2)
    };
    let seqs: Vec<u64> = resumed
        .rows
        .iter()
        .map(|row| row.coordinate().seq.value())
        .collect();
    assert_eq!(seqs, vec![2, 3]);
    assert!(resumed.continuation.is_none());
}

#[test]
fn an_unknown_kind_is_served_as_a_typed_skip_and_advances() {
    let root = scratch("unknown-kind");
    let database = root.join("store.db");
    {
        let mut store = open(&database);
        let _a = seed(&mut store, Some(&database), "ses_a", 1, None);
    }
    // An imported row the read posture does not know: the kind is outside
    // the closed vocabulary, and the fencing column is carried, never minted.
    {
        let raw = open_raw(&database);
        let payload = json!({ "note": "imported" }).to_string();
        match raw.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES ('evt_ses_a_im', 'session', 'ses_a', 1, 1_786_060_800_001, 'session.imaginary.9', ?1, 1)",
            params![payload],
        ) {
            Ok(1) => {}
            Ok(rows) => panic!("import staging inserted {rows} rows, expected 1"),
            Err(error) => panic!("import staging failed: {error}"),
        }
        match raw.execute(
            "INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) VALUES ('session', 'ses_a', 1) ON CONFLICT (aggregate_family, aggregate_id) DO UPDATE SET seq = 1",
            [],
        ) {
            Ok(1) => {}
            Ok(rows) => panic!("sequence staging updated {rows} rows, expected 1"),
            Err(error) => panic!("sequence staging failed: {error}"),
        }
    }
    let store = open(&database);
    let all = CaptureInterest::All;
    let page = read(&store, &all, &CaptureCursor::start(all.clone()), 64);
    assert_eq!(page.rows.len(), 2);
    match &page.rows[0] {
        CaptureRow::Event { coordinate, event } => {
            assert_eq!(coordinate.aggregate.to_string(), "ses_a");
            assert_eq!(coordinate.seq, EventSeq::FIRST);
            assert_eq!(event.kind_name(), "session.tool.success");
        }
        other => panic!("the first row must be the known event, not {other:?}"),
    }
    match &page.rows[1] {
        CaptureRow::SkippedUnknownKind { coordinate, kind } => {
            assert_eq!(coordinate.aggregate.to_string(), "ses_a");
            assert_eq!(coordinate.seq, EventSeq::new(1));
            assert_eq!(
                *kind,
                KindError::UnknownName {
                    name: "session.imaginary".to_owned(),
                    version: 9,
                }
            );
        }
        other => panic!("the second row must be the typed skip, not {other:?}"),
    }
    assert!(page.continuation.is_none());
    // The skip advances the cursor like any served row: after the fence at
    // the known event, only the skip remains.
    let coordinate = CaptureCoordinate {
        aggregate: session_id("ses_a"),
        seq: EventSeq::FIRST,
        fencing: EventFencing::LegacyUnfenced,
    };
    let cursor = form_cursor(&all, vec![coordinate]);
    let page = read(&store, &all, &cursor, 64);
    assert_eq!(page.rows.len(), 1);
    match &page.rows[0] {
        CaptureRow::SkippedUnknownKind { coordinate, .. } => {
            assert_eq!(coordinate.seq, EventSeq::new(1));
        }
        other => panic!("the only row after the fence must be the skip, not {other:?}"),
    }
    assert!(page.continuation.is_none());
}

/// Counts the distinct aggregates an interest sees, panicking when the
/// store refuses a count that was owed.
fn count_of(store: &EventStore, label: &str, interest: &CaptureInterest) -> u64 {
    match store.count_capture_aggregates(interest) {
        Ok(count) => count,
        Err(error) => panic!("{label} count refused: {error}"),
    }
}

#[test]
fn count_capture_aggregates_counts_distinct_aggregates() {
    // EVR8: bound counts aggregates, not rows — 1025 aggregates over 1026
    // rows (two-stage CTEs stay under SQLite's 1000 recursion depth).
    let root = scratch("count-distinct");
    let database = root.join("store.db");
    let _ = open(&database);
    let raw = open_raw(&database);
    match raw.execute_batch(
        "WITH RECURSIVE a(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM a WHERE i < 512), b(i) AS (SELECT DISTINCT i FROM (SELECT i FROM a UNION ALL SELECT i + 512 FROM a)) INSERT INTO event_sequence (aggregate_family, aggregate_id, seq) SELECT 'session', printf('ses_%04d', i), 0 FROM b; WITH RECURSIVE a(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM a WHERE i < 512), b(i) AS (SELECT DISTINCT i FROM (SELECT i FROM a UNION ALL SELECT i + 512 FROM a)) INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) SELECT printf('evt_ses_%04d_0', i), 'session', printf('ses_%04d', i), 0, 1_786_060_800_000, 'session.tool.success.1', '{}', 1 FROM b; UPDATE event_sequence SET seq = 1 WHERE aggregate_family = 'session' AND aggregate_id = 'ses_0000'; INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch) VALUES ('evt_ses_0000_1', 'session', 'ses_0000', 1, 1_786_060_800_000, 'session.tool.success.1', '{}', 1)",
    ) {
        Ok(()) => {}
        Err(error) => panic!("count staging failed: {error}"),
    }
    let count = count_of(&open(&database), "all", &CaptureInterest::All);
    assert_eq!(count, 1025, "aggregates, not rows");
}

#[test]
fn count_capture_aggregates_scopes_to_the_location_and_treats_absent_as_zero() {
    let root = scratch("count-location");
    let database = root.join("store.db");
    let mut store = open(&database);
    let one_loc = Some(("/work/one", None));
    let two_loc = Some(("/work/two", None));
    let _one = seed(&mut store, Some(&database), "ses_one", 2, one_loc);
    let _two = seed(&mut store, Some(&database), "ses_two", 3, two_loc);
    let one = location("/work/one", None);
    let scoped = count_of(&store, "scoped", &one);
    assert_eq!(scoped, 1, "counts only its own aggregate");
    let all = CaptureInterest::All;
    let whole = count_of(&store, "all", &all);
    assert_eq!(whole, 2, "the all interest must count every aggregate");
    // Absent: the projection table is gone — zero, named as such, never unreadable.
    match open_raw(&database).execute("DROP TABLE session_list_projection", []) {
        Ok(_) => {}
        Err(error) => panic!("projection drop failed: {error}"),
    }
    let absent = count_of(&store, "absent", &one);
    assert_eq!(absent, 0, "missing table is absent: absent is zero");
}

#[test]
fn a_windows_directory_round_trips_and_walks_exactly_once() {
    // HOLD B1 at the store door: a Windows directory — backslashes in the
    // interest — must survive the token codec, and the walk is exactly once.
    let directory = "C:\\workspace\\repo";
    let root = scratch("count-windows");
    let database = root.join("store.db");
    let mut store = open(&database);
    let win_loc = Some((directory, None));
    let _rows = seed(&mut store, Some(&database), "ses_win", 2, win_loc);
    let interest = location(directory, None);
    let decoded = |token: &str| match CaptureCursor::decode(token, &interest) {
        Ok(cursor) => cursor,
        Err(error) => panic!("the token must decode: {error}"),
    };
    let start = decoded(&CaptureCursor::start(interest.clone()).encode());
    let first = read(&store, &interest, &start, 1);
    assert_eq!(first.rows.len(), 1);
    assert_eq!(first.rows[0].coordinate().seq, EventSeq::FIRST);
    let continuation = match &first.continuation {
        Some(token) => token.clone(),
        None => panic!("two rows at one per page must carry a continuation"),
    };
    let resumed = decoded(&continuation.encode());
    let second = read(&store, &interest, &resumed, 1);
    assert_eq!(second.rows.len(), 1);
    assert_eq!(second.rows[0].coordinate().seq, EventSeq::new(1));
    assert!(second.continuation.is_none());
}

#[test]
fn a_stale_location_coordinate_refuses_the_carried_cursor() {
    // S3: the projection row vanishes after the cursor minted; the read
    // refuses as Stale and the client re-lists from the interest start.
    let root = scratch("count-stale");
    let database = root.join("store.db");
    let mut store = open(&database);
    let gone_loc = Some(("/work/gone", None));
    let _rows = seed(&mut store, Some(&database), "ses_gone", 1, gone_loc);
    let interest = location("/work/gone", None);
    let start = CaptureCursor::start(interest.clone());
    let first = read(&store, &interest, &start, 1);
    assert_eq!(first.rows.len(), 1);
    let coordinate = first.rows[0].coordinate().clone();
    let cursor = form_cursor(&interest, vec![coordinate]);
    match open_raw(&database).execute(
        "DELETE FROM session_list_projection WHERE session_id = 'ses_gone'",
        [],
    ) {
        Ok(1) => {}
        Ok(rows) => panic!("projection delete removed {rows} rows, expected 1"),
        Err(error) => panic!("projection delete failed: {error}"),
    }
    let error = read_refusal(&store, &interest, &cursor, 1);
    assert!(
        matches!(
            error,
            CaptureReadError::Cursor(CaptureCursorError::Stale { .. })
        ),
        "a deleted projection must refuse as stale: {error:?}"
    );
}

#[test]
fn a_fenced_and_an_unfenced_cursor_serve_the_same_page() {
    // S5: the store excludes by sequence only. A Fenced coordinate and a
    // LegacyUnfenced one at the same (aggregate, seq) are the same read.
    let mut store = open_memory();
    let _rows = seed(&mut store, None, "ses_epoch", 2, None);
    let all = CaptureInterest::All;
    let epoch = match WriterEpoch::new(1) {
        Some(epoch) => epoch,
        None => panic!("writer epoch 1 must form"),
    };
    let fenced_coordinate = CaptureCoordinate {
        aggregate: session_id("ses_epoch"),
        seq: EventSeq::FIRST,
        fencing: EventFencing::Fenced { epoch },
    };
    let mut unfenced_coordinate = fenced_coordinate.clone();
    unfenced_coordinate.fencing = EventFencing::LegacyUnfenced;
    let fenced = form_cursor(&all, vec![fenced_coordinate]);
    let unfenced = form_cursor(&all, vec![unfenced_coordinate]);
    let coordinates = |page: &CapturePage| {
        page.rows
            .iter()
            .map(|row| row.coordinate().clone())
            .collect::<Vec<_>>()
    };
    let fenced_page = read(&store, &all, &fenced, 64);
    let unfenced_page = read(&store, &all, &unfenced, 64);
    assert_eq!(coordinates(&fenced_page), coordinates(&unfenced_page));
}
