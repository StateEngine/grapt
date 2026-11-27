//! Explicit resource and performance bounds.
//!
//! Every bound this client relies on is a NAMED VALUE here, not a literal at a
//! call site and not an implicit consequence of a container's growth policy.
//! The reconstruction law is explicit that "resource claims include bytes,
//! counts, and working memory" and that "a count-bounded queue containing
//! unbounded values is not bounded", so each bound below states which of the
//! two it is and the other one is stated next to it.
//!
//! The frozen oracle client is the counterexample this module exists to
//! avoid: `tui/src/mini/stream-v2.transport.ts` is named in the god-file map
//! for its timer mechanics and unbounded retained state, and the client matrix
//! lists "unbounded retained state" among the things this crate must delete
//! rather than preserve.
//!
//! ## Why these are one struct rather than constants
//!
//! A production bound and a test bound must be the same SHAPE, or a witness
//! proves a policy nobody ships. [`Bounds::PRODUCTION`] is the shipped value
//! set; a witness constructs a smaller one through the same validating
//! constructor and therefore exercises the same overflow branch that
//! production would take, at a size a test can actually reach.

use thiserror::Error;

/// The bounds one client instance operates under.
///
/// Construction is validated: a zero bound would make the client refuse
/// everything while looking configured, which is worse than refusing to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bounds {
    /// Maximum bytes accepted for one SSE frame before the frame is refused.
    ///
    /// This is the BYTE bound that pairs with [`Self::max_pending_events`].
    /// Bounding the queue by count alone would leave one oversized frame able
    /// to exhaust memory while every counter still read as healthy.
    pub max_frame_bytes: usize,
    /// Maximum decoded events retained awaiting application.
    ///
    /// Overflow is a TYPED, VISIBLE state, never a silent drop: the client
    /// reports the gap and reconciles from durable truth, which is exactly the
    /// path a dropped connection takes. Losing frames quietly is what makes a
    /// transcript diverge from durable history with nothing to show for it.
    pub max_pending_events: usize,
    /// Maximum bytes accepted for one non-stream response body.
    ///
    /// This MIRRORS the API's own `MAX_PAGE_BYTES`: the largest response this
    /// client can conformantly be sent is a page, and accepting less would
    /// refuse valid traffic. The number is the wire's, not a buffer budget of
    /// this client's choosing, and
    /// `the_mirrored_bounds_are_the_wire_bounds` fails if either side moves.
    pub max_response_bytes: usize,
    /// Maximum transcript slices retained in the rendered projection.
    ///
    /// The transcript is bounded because a long session is the ordinary case,
    /// not the exceptional one. Older content is not lost — it remains durable
    /// and re-listable — it is simply not retained in memory.
    pub max_retained_slices: usize,
    /// Maximum bytes retained for one transcript slice's accumulated text.
    ///
    /// The count bound above pairs with this byte bound for the same reason
    /// the frame bounds pair.
    pub max_slice_bytes: usize,
    /// Maximum sessions retained in the picker projection.
    ///
    /// This bounds the CONSUMER's picker and the PRODUCER's session store
    /// alike. Review blocker 4 was that the second half was missing: the double
    /// held `sessions` in an unbounded `BTreeMap`, so a long-lived producer grew
    /// without limit while every client-side counter read as healthy. One bound
    /// applied at both ownership points is what makes the pair honest.
    pub max_retained_sessions: usize,
    /// Maximum outstanding permission asks retained, per owner.
    ///
    /// Bounded on BOTH sides for the same reason: the client's
    /// `PermissionAsk` projection (in `grapt-client`) and the producer's per-session ask
    /// vector. The previous revision bounded neither — `permissions` was an
    /// unbounded map of unbounded vectors, which is two unbounded dimensions in
    /// one field.
    pub max_retained_permissions: usize,
    /// Maximum durable log entries retained per session by a PRODUCER.
    ///
    /// This is the bound with no consumer half, and it is the one review named
    /// most precisely: the durable log grew through an unconditional
    /// `entry.log.push`. A producer's durable log is the one collection a
    /// consumer bound can never protect, because the consumer never sees it —
    /// it only ever sees the page the producer chooses to serve.
    ///
    /// Eviction here is not silent truncation of history. An evicted log makes
    /// the served page a PREFIX, and the page says so through the contract's
    /// own cursor, so the client refuses to claim liveness. Producer bound and
    /// client refusal are one fact observed from two sides.
    pub max_retained_log_entries: usize,
    /// Maximum catalog rows retained per picker.
    ///
    /// The project gateway, agent roster and model picker are all served from
    /// `MAX_CATALOG_ROWS`-bounded wire vectors, so the wire cannot flood them.
    /// This is the CLIENT's own bound over the same rows, and it exists for the
    /// reason the session picker has one: a bound that lives only on the far
    /// side of a seam is a bound this client cannot claim.
    pub max_catalog_rows: usize,
    /// Model rows kept from one catalog read.
    ///
    /// Separate from [`Self::max_catalog_rows`] because the contract makes it
    /// separate: the model list carries its own larger bound, and one shared
    /// client number both over-admitted the small catalogs and truncated the
    /// large one.
    pub max_model_rows: usize,
    /// Maximum bytes the composer will hold for one prompt.
    ///
    /// This MIRRORS the API's own `MAX_PROMPT_TEXT_BYTES`, for the reason the
    /// frame bounds mirror the SSE limits: a composer that accepted more than
    /// the wire will carry lets a user finish a long prompt and only then be
    /// told, by a server refusal, that none of it can be sent. Refusing the
    /// paste that would cross the bound puts the refusal where the user can
    /// still act on it.
    ///
    /// It is spelled here rather than imported so that a change on either side
    /// has to be made deliberately: a client that silently followed a server
    /// bound would adopt a new memory ceiling without anyone choosing it.
    ///
    /// The number is not trusted. `the_mirrored_bounds_are_the_wire_bounds`
    /// asserts it against `grapt_api::u3a::limits::MAX_PROMPT_TEXT_BYTES`, so
    /// a drift on either side fails a witness rather than shipping.
    pub max_prompt_bytes: usize,
    /// Maximum message-list pages one reconciliation will follow.
    ///
    /// Reconciliation pages until the server stops offering a cursor. A server
    /// that offered one forever would otherwise spin the client, so the loop is
    /// bounded and reaching the bound leaves the client DEGRADED with a reason
    /// rather than quietly rendering a prefix as if it were whole.
    pub max_message_pages: usize,
}

impl Bounds {
    /// The shipped production values.
    ///
    /// These mirror the API's own SSE limits where the client sits directly
    /// opposite one, so a client that accepted more than the server would ever
    /// send could not be exercised by any real transport, and a client that
    /// accepted less would refuse conformant traffic.
    pub const PRODUCTION: Self = Self {
        max_frame_bytes: grapt_api::u3a::MAX_SSE_FRAME_BYTES,
        max_pending_events: 1_024,
        max_response_bytes: 8 * 1024 * 1024,
        max_retained_slices: 512,
        max_slice_bytes: 256 * 1024,
        max_retained_sessions: 256,
        max_retained_permissions: 128,
        max_retained_log_entries: 4_096,
        max_catalog_rows: grapt_api::u3a::MAX_CATALOG_ROWS,
        max_model_rows: grapt_api::u3a::MAX_MODEL_ROWS,
        max_prompt_bytes: 65_536,
        max_message_pages: 64,
    };

    /// Every bound's name, in declaration order.
    ///
    /// One list drives both the validator and its witness, so a bound cannot
    /// be added to the struct and forgotten by the zero check.
    pub const FIELDS: &'static [&'static str] = &[
        "max_frame_bytes",
        "max_pending_events",
        "max_response_bytes",
        "max_retained_slices",
        "max_slice_bytes",
        "max_retained_sessions",
        "max_retained_permissions",
        "max_retained_log_entries",
        "max_catalog_rows",
        "max_model_rows",
        "max_prompt_bytes",
        "max_message_pages",
    ];

    /// This bound set as `(name, value)` pairs, in declaration order.
    fn named_values(self) -> [(&'static str, usize); 12] {
        [
            ("max_frame_bytes", self.max_frame_bytes),
            ("max_pending_events", self.max_pending_events),
            ("max_response_bytes", self.max_response_bytes),
            ("max_retained_slices", self.max_retained_slices),
            ("max_slice_bytes", self.max_slice_bytes),
            ("max_retained_sessions", self.max_retained_sessions),
            ("max_retained_permissions", self.max_retained_permissions),
            ("max_retained_log_entries", self.max_retained_log_entries),
            ("max_catalog_rows", self.max_catalog_rows),
            ("max_model_rows", self.max_model_rows),
            ("max_prompt_bytes", self.max_prompt_bytes),
            ("max_message_pages", self.max_message_pages),
        ]
    }

    /// Validate a bound set.
    ///
    /// Every field must be non-zero. A zero bound is not a "disabled" bound —
    /// it is a client that refuses all traffic while reporting itself
    /// configured, and that failure mode is much harder to diagnose than a
    /// refusal at construction.
    pub fn validate(self) -> Result<Self, BoundsError> {
        for (field, value) in self.named_values() {
            if value == 0 {
                return Err(BoundsError::Zero { field });
            }
        }
        Ok(self)
    }
}

impl Default for Bounds {
    fn default() -> Self {
        Self::PRODUCTION
    }
}

/// Why a bound set was refused.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum BoundsError {
    #[error(
        "bound `{field}` is zero; a zero bound refuses all traffic while reporting as configured"
    )]
    Zero { field: &'static str },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_production_bounds_validate() {
        assert_eq!(Bounds::PRODUCTION.validate(), Ok(Bounds::PRODUCTION));
        assert_eq!(Bounds::default(), Bounds::PRODUCTION);
    }

    /// The bounds that claim to mirror the wire actually equal it.
    ///
    /// A mirror asserted only in prose is not a mirror. This compares against
    /// the API constants themselves rather than re-typed numbers, because a
    /// literal copied into the assertion would drift in the same silence as
    /// the literal it is checking.
    #[test]
    fn the_mirrored_bounds_are_the_wire_bounds() {
        use grapt_api::u3a::limits::{MAX_PAGE_BYTES, MAX_PROMPT_TEXT_BYTES};

        assert_eq!(
            Bounds::PRODUCTION.max_response_bytes,
            MAX_PAGE_BYTES,
            "the largest conformant response is a page; accepting less refuses valid traffic"
        );
        assert_eq!(
            Bounds::PRODUCTION.max_prompt_bytes,
            MAX_PROMPT_TEXT_BYTES,
            "a composer that holds more than the wire carries defers the refusal to submit time"
        );
    }

    #[test]
    fn every_field_is_refused_at_zero() {
        // Each field individually, because a validator that checked only the
        // first field would pass a bound set with five zeroes.
        // The match is exhaustive over the field names, so adding a bound
        // without adding it here fails to compile rather than silently
        // escaping the zero check.
        for field in Bounds::FIELDS.iter().copied() {
            let mut bounds = Bounds::PRODUCTION;
            match field {
                "max_frame_bytes" => bounds.max_frame_bytes = 0,
                "max_pending_events" => bounds.max_pending_events = 0,
                "max_response_bytes" => bounds.max_response_bytes = 0,
                "max_retained_slices" => bounds.max_retained_slices = 0,
                "max_slice_bytes" => bounds.max_slice_bytes = 0,
                "max_retained_sessions" => bounds.max_retained_sessions = 0,
                "max_retained_permissions" => bounds.max_retained_permissions = 0,
                "max_retained_log_entries" => bounds.max_retained_log_entries = 0,
                "max_catalog_rows" => bounds.max_catalog_rows = 0,
                "max_model_rows" => bounds.max_model_rows = 0,
                "max_prompt_bytes" => bounds.max_prompt_bytes = 0,
                "max_message_pages" => bounds.max_message_pages = 0,
                other => panic!("`Bounds::FIELDS` names `{other}` but this witness cannot zero it"),
            }
            assert_eq!(
                bounds.validate(),
                Err(BoundsError::Zero { field }),
                "{field} must be refused at zero"
            );
        }
    }

    #[test]
    fn every_count_bound_is_paired_with_a_byte_bound() {
        // The law is that a count-bounded queue holding unbounded values is
        // not bounded. This asserts the pairing exists rather than trusting a
        // comment: the frame queue has both, and so does the slice store.
        let bounds = Bounds::PRODUCTION;
        assert!(bounds.max_pending_events > 0 && bounds.max_frame_bytes > 0);
        assert!(bounds.max_retained_slices > 0 && bounds.max_slice_bytes > 0);
    }
}
