//! The Session read view: existence and placement, straight off the durable
//! log.
//!
//! Law: `SECODE_RUST_FOUNDATIONAL_TYPES_AND_LIFECYCLES_2026-08-07.md`
//! §"Session lifecycle", and D1's coverage finding that `session` is
//! **event-replayable** state (`DB_EVIDENCE_D1_2026-08-08.md` §5).
//!
//! # Why this reads events rather than a projection table
//!
//! The Rust write path owns `event` / `event_sequence`; the `session`
//! projection table is not yet Rust-owned, and inventing one here would put a
//! durable schema in a read view. The reads use the existing
//! family/aggregate/type/sequence index; lifecycle ordering must not make
//! SQLite scan unrelated conversation rows instead.
//!
//! # Three answers, never conflated
//!
//! A store read that fails is [`SessionStoreError::Store`]. A Session with no
//! seed is `Ok(false)` / [`SessionStoreError::Unknown`]. A seed that exists but
//! cannot be understood is [`SessionStoreError::Corrupt`] — never a quiet
//! "unknown Session", which would tell a caller their Session never existed
//! because a row was malformed.

mod fork;

use grapt_store::EventStore;
use grapt_types::{LocationRef, LocationRefError, SessionId, WorkspaceId};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;

/// The durable kind that seeds a Session, versioned as the log stores it.
const CREATED: &str = "session.created.1";

/// The aggregate family these reads are entitled to answer from.
///
/// `event` is ONE log carrying a family-tagged union of rows, and older
/// databases may hold rows of the retired campaign family. Reading by `aggregate_id` alone asks a question
/// the table can no longer answer unambiguously: the identity column is not a
/// key on its own, only `(aggregate_family, aggregate_id)` is — that is the
/// primary key of `event_sequence` and the leading pair of the unique index.
/// `F3B1A_RESIDUALS.md` item 5 recorded that these two reads were correct only
/// while sessions were the only writable family; that precondition ended when
/// the campaign family landed.
///
/// See `F3B4C_RESIDUALS.md` item 9 for the exact accounting. The `type`
/// predicate ALSO separates the families for every campaign row the STORE
/// ITSELF wrote, because the retired campaign append path could only mint
/// `agent_campaign.*` kind strings. It does NOT separate them in general:
/// `event_family_shape` constrains the payload COLUMNS for a family and says
/// nothing about `type`, so a row arriving by another route — a hostile writer,
/// or a database part-way through a conversion — may legally carry
/// `aggregate_family = 'campaign'` together with `session.created.1`.
///
/// Against that row this predicate is what answers correctly, and its removal
/// is CAUGHT: `M13` and `M14` red independently on dropping it from these two
/// reads, witnessed by `cross_family_identity.rs`, which stages exactly such a
/// row. So "no other family happens to mint this kind string today" is a fact
/// about what the store writes, not a law about what the table can hold, and a
/// read keyed on half a key is a defect either way.
const SESSION_FAMILY: &str = "session";

// Unary + preserves sequence values but prevents SQLite from choosing the
// aggregate/sequence index solely to avoid sorting. The existing type index
// selects only owned kinds; their rows still fold in global sequence order.
const DELETED_SQL: &str = "SELECT type, CASE WHEN type = 'session.deleted.1'
                    AND length(CAST(data AS BLOB)) <= ?2 THEN data END
                 FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND type IN ('session.deleted.1', 'session.deleted.2')
                 ORDER BY +seq LIMIT 1";

const LIFECYCLE_SQL: &str = "SELECT type, data FROM event
                 WHERE aggregate_family = ?1 AND aggregate_id = ?2
                   AND type IN ('session.created.1', 'session.renamed.1',
                                'session.agent.selected.1', 'session.model.selected.1',
                                'session.preset.selected.1', 'session.launched.1')
                 ORDER BY +seq";

#[derive(Debug, thiserror::Error)]
pub enum SessionStoreError {
    #[error("session store read failed for {session}: {source}")]
    Store {
        session: String,
        #[source]
        source: rusqlite::Error,
    },
    #[error("unknown session {session}")]
    Unknown { session: String },
    #[error("session {session} seed is corrupt: {reason}")]
    Corrupt { session: String, reason: String },
}

/// A borrowed, synchronous read view over one store connection.
///
/// It borrows rather than owns: the write path owns the connection, and a
/// second connection to the same database would be a second view of a
/// transaction boundary somebody else controls.
#[derive(Debug)]
pub struct SessionStore<'a> {
    conn: &'a Connection,
}

impl<'a> SessionStore<'a> {
    pub fn over(store: &'a EventStore) -> Self {
        Self {
            conn: store.connection(),
        }
    }

    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Whether this Session has been seeded durably.
    ///
    /// `Result`-bearing on purpose: a read failure must surface as a store
    /// error. Collapsing it into `false` would report a Session as
    /// non-existent because a disk hiccuped.
    pub fn exists(&self, session: &SessionId) -> Result<bool, SessionStoreError> {
        let seeded =
            Self::seeded(self.conn, session).map_err(|source| SessionStoreError::Store {
                session: session.as_str().to_owned(),
                source,
            })?;
        Ok(seeded && !self.deleted(session)?)
    }

    pub(crate) fn deleted(&self, session: &SessionId) -> Result<bool, SessionStoreError> {
        let corrupt = |reason: String| SessionStoreError::Corrupt {
            session: session.as_str().to_owned(),
            reason,
        };
        let payload_limit =
            i64::try_from(grapt_event::LegacyDataLimit::MEASURED_DEFAULT.max_data_bytes())
                .map_err(|_| corrupt("durable data bound exceeds SQLite range".into()))?;
        let deletion: Option<(String, Option<String>)> = self
            .conn
            .query_row(
                DELETED_SQL,
                rusqlite::params![session.as_str(), payload_limit],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()
            .map_err(|source| SessionStoreError::Store {
                session: session.as_str().to_owned(),
                source,
            })?;
        let Some((kind, raw)) = deletion else {
            return Ok(false);
        };
        if kind == "session.deleted.1" {
            let raw = raw.ok_or_else(|| {
                corrupt("session.deleted.1 payload exceeds the durable data bound".into())
            })?;
            let data = serde_json::from_str(&raw)
                .map_err(|error| corrupt(format!("session.deleted.1 JSON: {error}")))?;
            if crate::deletion::decode_v1(&data).map_err(corrupt)? != *session {
                return Err(corrupt(
                    "session.deleted.1 sessionID differs from aggregate".into(),
                ));
            }
        }
        Ok(true)
    }

    /// The same seed question, asked from inside a caller's open transaction.
    ///
    /// The create path must ask it under the write lock — a check taken on a
    /// separate connection could pass and then lose the race to another
    /// writer's seed. It shares [`Self::seeded`] with [`Self::exists`] rather
    /// than restating the SQL: the family predicate below is load-bearing
    /// (`M13`/`M14` red on its removal), and a second copy of a read that a
    /// mutation row already guards would be a second place for that guard to
    /// go missing unwitnessed.
    ///
    /// It returns the raw `rusqlite::Error` because the create path names its
    /// own typed refusal; wrapping into [`SessionStoreError`] here and
    /// unwrapping there would lose the session coordinate twice.
    pub(crate) fn seeded_within(
        tx: &grapt_store::GovernedTransaction<'_>,
        session: &SessionId,
    ) -> Result<bool, rusqlite::Error> {
        Self::seeded(tx, session)
    }

    fn seeded(conn: &Connection, session: &SessionId) -> Result<bool, rusqlite::Error> {
        let found: Option<i64> = conn
            .query_row(
                "SELECT 1 FROM event
                 WHERE aggregate_family = ?1 AND aggregate_id = ?2 AND type = ?3 LIMIT 1",
                rusqlite::params![SESSION_FAMILY, session.as_str(), CREATED],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| {
                if error == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(error)
                }
            })?;
        Ok(found.is_some())
    }

    /// The Session's placement, from its seed.
    ///
    /// The oracle writes `info.directory` and the optional `info.workspaceID`
    /// at `session.create` (`core/src/session.ts:413-415`); this reads exactly
    /// those and canonicalizes them. A seed without a directory is corruption,
    /// never a defaulted placement — running a Session in the wrong directory
    /// is worse than refusing to run it.
    pub fn placement(&self, session: &SessionId) -> Result<LocationRef, SessionStoreError> {
        let corrupt = |reason: String| SessionStoreError::Corrupt {
            session: session.as_str().to_owned(),
            reason,
        };

        // Earliest seed wins: a Session is created once, and a second
        // `session.created` for one aggregate is a fact about a broken log,
        // not a re-placement.
        let raw = self.seed_data(session)?;

        let data: Value = serde_json::from_str(&raw)
            .map_err(|error| corrupt(format!("seed data is not valid JSON: {error}")))?;
        let directory = data
            .pointer("/info/directory")
            .and_then(Value::as_str)
            .ok_or_else(|| corrupt("seed has no info.directory".to_owned()))?;
        let workspace = match data.pointer("/info/workspaceID") {
            // ABSENT means implicit-local placement, and only absent means it.
            // The oracle's schema is `optional(WorkspaceID)` —
            // `Schema.optionalKey` over a branded string — so the KEY may be
            // missing, but a present value must decode as a workspace id.
            // `null` decodes as neither, which makes it a fact about a broken
            // log. Folding it into `None` would silently run the Session in the
            // implicit-local workspace, which is the one place a wrong
            // placement is invisible.
            None => None,
            Some(Value::Null) => {
                return Err(corrupt(
                    "seed info.workspaceID is null; the schema admits an absent key, never a null value"
                        .to_owned(),
                ))
            }
            Some(Value::String(raw)) => match WorkspaceId::new(raw.clone()) {
                Some(workspace) => Some(workspace),
                None => return Err(corrupt("seed has an empty info.workspaceID".to_owned())),
            },
            Some(_) => return Err(corrupt("seed info.workspaceID is not a string".to_owned())),
        };

        LocationRef::new(directory, workspace).map_err(|error: LocationRefError| {
            corrupt(format!("seed placement is not a valid Location: {error}"))
        })
    }

    /// The Session this one records as its parent, if the seed named one.
    ///
    /// ABSENT is the ordinary answer and an explicit null is corruption, for
    /// the same reason [`Self::placement`] refuses a null `workspaceID`: the
    /// schema admits a missing key, never a null value, and folding null into
    /// `None` would report an orphan Session as deliberately top-level.
    pub fn parent(&self, session: &SessionId) -> Result<Option<SessionId>, SessionStoreError> {
        let corrupt = |reason: String| SessionStoreError::Corrupt {
            session: session.as_str().to_owned(),
            reason,
        };
        let raw = self.seed_data(session)?;
        let data: Value = serde_json::from_str(&raw)
            .map_err(|error| corrupt(format!("seed data is not valid JSON: {error}")))?;
        match data.pointer("/info/parentID") {
            None => Ok(None),
            Some(Value::Null) => Err(corrupt(
                "seed info.parentID is null; the schema admits an absent key, never a null value"
                    .to_owned(),
            )),
            Some(Value::String(raw)) => raw
                .parse()
                .map(Some)
                .map_err(|error| corrupt(format!("seed info.parentID: {error}"))),
            Some(_) => Err(corrupt("seed info.parentID is not a string".to_owned())),
        }
    }

    /// Every lifecycle row on this Session's aggregate, oldest first.
    ///
    /// Scoped to the kinds [`crate::event::SessionEvent`] folds rather than
    /// selecting the whole aggregate: a Session's log also carries its content,
    /// tool and step facts, and reading all of them to fold five would make the
    /// cost of `record` grow with the length of the conversation.
    pub(crate) fn lifecycle_rows(
        &self,
        session: &SessionId,
    ) -> Result<Vec<(String, u32, Value)>, SessionStoreError> {
        let store = |source| SessionStoreError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let corrupt = |reason: String| SessionStoreError::Corrupt {
            session: session.as_str().to_owned(),
            reason,
        };
        let mut statement = self.conn.prepare(LIFECYCLE_SQL).map_err(store)?;
        let mapped = statement
            .query_map(rusqlite::params![SESSION_FAMILY, session.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(store)?;
        let mut out = Vec::new();
        for row in mapped {
            let (versioned, data) = row.map_err(store)?;
            let (kind, version) = versioned
                .rsplit_once('.')
                .ok_or_else(|| corrupt(format!("stored type {versioned} carries no version")))?;
            let version: u32 = version.parse().map_err(|_| {
                corrupt(format!("stored type {versioned} has a non-integer version"))
            })?;
            let value: Value = serde_json::from_str(&data)
                .map_err(|error| corrupt(format!("{versioned} data is not valid JSON: {error}")))?;
            out.push((kind.to_owned(), version, value));
        }
        Ok(out)
    }

    /// The earliest seed's raw `data` column.
    ///
    /// Shared by [`Self::placement`] and [`Self::parent`] so "earliest seed
    /// wins" is stated once. Two copies of that rule is two places for it to
    /// stop being true.
    fn seed_data(&self, session: &SessionId) -> Result<String, SessionStoreError> {
        let raw: Option<String> = self
            .conn
            .query_row(
                "SELECT data FROM event
                 WHERE aggregate_family = ?1 AND aggregate_id = ?2 AND type = ?3
                 ORDER BY seq LIMIT 1",
                rusqlite::params![SESSION_FAMILY, session.as_str(), CREATED],
                |row| row.get(0),
            )
            .map(Some)
            .or_else(|error| {
                if error == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(SessionStoreError::Store {
                        session: session.as_str().to_owned(),
                        source: error,
                    })
                }
            })?;
        raw.ok_or_else(|| SessionStoreError::Unknown {
            session: session.as_str().to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_store::{AggregateId, AppendInput, EventStore};
    use grapt_types::{EventId, EventVersion};
    use serde_json::json;

    fn seed(store: &mut EventStore, id: &str, session: &SessionId, info: Value) {
        let event: EventId = match id.parse() {
            Ok(event) => event,
            Err(error) => panic!("event id must parse: {error}"),
        };
        let version = match EventVersion::new(1) {
            Some(version) => version,
            None => panic!("version 1 must exist"),
        };
        let data = json!({ "sessionID": session.as_str(), "info": info });
        // Test setup elects explicitly; the append path never does.
        let aggregate = AggregateId::Session(session.clone());
        let writer = match store
            .elect_writer(&aggregate)
            .and_then(|_| store.fencing_token(&aggregate))
        {
            Ok(writer) => writer,
            Err(error) => panic!("seed must be able to elect a writer: {error}"),
        };
        let appended = store.append(
            AppendInput {
                id: &event,
                aggregate,
                writer,
                kind_name: "session.created",
                version,
                created_ms: 1_786_060_800_000,
                data: &data,
            },
            &mut [],
        );
        if let Err(error) = appended {
            panic!("seed append must succeed: {error}");
        }
    }

    fn session(id: &str) -> SessionId {
        match id.parse::<SessionId>() {
            Ok(session) => session,
            Err(error) => panic!("session id must parse: {error}"),
        }
    }

    #[test]
    fn a_seeded_session_exposes_its_canonical_placement() -> Result<(), Box<dyn std::error::Error>>
    {
        let mut store = EventStore::open_in_memory()?;
        let ses = session("ses_place");
        // A host-independent directory on purpose: the separator law belongs to
        // `LocationRef` and is witnessed against the oracle's `canonicalKey`
        // there, per platform. What this witness owns is that the seed's two
        // placement fields are read and canonicalized, not defaulted.
        seed(
            &mut store,
            "evt_place",
            &ses,
            json!({"directory": "/work/app", "workspaceID": "ws_one"}),
        );

        let view = SessionStore::over(&store);
        assert!(view.exists(&ses)?);
        let placement = view.placement(&ses)?;
        assert_eq!(placement.directory(), "/work/app");
        assert_eq!(placement.workspace().map(|w| w.as_str()), Some("ws_one"));
        Ok(())
    }

    #[test]
    fn an_absent_workspace_is_implicit_local_placement() -> Result<(), Box<dyn std::error::Error>> {
        let mut store = EventStore::open_in_memory()?;
        let ses = session("ses_implicit");
        seed(
            &mut store,
            "evt_implicit",
            &ses,
            json!({"directory": "/work/app"}),
        );
        let placement = SessionStore::over(&store).placement(&ses)?;
        assert_eq!(
            placement.workspace(),
            None,
            "an omitted key is the oracle's optional-key case: no workspace"
        );
        Ok(())
    }

    #[test]
    fn an_explicit_null_workspace_is_corruption_not_implicit_local(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The distinction the constitution requires, and the one the oracle's
        // `optional(WorkspaceID)` enforces: the key may be ABSENT, but a
        // present `null` decodes as no workspace id at all. Accepting it would
        // place the Session locally on the strength of a value that failed to
        // decode.
        let mut store = EventStore::open_in_memory()?;
        let ses = session("ses_null");
        seed(
            &mut store,
            "evt_null",
            &ses,
            json!({"directory": "/work/app", "workspaceID": null}),
        );
        match SessionStore::over(&store).placement(&ses) {
            Err(SessionStoreError::Corrupt { reason, .. }) => {
                assert!(reason.contains("workspaceID"), "reason: {reason}");
            }
            other => panic!("expected typed corruption, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn an_unseeded_session_is_unknown_not_corrupt() -> Result<(), Box<dyn std::error::Error>> {
        let store = EventStore::open_in_memory()?;
        let ses = session("ses_ghost");
        let view = SessionStore::over(&store);
        assert!(!view.exists(&ses)?);
        assert!(matches!(
            view.placement(&ses),
            Err(SessionStoreError::Unknown { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_seed_without_a_directory_is_corruption_not_a_default(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = EventStore::open_in_memory()?;
        let ses = session("ses_nodir");
        seed(&mut store, "evt_nodir", &ses, json!({"title": "no place"}));
        let view = SessionStore::over(&store);
        assert!(
            view.exists(&ses)?,
            "the Session exists; only its placement is unreadable"
        );
        match view.placement(&ses) {
            Err(SessionStoreError::Corrupt { reason, .. }) => {
                assert!(reason.contains("info.directory"), "reason: {reason}");
            }
            other => panic!("expected corruption, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn a_relative_seed_directory_refuses_rather_than_placing_the_session(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut store = EventStore::open_in_memory()?;
        let ses = session("ses_rel");
        seed(
            &mut store,
            "evt_rel",
            &ses,
            json!({"directory": "work/app"}),
        );
        assert!(matches!(
            SessionStore::over(&store).placement(&ses),
            Err(SessionStoreError::Corrupt { .. })
        ));
        Ok(())
    }

    #[test]
    fn a_store_read_failure_is_never_an_unknown_session() -> Result<(), Box<dyn std::error::Error>>
    {
        // Interrupt the actual read while preserving the governed schema.
        let mut store = EventStore::open_in_memory()?;
        store.progress_handler(1, Some(|| true))?;
        let ses = session("ses_broken");
        let view = SessionStore::over(&store);
        assert!(matches!(
            view.exists(&ses),
            Err(SessionStoreError::Store { .. })
        ));
        assert!(matches!(
            view.placement(&ses),
            Err(SessionStoreError::Store { .. })
        ));
        Ok(())
    }
}

#[cfg(test)]
#[path = "store/read_tests.rs"]
mod read_tests;
