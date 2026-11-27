//! The closed durable event-kind vocabulary.
//!
//! A stored v1 event's `type` column is the versioned string `name.version`
//! (`versionedType`, oracle `packages/schema/src/event.ts:178-180`); the
//! version is a dotted integer suffix, never a separate column. This module
//! replaces the raw-`String` kind that predated the release order with a
//! closed enum, so "exhaustive version decoding" is a compiler fact rather
//! than a convention.
//!
//! # What is in the set, and where each entry comes from
//!
//! Three provenances, each measured rather than assumed:
//!
//! * **Manifest** — the 57 versioned keys `EventManifest.Durable` declares at
//!   the frozen oracle `42e4888cc0` (7 SessionV1 legacy, 37 SessionEvent v1,
//!   5 SessionEvent v2, 4 permission, 4 form; all on aggregate `sessionID`).
//! * **HistoricalLogOnly** — `session.tool.success.1` (8,146 rows) and
//!   `session.tool.failed.1` (200 rows): live in the frozen clone but absent
//!   from today's manifest, so the oracle's own read path silently skips
//!   them. Import must not: they are real durable history. Row counts are
//!   from a read-only `GROUP BY type` census of the sanitized frozen clone
//!   (the 2026-08-08 sanitized clone, SHA-256
//!   `6d4cf224e3129d73babaafd75e0f1e635e4381119f29af99f3e43da9a4536d71` per
//!   `DB_EVIDENCE_D1_2026-08-08.md`), run 2026-08-09 during the F2A
//!   pre-flight.
//! * **HistoricalDeleted** — `session.tool.progress.1`: retroactively
//!   `DELETE`d by migration `20260722011141`, zero rows in the shipped clone,
//!   but legal in any database that stopped migrating earlier.
//!
//! The complete distinct-type census of the same clone (69,740 events) is
//! 42 strings: 40 manifest keys plus the two `HistoricalLogOnly` kinds. The
//! 17 manifest keys with zero occurrences (the whole SessionV1 family among
//! them) still decode — a different database may hold them. The vocabulary
//! is additionally pinned against the independent in-tree fixture
//! `tests/fixtures/durable-kinds.tsv` in both directions.
//!
//! # The `session.deleted` collision
//!
//! `session.deleted` exists at v1 (SessionV1, payload embeds a full
//! `SessionInfo`) AND v2 (SessionEvent, payload `{sessionID}` only) with
//! incompatible payloads. Decode therefore keys on the exact
//! `(name, version)` pair — the stored map key — never the bare tag. The
//! oracle's `Event.latest` picking the higher version is a PUBLIC-surface
//! behavior only and must not leak into import.

use grapt_types::EventVersion;
use std::fmt;

/// Where a kind's legitimacy comes from. Provenance, not lifecycle: a
/// historical kind decodes exactly like a manifest kind; the label exists so
/// a consumer can refuse to WRITE one (no new v1 write of any provenance is
/// legal) and so audits can say which law admitted each key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KindProvenance {
    /// Declared by `EventManifest.Durable` at the frozen oracle.
    Manifest,
    /// Present in the frozen clone's log but absent from today's manifest;
    /// the oracle read path skips it, import does not.
    HistoricalLogOnly,
    /// Retroactively deleted from the shipped database by a migration;
    /// legal in databases that stopped migrating earlier.
    HistoricalDeleted,
    /// Declared by the Rust reconstruction's own ratified law, not by the
    /// frozen oracle manifest.
    ///
    /// This is the first provenance that is not backward-facing, and it is a
    /// DISTINCT label on purpose. The other three all say "the oracle wrote
    /// this once"; labelling a Rust-native fact `Manifest` would claim the
    /// frozen manifest declared a key it never contained, and labelling it
    /// historical would claim it was retired. Both are false statements about
    /// where a key's legitimacy comes from, and provenance is exactly the
    /// field that answers that question.
    ///
    /// It also carries the envelope law: a `RustNative` kind rides
    /// [`crate::envelope::EnvelopeKind::RustSession`] and is refused by the
    /// `Legacy` constructor, so the two cannot be interchanged by accident.
    RustNative,
}

/// Why a stored `type` string is not a member of the closed vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KindError {
    /// The string has no `.N` positive-integer version suffix at all.
    Unversioned { raw: String },
    /// The name is not a known durable event name.
    UnknownName { name: String, version: u32 },
    /// The name is known, but not at this version. Failing closed here is
    /// load-bearing: falling back to a "nearest" known version would decode
    /// `session.deleted.1`'s twenty-field payload with `session.deleted.2`'s
    /// one-field contract, or invent versions no engine ever wrote.
    UnknownVersion { name: String, version: u32 },
}

impl fmt::Display for KindError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KindError::Unversioned { raw } => {
                write!(f, "type {raw:?} has no positive `.N` version suffix")
            }
            KindError::UnknownName { name, version } => {
                write!(f, "unknown durable event name {name:?} (version {version})")
            }
            KindError::UnknownVersion { name, version } => {
                write!(f, "known event name {name:?} has no version {version}")
            }
        }
    }
}

impl std::error::Error for KindError {}

macro_rules! event_kinds {
    ($( $variant:ident => ($name:literal, $version:literal, $provenance:ident) ),+ $(,)?) => {
        /// One member of the closed durable-event vocabulary: an exact
        /// `(name, version)` pair. Every consumer match is exhaustive —
        /// adding a variant breaks each one until someone decides what the
        /// new kind means there.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum EventKind {
            $( $variant, )+
        }

        impl EventKind {
    pub fn is_native_mail(self) -> bool { self == Self::MailPageRecordedV1 }

            /// Closed durable vocabulary, sized from the same declarations.
            pub const ALL: [EventKind; [$(stringify!($variant)),+].len()] = [ $( EventKind::$variant, )+ ];

            /// The bare event name (no version suffix).
            pub fn name(self) -> &'static str {
                match self {
                    $( EventKind::$variant => $name, )+
                }
            }

            /// The schema version.
            pub fn version(self) -> EventVersion {
                let raw = match self {
                    $( EventKind::$variant => $version, )+
                };
                // Every declared version literal is >= 1; the closed macro
                // input is the proof, and the test below re-proves it.
                EventVersion::new(raw).unwrap_or_else(|| unreachable!("declared version is zero"))
            }

            /// Where this kind's legitimacy comes from.
            pub fn provenance(self) -> KindProvenance {
                match self {
                    $( EventKind::$variant => KindProvenance::$provenance, )+
                }
            }

            /// The exact stored `type` string, `name.version`.
            pub fn as_str(self) -> &'static str {
                match self {
                    $( EventKind::$variant => concat!($name, ".", $version), )+
                }
            }

            fn from_pair(name: &str, version: u32) -> Result<Self, KindError> {
                match (name, version) {
                    $( ($name, $version) => Ok(EventKind::$variant), )+
                    _ => {
                        let known = [ $( $name, )+ ];
                        if known.contains(&name) {
                            Err(KindError::UnknownVersion {
                                name: name.to_owned(),
                                version,
                            })
                        } else {
                            Err(KindError::UnknownName {
                                name: name.to_owned(),
                                version,
                            })
                        }
                    }
                }
            }
        }
    };
}

event_kinds! {
    // SessionV1 legacy family (7). Zero rows in the shipped clone, live in
    // the manifest; other databases may hold them.
    SessionCreatedV1 => ("session.created", 1, Manifest),
    SessionUpdatedV1 => ("session.updated", 1, Manifest),
    SessionDeletedV1 => ("session.deleted", 1, Manifest),
    MessageUpdatedV1 => ("message.updated", 1, Manifest),
    MessageRemovedV1 => ("message.removed", 1, Manifest),
    MessagePartUpdatedV1 => ("message.part.updated", 1, Manifest),
    MessagePartRemovedV1 => ("message.part.removed", 1, Manifest),

    // SessionEvent v1 family (37).
    SessionAgentSelectedV1 => ("session.agent.selected", 1, Manifest),
    SessionPresetSelectedV1 => ("session.preset.selected", 1, Manifest),
    SessionModelSelectedV1 => ("session.model.selected", 1, Manifest),
    SessionSubagentModelSelectedV1 => ("session.subagent-model.selected", 1, Manifest),
    SessionSubagentModelClearedV1 => ("session.subagent-model.cleared", 1, Manifest),
    SessionMovedV1 => ("session.moved", 1, Manifest),
    SessionRenamedV1 => ("session.renamed", 1, Manifest),
    SessionInputPromotedV1 => ("session.input.promoted", 1, Manifest),
    SessionInputAdmittedV1 => ("session.input.admitted", 1, Manifest),
    SessionNativeWakeHeldV1 => ("session.input.native-wake-held", 1, RustNative),
    SessionNativeMailStateV1 => ("session.native-mail-state", 1, RustNative),
    SessionConfigBoundV1 => ("session.config.bound", 1, RustNative),
    SessionLaunchedV1 => ("session.launched", 1, RustNative),
    SessionExecutionStartedV1 => ("session.execution.started", 1, Manifest),
    SessionExecutionSucceededV1 => ("session.execution.succeeded", 1, Manifest),
    SessionExecutionFailedV1 => ("session.execution.failed", 1, Manifest),
    SessionExecutionInterruptedV1 => ("session.execution.interrupted", 1, Manifest),
    SessionSyntheticV1 => ("session.synthetic", 1, Manifest),
    SessionSkillActivatedV1 => ("session.skill.activated", 1, Manifest),
    SessionShellStartedV1 => ("session.shell.started", 1, Manifest),
    SessionShellEndedV1 => ("session.shell.ended", 1, Manifest),
    SessionStepStartedV1 => ("session.step.started", 1, Manifest),
    SessionStepEndedV1 => ("session.step.ended", 1, Manifest),
    // L4a. The counterpart of `session.step.failed.2`, and `RustNative` for
    // exactly the same reason: v1 of this name is a manifest key whose frozen
    // payload carries `finish` as a raw provider reason plus cost, tokens and
    // assistantMessageID, none of which this contract has. v2 is the
    // Rust-native durable Step SUCCESS terminal, carrying a positive
    // `StepNumber` and one `ProviderCompletionDisposition`. Until it landed
    // the kind was declared and unimplemented: a Step that SUCCEEDED wrote
    // nothing durable, while every failing Step wrote its terminal.
    SessionStepEndedV2 => ("session.step.ended", 2, RustNative),
    SessionStepEndedV3 => ("session.step.ended", 3, RustNative),
    SessionStepFailedV1 => ("session.step.failed", 1, Manifest),
    // R2b-1. `session.step.failed` is a frozen-oracle name, so v1 stays
    // `Manifest` and keeps every byte it has. v2 is the Rust-native durable
    // Step terminal carrying a typed `StepTerminalReason` instead of the v1
    // `{type, message}` prose that R2a removed from the runtime. Labelling it
    // `Manifest` would claim the frozen manifest declared a key it never
    // contained; decode keys on the exact (name, version) pair, so the two
    // versions can never answer for each other.
    SessionStepFailedV2 => ("session.step.failed", 2, RustNative),
    SessionStepFailedV3 => ("session.step.failed", 3, RustNative),
    SessionTextStartedV1 => ("session.text.started", 1, Manifest),
    SessionTextEndedV1 => ("session.text.ended", 1, Manifest),
    SessionReasoningStartedV1 => ("session.reasoning.started", 1, Manifest),
    SessionReasoningEndedV1 => ("session.reasoning.ended", 1, Manifest),
    SessionToolInputStartedV1 => ("session.tool.input.started", 1, Manifest),
    SessionToolInputStartedV2 => ("session.tool.input.started", 2, RustNative),
    SessionToolInputEndedV1 => ("session.tool.input.ended", 1, Manifest),
    SessionToolCalledV1 => ("session.tool.called", 1, Manifest),
    // New Rust-native exactly-once terminal. Historical success/failure kinds
    // remain readable and unchanged; this version owns the closed typed payload.
    SessionToolTerminalV1 => ("session.tool.terminal", 1, RustNative),
    SessionPatchReceiptV1 => ("session.patch.receipt", 1, RustNative),
    SessionTerminalObservedV1 => ("session.terminal.observed", 1, RustNative),
    SessionToolNestedStartedV1 => ("session.tool.nested.started", 1, RustNative),
    // DF0-SESSION. Three Rust-native NAMES, not new versions of oracle names.
    // The oracle spells assistant content as per-kind start/end pairs
    // (`session.text.*`, `session.reasoning.*`) plus `message.part.updated.1`,
    // which is the parallel-per-kind-array shape the supersession matrix
    // corrects: it "may lose interleaving or pairing". Minting v2 of those
    // names would carry the corrected-away shape forward under a new number.
    // `session.content.appended.1` is one ordered sequence with an exact
    // `ContentOrdinal`, so the correction is in the vocabulary rather than in a
    // consumer's reassembly. `session.usage.observed.1` is likewise separate
    // from the oracle's `session.usage.recorded.1`, whose payload has no
    // observation phase or reported-versus-estimated provenance to carry.
    SessionAssistantMessageOpenedV1 => ("session.assistant-message.opened", 1, RustNative),
    SessionAssistantMessageOpenedV2 => ("session.assistant-message.opened", 2, RustNative),
    SessionContentBlockOpenedV1 => ("session.content-block.opened", 1, RustNative),
    SessionContentAppendedV1 => ("session.content.appended", 1, RustNative),
    // CLAIM-2: the open text block's text so far, so a process loss keeps what
    // was already streamed. Recovery promotes the newest one; it is never a
    // content item by itself.
    SessionContentCheckpointedV1 => ("session.content.checkpointed", 1, RustNative),
    SessionUsageObservedV1 => ("session.usage.observed", 1, RustNative),
    // PC2. v1 remains readable for shipped observations whose single input
    // counter is route-ambiguous. Only v2 can carry separate inclusive/fresh
    // input and provenance on each leaf, so every new writer uses this pair.
    SessionUsageObservedV2 => ("session.usage.observed", 2, RustNative),
    SessionRetryScheduledV1 => ("session.retry.scheduled", 1, Manifest),
    SessionCompactionAdmittedV1 => ("session.compaction.admitted", 1, Manifest),
    SessionCompactionStartedV1 => ("session.compaction.started", 1, Manifest),
    SessionCompactionEndedV1 => ("session.compaction.ended", 1, Manifest),
    // The Rust-native compaction descriptor. SAME semantic boundary as v1 —
    // the point at which a compaction settles — carried at a version whose
    // payload can hold the eleven ratified descriptor coordinates. v1 remains
    // the frozen oracle shape with its own strict decoder and fold arm; the
    // two versions are separate rows here because decode keys on the exact
    // (name, version) pair, which is what keeps them from ever sharing a
    // payload contract.
    SessionCompactionEndedV2 => ("session.compaction.ended", 2, RustNative),
    SessionCompactionCallStartedV1 => ("session.compaction.call.started", 1, RustNative),
    SessionCompactionCallUsageV1 => ("session.compaction.call.usage", 1, RustNative),
    SessionCompactionCallEndedV1 => ("session.compaction.call.ended", 1, RustNative),
    SessionRequestCacheV1 => ("session.request.cache", 1, RustNative),
    SessionRequestContextV1 => ("session.request.context", 1, RustNative),
    MailPageRecordedV1 => ("mail.page.recorded", 1, RustNative),
    SessionMailPresentedV1 => ("session.mail.presented", 1, RustNative),
    SessionOperationMailPresentedV1 => ("session.subagent-mail.presented", 1, RustNative),
    // A user-facing notice about a turn (today: MCP servers it could not
    // use). Product-authored text only, and never provider history.
    SessionNoticeRecordedV1 => ("session.notice.recorded", 1, RustNative),
    SessionStatusStartedV1 => ("session.status.started", 1, RustNative),
    SessionStatusChangedV1 => ("session.status.changed", 1, RustNative),
    SessionCompactionFailedV1 => ("session.compaction.failed", 1, Manifest),
    SessionRevertStagedV1 => ("session.revert.staged", 1, Manifest),
    SessionRevertClearedV1 => ("session.revert.cleared", 1, Manifest),
    SessionRevertCommittedV1 => ("session.revert.committed", 1, Manifest),
    SessionModelAttemptStartedV1 => ("session.model-attempt.started", 1, Manifest),
    SessionUsageRecordedV1 => ("session.usage.recorded", 1, Manifest),

    // SessionEvent v2 family (5).
    SessionDeletedV2 => ("session.deleted", 2, Manifest),
    SessionForkedV2 => ("session.forked", 2, Manifest),
    SessionInstructionsUpdatedV2 => ("session.instructions.updated", 2, Manifest),
    SessionToolSuccessV2 => ("session.tool.success", 2, Manifest),
    SessionToolFailedV2 => ("session.tool.failed", 2, Manifest),

    // Permission family (6).
    PermissionAskedV1 => ("permission.asked", 1, Manifest),
    // Rust-native replay-complete admission. V1 remains readable, while v2
    // carries the HTTP owner, client, agent, project, resource and source
    // coordinates needed to rebuild every permission projection.
    PermissionAskedV2 => ("permission.asked", 2, RustNative),
    PermissionRepliedV1 => ("permission.replied", 1, Manifest),
    // Rust-native authority receipt. The v1 oracle event remains readable,
    // while v2 carries the answering principal/client/request/nonce tuple.
    PermissionRepliedV2 => ("permission.replied", 2, RustNative),
    PermissionSuspendedV1 => ("permission.suspended", 1, Manifest),
    PermissionCancelledV1 => ("permission.cancelled", 1, Manifest),
    PermissionRuleRevokedV1 => ("permission.rule.revoked", 1, RustNative),

    // Form family (4).
    FormCreatedV1 => ("form.created", 1, Manifest),
    FormCreatedV2 => ("form.created", 2, RustNative),
    FormSuspendedV2 => ("form.suspended", 2, RustNative),
    FormRepliedV2 => ("form.replied", 2, RustNative),
    FormCancelledV2 => ("form.cancelled", 2, RustNative),
    FormRepliedV1 => ("form.replied", 1, Manifest),
    FormCancelledV1 => ("form.cancelled", 1, Manifest),
    FormSuspendedV1 => ("form.suspended", 1, Manifest),

    // Historical kinds the log contains but today's manifest does not.
    SessionToolSuccessV1 => ("session.tool.success", 1, HistoricalLogOnly),
    SessionToolFailedV1 => ("session.tool.failed", 1, HistoricalLogOnly),
    SessionToolProgressV1 => ("session.tool.progress", 1, HistoricalDeleted),
}

impl EventKind {
    pub fn is_global_form(self) -> bool {
        matches!(
            self,
            Self::FormCreatedV2
                | Self::FormSuspendedV2
                | Self::FormRepliedV2
                | Self::FormCancelledV2
        )
    }

    /// Parses a stored `type` string into the closed vocabulary.
    ///
    /// The version is the final `.N` segment with `N >= 1`, exactly the
    /// oracle's `versionedType` inverse. Everything else fails closed with a
    /// typed reason; there is no passthrough representation for an unknown
    /// kind, so "decode it anyway" is unrepresentable here. (Whether an
    /// unknown kind aborts or is skipped-with-cursor-advance is the CALLER's
    /// posture — see `legacy::decode` — but both postures start from this
    /// same refusal.)
    pub fn parse(raw: &str) -> Result<Self, KindError> {
        let (name, version) = raw
            .rsplit_once('.')
            .and_then(|(name, suffix)| {
                let version: u32 = suffix.parse().ok()?;
                if version == 0 || name.is_empty() {
                    return None;
                }
                Some((name, version))
            })
            .ok_or_else(|| KindError::Unversioned {
                raw: raw.to_owned(),
            })?;
        Self::from_pair(name, version)
    }
}

impl fmt::Display for EventKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_round_trips_through_its_stored_string() {
        for kind in EventKind::ALL {
            assert_eq!(
                EventKind::parse(kind.as_str()),
                Ok(kind),
                "round trip failed for {kind}"
            );
            assert!(kind.version().value() >= 1);
        }
    }

    #[test]
    fn legacy_provenance_stays_stable_when_native_vocabulary_grows() {
        let manifest = EventKind::ALL
            .iter()
            .filter(|k| k.provenance() == KindProvenance::Manifest)
            .count();
        let rust_native = EventKind::ALL
            .iter()
            .filter(|k| k.provenance() == KindProvenance::RustNative)
            .count();
        let historical = EventKind::ALL.len() - manifest - rust_native;
        assert_eq!((manifest, historical), (57, 3));

        // The versioned Rust-native boundaries are DISTINCT from their
        // manifest v1 predecessors. Collapsing either half would let one
        // payload contract answer for both.
        assert_eq!(
            EventKind::SessionCompactionEndedV1.provenance(),
            KindProvenance::Manifest
        );
        assert_eq!(
            EventKind::SessionStepFailedV1.provenance(),
            KindProvenance::Manifest
        );
        assert_eq!(
            EventKind::SessionStepFailedV2.provenance(),
            KindProvenance::RustNative
        );
        assert_eq!(
            EventKind::SessionStepEndedV1.provenance(),
            KindProvenance::Manifest
        );
        assert_eq!(
            EventKind::SessionStepEndedV2.provenance(),
            KindProvenance::RustNative
        );
        assert_eq!(
            EventKind::parse("session.step.ended.2"),
            Ok(EventKind::SessionStepEndedV2)
        );
        assert_eq!(
            EventKind::SessionCompactionEndedV2.provenance(),
            KindProvenance::RustNative
        );
        assert_eq!(
            EventKind::parse("session.compaction.ended.2"),
            Ok(EventKind::SessionCompactionEndedV2)
        );
        // v3 is still refused, and as UnknownVersion rather than UnknownName,
        // because the name is now known at two versions.
        assert_eq!(
            EventKind::parse("session.compaction.ended.3"),
            Err(KindError::UnknownVersion {
                name: "session.compaction.ended".to_owned(),
                version: 3,
            })
        );

        // The stored strings are pairwise distinct — a duplicate entry would
        // make `parse` unreachable for one of them.
        let mut strings: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
        strings.sort_unstable();
        strings.dedup();
        assert_eq!(strings.len(), EventKind::ALL.len());
    }

    #[test]
    fn an_unknown_name_fails_closed() {
        assert_eq!(
            EventKind::parse("session.imaginary.1"),
            Err(KindError::UnknownName {
                name: "session.imaginary".into(),
                version: 1
            })
        );
    }

    #[test]
    fn a_known_name_at_an_unknown_version_fails_closed_not_nearest() {
        // W4's sharp edge: `session.deleted` exists at 1 AND 2; version 3
        // must refuse rather than resolve to either neighbour.
        assert_eq!(
            EventKind::parse("session.deleted.3"),
            Err(KindError::UnknownVersion {
                name: "session.deleted".into(),
                version: 3
            })
        );
        // And the collision pair itself resolves by exact pair, never tag.
        assert_eq!(
            EventKind::parse("session.deleted.1"),
            Ok(EventKind::SessionDeletedV1)
        );
        assert_eq!(
            EventKind::parse("session.deleted.2"),
            Ok(EventKind::SessionDeletedV2)
        );
        assert_eq!(
            EventKind::parse("session.tool.success.1"),
            Ok(EventKind::SessionToolSuccessV1)
        );
    }

    #[test]
    fn versionless_zero_and_empty_forms_are_unversioned() {
        for raw in ["session.created", "session.created.0", ".1", ""] {
            assert!(
                matches!(EventKind::parse(raw), Err(KindError::Unversioned { .. })),
                "{raw:?} must be Unversioned"
            );
        }
    }

    /// Exhaustiveness probe in the spirit of types probe #5: this match has
    /// no wildcard, so adding a variant refuses to compile until every
    /// consumer decides what the new kind means.
    #[test]
    fn provenance_match_is_exhaustive_without_wildcard() {
        for kind in EventKind::ALL {
            let _ = match kind.provenance() {
                KindProvenance::Manifest => 0,
                KindProvenance::HistoricalLogOnly => 1,
                KindProvenance::HistoricalDeleted => 2,
                KindProvenance::RustNative => 3,
            };
        }
    }
}
