//! Immutable capped fork lineage.
//!
//! Law §4.1 and the supersession-matrix `Fork` row: a fork inherits parent
//! history by immutable lineage reference capped at an exact parent
//! sequence/boundary. It never copies materialized history into new canonical
//! facts, and parent changes after the cap do not enter the child.
//!
//! This module owns that law as pure values. It deliberately owns **no** I/O
//! seam: it cannot read an event table, and it does not describe how a
//! consumer should find the next boundary. A consumer walks upward by loading
//! one [`ForkBoundary`] at a time and handing it to [`ForkLineageBuilder`];
//! the cap arithmetic, the cycle refusal and the segment ordering are decided
//! here, where they can be witnessed without a database.
//!
//! The three coordinates that must never be confused:
//!
//! * a [`ParentSeq`] is a boundary on the **parent**, and `-1` is a real
//!   frozen value meaning the boundary precedes every parent event;
//! * a [`LineageCap`] is what a walk applies when reading **one** aggregate,
//!   and `Own` is the uncapped case that keeps a session's own history its own
//!   truth rather than something inherited;
//! * each cap belongs to one aggregate. Crossing a boundary uses that
//!   boundary's parent cap, never a numeric comparison with child positions.

use std::collections::HashSet;
use std::fmt;

use crate::event::EventSeq;
use crate::session::SessionId;

/// How much of a parent aggregate a fork inherits.
///
/// The frozen `SessionEvent.Forked` v2 shape carries `parentSeq` as an integer
/// at least `-1`. `-1` is not "absent" and not an error: it means the fork
/// boundary precedes the parent's first event, so nothing is inherited. Keeping
/// it a named variant stops that sentinel from being read as a sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ParentSeq {
    /// The boundary precedes every parent event. Nothing is inherited.
    ///
    /// Declared first so the derived `Ord` agrees with the raw integer order:
    /// `Nothing` is tighter than every `Through`, exactly as `-1 < 0`.
    Nothing,
    /// Inherit the parent through this sequence, inclusive.
    Through(EventSeq),
}

impl ParentSeq {
    /// The exact frozen encoding of [`ParentSeq::Nothing`].
    pub const NOTHING_RAW: i64 = -1;

    /// Decode the frozen wire integer.
    ///
    /// Returns `None` below the floor. The floor is the only arithmetic law
    /// here: a decoder that accepted `-2` would silently produce a cap that
    /// orders below "inherit nothing" and could never be reached again.
    pub fn from_raw(raw: i64) -> Option<Self> {
        match raw {
            Self::NOTHING_RAW => Some(Self::Nothing),
            raw => u64::try_from(raw)
                .ok()
                .map(|seq| Self::Through(EventSeq::new(seq))),
        }
    }

    /// The frozen wire integer.
    pub fn as_raw(self) -> i64 {
        match self {
            Self::Nothing => Self::NOTHING_RAW,
            // `EventSeq` is a u64 and a real aggregate sequence never
            // approaches i64::MAX, but saturating is still the honest lowering:
            // it can only ever narrow, never widen, an inherited range.
            Self::Through(seq) => i64::try_from(seq.value()).unwrap_or(i64::MAX),
        }
    }

    /// The inherited sequence, if any is inherited at all.
    pub fn through(self) -> Option<EventSeq> {
        match self {
            Self::Nothing => None,
            Self::Through(seq) => Some(seq),
        }
    }
}

/// What a lineage walk applies when reading one aggregate's events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LineageCap {
    /// The session's own aggregate. Its history is its own truth and is never
    /// capped by an ancestor's boundary.
    Own,
    /// Reached across a fork boundary, capped in this aggregate's numbering.
    Inherited(ParentSeq),
}

impl LineageCap {
    /// Intersect two caps on the same aggregate.
    ///
    /// The tighter of the two caps always wins. This is the whole
    /// "parent-after-cap is excluded" law within that sequence domain.
    /// Fork descent uses the boundary's parent coordinates instead: a child's
    /// sequence cannot narrow a different aggregate's history.
    pub fn narrow(self, parent_seq: ParentSeq) -> Self {
        match self {
            Self::Own => Self::Inherited(parent_seq),
            Self::Inherited(existing) => Self::Inherited(existing.min(parent_seq)),
        }
    }

    /// The inclusive sequence bound, or `None` for an uncapped own aggregate.
    ///
    /// Deliberately `Option<ParentSeq>` and not `Option<EventSeq>`: `Nothing`
    /// and `Own` are opposites, and collapsing either into `None` is how an
    /// inherit-nothing boundary would turn into an uncapped read.
    pub fn bound(self) -> Option<ParentSeq> {
        match self {
            Self::Own => None,
            Self::Inherited(parent_seq) => Some(parent_seq),
        }
    }

    /// Whether this cap admits an event at `seq` from the aggregate it applies
    /// to.
    pub fn admits(self, seq: EventSeq) -> bool {
        match self {
            Self::Own => true,
            Self::Inherited(ParentSeq::Nothing) => false,
            Self::Inherited(ParentSeq::Through(through)) => seq <= through,
        }
    }
}

/// Why a fork boundary could not be constructed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkBoundaryError {
    /// The child named itself as its own parent.
    SelfParent { session: SessionId },
    /// `parentSeq` was below the frozen floor of `-1`.
    ParentSeqBelowFloor { raw: i64 },
}

impl fmt::Display for ForkBoundaryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SelfParent { session } => {
                write!(f, "fork parent equals its child session {session}")
            }
            Self::ParentSeqBelowFloor { raw } => {
                write!(
                    f,
                    "fork parentSeq must be an integer at least -1, got {raw}"
                )
            }
        }
    }
}

impl std::error::Error for ForkBoundaryError {}

/// One validated fork boundary: a child, its parent, and the exact cap.
///
/// Both refusals are enforced by the constructor, so a `ForkBoundary` value in
/// hand is already known to name two distinct sessions and a cap at or above
/// the frozen floor. There is no way to build one that does not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkBoundary {
    child: SessionId,
    parent: SessionId,
    parent_seq: ParentSeq,
}

impl ForkBoundary {
    /// Validate a boundary from an already-decoded cap.
    pub fn new(
        child: SessionId,
        parent: SessionId,
        parent_seq: ParentSeq,
    ) -> Result<Self, ForkBoundaryError> {
        if child == parent {
            return Err(ForkBoundaryError::SelfParent { session: child });
        }
        Ok(Self {
            child,
            parent,
            parent_seq,
        })
    }

    /// Validate a boundary from the frozen wire integer, refusing both the
    /// self-parent and below-floor shapes.
    pub fn from_raw_parent_seq(
        child: SessionId,
        parent: SessionId,
        raw_parent_seq: i64,
    ) -> Result<Self, ForkBoundaryError> {
        let parent_seq =
            ParentSeq::from_raw(raw_parent_seq).ok_or(ForkBoundaryError::ParentSeqBelowFloor {
                raw: raw_parent_seq,
            })?;
        Self::new(child, parent, parent_seq)
    }

    pub fn child(&self) -> &SessionId {
        &self.child
    }

    pub fn parent(&self) -> &SessionId {
        &self.parent
    }

    pub fn parent_seq(&self) -> ParentSeq {
        self.parent_seq
    }
}

/// One aggregate in a lineage, with the cap that applies while reading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageSegment {
    session: SessionId,
    cap: LineageCap,
}

impl LineageSegment {
    pub fn session(&self) -> &SessionId {
        &self.session
    }

    pub fn cap(&self) -> LineageCap {
        self.cap
    }
}

/// Why a lineage walk was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ForkLineageError {
    /// The chain revisited a session already in the walk.
    Cycle { session: SessionId },
    /// The supplied boundary does not belong to the aggregate the walk
    /// currently sits on.
    ///
    /// This refuses a mis-wired consumer that loads one session's boundary
    /// while walking another. Without it a caller could splice two unrelated
    /// chains and the cap arithmetic would still look sound.
    BoundaryNotForHead {
        head: SessionId,
        boundary_child: SessionId,
    },
}

impl fmt::Display for ForkLineageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cycle { session } => {
                write!(f, "fork lineage contains a cycle at {session}")
            }
            Self::BoundaryNotForHead {
                head,
                boundary_child,
            } => write!(
                f,
                "fork boundary belongs to {boundary_child}, not the lineage head {head}"
            ),
        }
    }
}

impl std::error::Error for ForkLineageError {}

/// A completed lineage, ordered oldest ancestor first.
///
/// The order is part of the law, not a convenience: a fold applies the oldest
/// retained ancestor history before the nearer one, and the leaf last.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForkLineage {
    segments: Vec<LineageSegment>,
}

impl ForkLineage {
    /// The segments, oldest ancestor first and the walk's origin last.
    pub fn segments(&self) -> &[LineageSegment] {
        &self.segments
    }

    /// The aggregate the walk started from.
    ///
    /// Infallible: a lineage always has at least its origin segment.
    pub fn origin(&self) -> &LineageSegment {
        match self.segments.last() {
            Some(segment) => segment,
            // Unreachable by construction — every builder seeds one segment and
            // nothing removes segments — but matching keeps the deny-by-default
            // `unwrap_used`/`expect_used` law intact rather than buying an
            // allow for a nicety.
            None => unreachable!("a fork lineage always retains its origin segment"),
        }
    }
}

/// Walks a fork chain upward, applying the cap law and refusing cycles.
///
/// The builder holds no connection and performs no lookup. The consumer owns
/// finding each next boundary; the builder owns what that boundary means.
#[derive(Debug)]
pub struct ForkLineageBuilder {
    visited: HashSet<SessionId>,
    /// Leaf first while building; reversed by [`ForkLineageBuilder::finish`].
    chain: Vec<LineageSegment>,
}

impl ForkLineageBuilder {
    /// Start at a session reading its **own** uncapped history.
    ///
    /// This is the ordinary "fold this session's history" walk. The origin's
    /// own facts are its own truth, so its cap is [`LineageCap::Own`].
    pub fn own(session: SessionId) -> Self {
        let mut visited = HashSet::new();
        visited.insert(session.clone());
        Self {
            visited,
            chain: vec![LineageSegment {
                session,
                cap: LineageCap::Own,
            }],
        }
    }

    /// Start at a fork's **parent**, already capped by that boundary.
    ///
    /// This is the inherited-only walk: it computes exactly what a child
    /// inherits and never reads the child's own aggregate, which is what makes
    /// inheritance a reference rather than a copy.
    pub fn inherited(boundary: &ForkBoundary) -> Self {
        let mut visited = HashSet::new();
        visited.insert(boundary.parent.clone());
        Self {
            visited,
            chain: vec![LineageSegment {
                session: boundary.parent.clone(),
                cap: LineageCap::Inherited(boundary.parent_seq),
            }],
        }
    }

    /// The aggregate the walk currently sits on.
    pub fn head(&self) -> &LineageSegment {
        match self.chain.last() {
            Some(segment) => segment,
            None => unreachable!("a lineage builder always retains its origin segment"),
        }
    }

    /// Cross `boundary` upward from the current head.
    ///
    /// Refuses a boundary belonging to a different aggregate, and refuses a
    /// parent already present in this walk.
    pub fn descend(&mut self, boundary: &ForkBoundary) -> Result<(), ForkLineageError> {
        let head = self.head();
        if head.session != boundary.child {
            return Err(ForkLineageError::BoundaryNotForHead {
                head: head.session.clone(),
                boundary_child: boundary.child.clone(),
            });
        }
        // The fork fact is the child's first event. A nonempty child prefix
        // includes it and therefore its exact frozen parent boundary. Numeric
        // minima across aggregates can silently discard ancestor instructions
        // when a copied child history has fewer rows than its parent.
        let cap = match head.cap {
            LineageCap::Inherited(ParentSeq::Nothing) => LineageCap::Inherited(ParentSeq::Nothing),
            _ => LineageCap::Inherited(boundary.parent_seq),
        };
        if !self.visited.insert(boundary.parent.clone()) {
            return Err(ForkLineageError::Cycle {
                session: boundary.parent.clone(),
            });
        }
        self.chain.push(LineageSegment {
            session: boundary.parent.clone(),
            cap,
        });
        Ok(())
    }

    /// Finish the walk, ordering segments oldest ancestor first.
    pub fn finish(mut self) -> ForkLineage {
        self.chain.reverse();
        ForkLineage {
            segments: self.chain,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn session(raw: &str) -> Result<SessionId, Box<dyn std::error::Error>> {
        Ok(raw.parse::<SessionId>()?)
    }

    // ---- ParentSeq: the -1 floor is a real value, not an error -------------

    #[test]
    fn parent_seq_decodes_the_frozen_floor_as_inherit_nothing() -> TestResult {
        assert_eq!(ParentSeq::from_raw(-1), Some(ParentSeq::Nothing));
        assert_eq!(
            ParentSeq::from_raw(0),
            Some(ParentSeq::Through(EventSeq::FIRST))
        );
        assert_eq!(ParentSeq::Nothing.as_raw(), -1);
        assert_eq!(ParentSeq::Through(EventSeq::new(7)).as_raw(), 7);
        Ok(())
    }

    #[test]
    fn parent_seq_refuses_below_the_frozen_floor() -> TestResult {
        assert_eq!(ParentSeq::from_raw(-2), None);
        assert_eq!(ParentSeq::from_raw(i64::MIN), None);
        Ok(())
    }

    #[test]
    fn inherit_nothing_orders_below_every_real_sequence() -> TestResult {
        // The cap arithmetic is `min`, so this ordering IS the law that an
        // inherit-nothing boundary can never be widened by an ancestor.
        assert!(ParentSeq::Nothing < ParentSeq::Through(EventSeq::FIRST));
        assert!(ParentSeq::Through(EventSeq::new(1)) < ParentSeq::Through(EventSeq::new(2)));
        Ok(())
    }

    // ---- the cap narrows in one direction only -----------------------------

    #[test]
    fn a_first_boundary_caps_an_own_aggregate() -> TestResult {
        let cap = LineageCap::Own.narrow(ParentSeq::Through(EventSeq::new(3)));
        assert_eq!(
            cap,
            LineageCap::Inherited(ParentSeq::Through(EventSeq::new(3)))
        );
        Ok(())
    }

    #[test]
    fn a_looser_ancestor_boundary_cannot_widen_a_tighter_cap() -> TestResult {
        let cap = LineageCap::Inherited(ParentSeq::Through(EventSeq::new(2)))
            .narrow(ParentSeq::Through(EventSeq::new(9)));
        assert_eq!(
            cap,
            LineageCap::Inherited(ParentSeq::Through(EventSeq::new(2))),
            "the tighter cap must survive; widening would readmit excluded parent history"
        );
        Ok(())
    }

    #[test]
    fn a_tighter_ancestor_boundary_narrows_further() -> TestResult {
        let cap = LineageCap::Inherited(ParentSeq::Through(EventSeq::new(9)))
            .narrow(ParentSeq::Through(EventSeq::new(2)));
        assert_eq!(
            cap,
            LineageCap::Inherited(ParentSeq::Through(EventSeq::new(2)))
        );
        Ok(())
    }

    #[test]
    fn inherit_nothing_absorbs_every_ancestor_boundary() -> TestResult {
        let cap = LineageCap::Inherited(ParentSeq::Nothing)
            .narrow(ParentSeq::Through(EventSeq::new(1_000)));
        assert_eq!(cap, LineageCap::Inherited(ParentSeq::Nothing));
        Ok(())
    }

    // ---- cap admission: both directions ------------------------------------

    #[test]
    fn a_cap_admits_through_the_boundary_and_excludes_everything_after() -> TestResult {
        let cap = LineageCap::Inherited(ParentSeq::Through(EventSeq::new(1)));
        assert!(
            cap.admits(EventSeq::new(0)),
            "history at the cap is inherited"
        );
        assert!(cap.admits(EventSeq::new(1)), "the cap is inclusive");
        assert!(
            !cap.admits(EventSeq::new(2)),
            "a parent fact after the cap never enters the child"
        );
        Ok(())
    }

    #[test]
    fn an_own_aggregate_is_never_capped() -> TestResult {
        assert!(LineageCap::Own.admits(EventSeq::new(0)));
        assert!(LineageCap::Own.admits(EventSeq::new(u64::MAX)));
        assert_eq!(
            LineageCap::Own.bound(),
            None,
            "a session's own history is its own truth, not an inherited range"
        );
        Ok(())
    }

    #[test]
    fn inherit_nothing_admits_no_parent_event_at_all() -> TestResult {
        let cap = LineageCap::Inherited(ParentSeq::Nothing);
        assert!(!cap.admits(EventSeq::FIRST));
        assert_eq!(
            cap.bound(),
            Some(ParentSeq::Nothing),
            "inherit-nothing must stay distinguishable from an uncapped own read"
        );
        Ok(())
    }

    // ---- boundary refusals -------------------------------------------------

    #[test]
    fn a_boundary_refuses_a_self_parent() -> TestResult {
        let child = session("ses_lineage_self")?;
        assert_eq!(
            ForkBoundary::new(child.clone(), child.clone(), ParentSeq::Nothing),
            Err(ForkBoundaryError::SelfParent { session: child })
        );
        Ok(())
    }

    #[test]
    fn a_boundary_refuses_a_parent_seq_below_the_floor() -> TestResult {
        let child = session("ses_lineage_child")?;
        let parent = session("ses_lineage_parent")?;
        assert_eq!(
            ForkBoundary::from_raw_parent_seq(child, parent, -2),
            Err(ForkBoundaryError::ParentSeqBelowFloor { raw: -2 })
        );
        Ok(())
    }

    #[test]
    fn a_boundary_accepts_the_floor_itself() -> TestResult {
        let child = session("ses_lineage_child")?;
        let parent = session("ses_lineage_parent")?;
        let boundary = ForkBoundary::from_raw_parent_seq(child, parent, -1)?;
        assert_eq!(boundary.parent_seq(), ParentSeq::Nothing);
        Ok(())
    }

    // ---- the walk ----------------------------------------------------------

    #[test]
    fn an_own_walk_keeps_its_leaf_uncapped_and_orders_ancestors_first() -> TestResult {
        let root = session("ses_lineage_root")?;
        let middle = session("ses_lineage_middle")?;
        let leaf = session("ses_lineage_leaf")?;

        let mut builder = ForkLineageBuilder::own(leaf.clone());
        builder.descend(&ForkBoundary::new(
            leaf.clone(),
            middle.clone(),
            ParentSeq::Through(EventSeq::new(5)),
        )?)?;
        builder.descend(&ForkBoundary::new(
            middle.clone(),
            root.clone(),
            ParentSeq::Through(EventSeq::new(9)),
        )?)?;
        let lineage = builder.finish();

        let segments = lineage.segments();
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0].session(), &root);
        assert_eq!(segments[1].session(), &middle);
        assert_eq!(segments[2].session(), &leaf);
        assert_eq!(
            segments[2].cap(),
            LineageCap::Own,
            "the leaf reads its own aggregate uncapped"
        );
        assert_eq!(
            segments[1].cap(),
            LineageCap::Inherited(ParentSeq::Through(EventSeq::new(5)))
        );
        assert_eq!(
            segments[0].cap(),
            LineageCap::Inherited(ParentSeq::Through(EventSeq::new(9))),
            "the root's boundary is expressed in root coordinates, not middle coordinates"
        );
        assert_eq!(lineage.origin().session(), &leaf);
        Ok(())
    }

    #[test]
    fn an_inherited_walk_starts_at_the_parent_and_never_reads_the_child() -> TestResult {
        let parent = session("ses_lineage_parent")?;
        let child = session("ses_lineage_child")?;
        let boundary = ForkBoundary::new(
            child.clone(),
            parent.clone(),
            ParentSeq::Through(EventSeq::new(1)),
        )?;

        let lineage = ForkLineageBuilder::inherited(&boundary).finish();
        let segments = lineage.segments();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].session(), &parent);
        assert_eq!(
            segments[0].cap(),
            LineageCap::Inherited(ParentSeq::Through(EventSeq::new(1)))
        );
        assert!(
            segments.iter().all(|segment| segment.session() != &child),
            "inheritance is a capped reference to the parent, never a read of the child"
        );
        Ok(())
    }

    #[test]
    fn an_empty_child_prefix_does_not_inherit_its_ancestors() -> TestResult {
        let child = session("ses_lineage_child")?;
        let parent = session("ses_lineage_parent")?;
        let root = session("ses_lineage_root")?;
        let mut builder = ForkLineageBuilder::inherited(&ForkBoundary::new(
            child,
            parent.clone(),
            ParentSeq::Nothing,
        )?);
        builder.descend(&ForkBoundary::new(
            parent,
            root,
            ParentSeq::Through(EventSeq::new(99)),
        )?)?;
        assert!(builder
            .finish()
            .segments()
            .iter()
            .all(|segment| { segment.cap() == LineageCap::Inherited(ParentSeq::Nothing) }));
        Ok(())
    }

    #[test]
    fn a_walk_refuses_a_cycle_back_to_a_visited_session() -> TestResult {
        let a = session("ses_lineage_a")?;
        let b = session("ses_lineage_b")?;

        let mut builder = ForkLineageBuilder::own(a.clone());
        builder.descend(&ForkBoundary::new(
            a.clone(),
            b.clone(),
            ParentSeq::Nothing,
        )?)?;
        assert_eq!(
            builder.descend(&ForkBoundary::new(
                b.clone(),
                a.clone(),
                ParentSeq::Nothing
            )?),
            Err(ForkLineageError::Cycle { session: a })
        );
        Ok(())
    }

    #[test]
    fn a_walk_refuses_a_boundary_from_another_aggregate() -> TestResult {
        let leaf = session("ses_lineage_leaf")?;
        let stranger = session("ses_lineage_stranger")?;
        let elsewhere = session("ses_lineage_elsewhere")?;

        let mut builder = ForkLineageBuilder::own(leaf.clone());
        assert_eq!(
            builder.descend(&ForkBoundary::new(
                stranger.clone(),
                elsewhere,
                ParentSeq::Nothing,
            )?),
            Err(ForkLineageError::BoundaryNotForHead {
                head: leaf,
                boundary_child: stranger,
            })
        );
        Ok(())
    }

    #[test]
    fn one_child_cannot_carry_two_fork_boundaries() -> TestResult {
        // The value-level shape of the durable "session already has a fork
        // fact" refusal: once a child's boundary is applied the walk has moved
        // on, so a second boundary for that same child can never be applied
        // over the first.
        let child = session("ses_lineage_child")?;
        let first_parent = session("ses_lineage_first_parent")?;
        let second_parent = session("ses_lineage_second_parent")?;

        let mut builder = ForkLineageBuilder::own(child.clone());
        builder.descend(&ForkBoundary::new(
            child.clone(),
            first_parent.clone(),
            ParentSeq::Through(EventSeq::new(4)),
        )?)?;
        assert_eq!(
            builder.descend(&ForkBoundary::new(
                child.clone(),
                second_parent,
                ParentSeq::Through(EventSeq::new(4)),
            )?),
            Err(ForkLineageError::BoundaryNotForHead {
                head: first_parent,
                boundary_child: child,
            }),
            "a second boundary for the same child cannot displace the first"
        );
        Ok(())
    }

    #[test]
    fn a_parent_fact_after_the_cap_is_never_resurrected_by_a_later_read() -> TestResult {
        // Resurrection shape: the cap is a property of the boundary, not of
        // when the parent was observed. A parent fact appended after the fork
        // lands at a higher sequence, and the same cap must exclude it on
        // every subsequent read.
        let child = session("ses_lineage_child")?;
        let parent = session("ses_lineage_parent")?;
        let boundary = ForkBoundary::new(child, parent, ParentSeq::Through(EventSeq::new(1)))?;
        let cap = ForkLineageBuilder::inherited(&boundary)
            .finish()
            .origin()
            .cap();

        let after_the_fork = [EventSeq::new(2), EventSeq::new(3), EventSeq::new(99)];
        assert!(
            after_the_fork.iter().all(|seq| !cap.admits(*seq)),
            "no later parent fact may enter the child, however long after the fork it arrives"
        );
        assert!(
            cap.admits(EventSeq::new(1)),
            "the boundary itself stays inherited"
        );
        Ok(())
    }

    #[test]
    fn rebuilding_a_lineage_from_the_same_boundaries_is_identical() -> TestResult {
        // Rebuild shape: the cache is discardable because the walk is a pure
        // function of the durable boundaries. Two independent walks over the
        // same chain must be equal, value for value.
        let root = session("ses_lineage_root")?;
        let middle = session("ses_lineage_middle")?;
        let leaf = session("ses_lineage_leaf")?;
        let first = ForkBoundary::new(
            leaf.clone(),
            middle.clone(),
            ParentSeq::Through(EventSeq::new(5)),
        )?;
        let second = ForkBoundary::new(middle, root, ParentSeq::Through(EventSeq::new(9)))?;

        let build = || -> Result<ForkLineage, ForkLineageError> {
            let mut builder = ForkLineageBuilder::own(leaf.clone());
            builder.descend(&first)?;
            builder.descend(&second)?;
            Ok(builder.finish())
        };
        assert_eq!(
            build()?,
            build()?,
            "the walk is a pure fold over boundaries"
        );
        Ok(())
    }

    #[test]
    fn a_refused_descent_leaves_the_walk_unchanged() -> TestResult {
        let a = session("ses_lineage_a")?;
        let b = session("ses_lineage_b")?;

        let mut builder = ForkLineageBuilder::own(a.clone());
        builder.descend(&ForkBoundary::new(
            a.clone(),
            b.clone(),
            ParentSeq::Nothing,
        )?)?;
        let before = builder.head().clone();
        let _ = builder.descend(&ForkBoundary::new(
            b.clone(),
            a.clone(),
            ParentSeq::Nothing,
        )?);
        assert_eq!(
            builder.head(),
            &before,
            "a refused descent must not half-apply a segment"
        );
        assert_eq!(builder.finish().segments().len(), 2);
        Ok(())
    }
}
