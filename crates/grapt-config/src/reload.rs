//! Holding a complete configuration ACROSS loads.
//!
//! This is **U1b2b** of the IA0-2 configuration lane, and the last module of
//! U1b. Everything under it answers a question about ONE load: [`discover`]
//! says which slots exist and what was found at each, [`admit`] says which
//! captured documents became contributors, and [`ConfigLedger::complete`] says
//! whether the answer is whole. None of them can say whether the answer is the
//! SAME answer as last time, because none of them outlives the call.
//!
//! A [`ConfigSession`] does. It holds one [`CompleteLedger`] and the
//! [`Generation`] that produced it, and it can be asked to look at the world
//! again.
//!
//! # The one law this module exists to hold
//!
//! **A generation advances exactly once per complete load, and a refused
//! reload changes nothing.**
//!
//! Both halves are load-bearing and they fail in opposite directions.
//!
//! A generation that advanced on a REFUSED reload would be a version number
//! for a version that does not exist. Everything downstream of a configuration
//! compares generations to decide whether to re-derive: a caller that sees the
//! number move re-reads a configuration nobody successfully loaded, and — worse
//! — a caller that CACHED against the old number never re-reads again, because
//! the number it is watching has already moved past the one its cache is keyed
//! to. The refusal would silently pin a stale answer as current.
//!
//! A generation that advanced TWICE, or not at all, on a complete load fails
//! the same consumers from the other side. Not at all means a real change is
//! invisible and every cache keyed to the generation serves the superseded
//! configuration forever. Twice means a consumer that steps generations to
//! reconcile — the natural way to ask "what happened since I last looked" —
//! sees a generation that no load ever produced and cannot tell whether it
//! missed one.
//!
//! So the number is not a convenience. It is the only thing a holder of a
//! configuration can compare, and it is worth exactly as much as the guarantee
//! that it moves once, on success, and never otherwise.
//!
//! # How the law is held, rather than remembered
//!
//! [`ConfigSession::reload`] computes the ENTIRE next configuration before it
//! touches a single field of the session. Discovery, admission and the
//! completeness check all run against local values; only when all three have
//! succeeded does anything mutate, and the two mutations — the generation and
//! the held configuration — sit on consecutive lines at the end of the
//! function with no fallible step between them.
//!
//! The refusal path cannot lose the held configuration, and not because this
//! module remembers not to lose it. Every refusal is built by
//! `ConfigSession::refused`, which takes `&self`. A refusal has no `&mut`
//! anywhere in its construction, so "clobber the held state on the way out"
//! is not an edit somebody could make here and have it compile. That is a
//! deliberate choice of shape: the alternative — a `&mut self` helper that is
//! documented not to mutate — would put the guarantee in prose, and prose is
//! what the reader has to trust when the type system could have been the one
//! saying it.
//!
//! # What a session does NOT do
//!
//! It does not watch. There is no thread, no timer and no filesystem
//! notification here: a session re-examines the world exactly when a caller
//! asks it to, with a reader the caller supplies. Deciding WHEN to look is a
//! host's question — it knows about editors writing files in two steps, about
//! debounce windows, about whether a reload mid-request is acceptable — and
//! answering it here would bake one host's answer into every host's
//! configuration crate.
//!
//! It does not DIFF. A session can say the generation moved; it does not say
//! what moved. Reporting that would mean comparing two configurations, and
//! comparing them meaningfully means knowing what each setting MEANS — which
//! is the same domain knowledge this crate declines to have when it declines
//! to merge. The census is retained on both sides ([`ConfigLedger::census`]),
//! so a caller that wants the comparison has the material for it.
//!
//! It does not roll back to an arbitrary earlier generation. Exactly one
//! configuration is in force at a time, and it is the newest one that loaded
//! completely.
//!
//! [`discover`]: crate::source::discover
//! [`admit`]: crate::ledger::admit
//! [`ConfigLedger::complete`]: crate::ledger::ConfigLedger::complete
//! [`ConfigLedger::census`]: crate::ledger::ConfigLedger::census

use core::fmt;

use crate::bounds::ConfigBounds;
use crate::ledger::{admit, CompleteLedger, IncompleteLedger, LedgerRefusal};
use crate::source::{discover, DiscoveryPlan, DiscoveryRefusal, SourceReader};
use crate::source_bounds::SourceBounds;

/// Which complete load produced the configuration in force.
///
/// Opaque, with no `Default`, no arithmetic and no public constructor, for the
/// same reason [`crate::bounds`] types have none: a generation a caller could
/// build is a generation a caller could build a STALE one of, and comparing
/// against it would answer confidently and wrongly. The only way to obtain one
/// is from a session that actually loaded.
///
/// Ordered, because "is this newer than what I cached" is the question it
/// exists to answer. Equality is what a cache key compares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Generation(u64);

impl Generation {
    /// The generation of the first complete load.
    ///
    /// One rather than zero so that a generation is never the value an
    /// uninitialised or defaulted counter would hold. A caller that stores
    /// `0` as "I have never loaded" cannot collide with a real generation.
    const FIRST: Self = Generation(1);

    /// This generation as a number, for logging and cache keys.
    pub fn value(self) -> u64 {
        self.0
    }

    /// The generation after this one, or `None` at the ceiling.
    ///
    /// Checked rather than wrapping, and checked rather than saturating.
    /// Wrapping would make generation `0` follow `u64::MAX` and hand every
    /// cache in the system a number it had already seen — the one failure the
    /// whole type exists to prevent. Saturating is worse in a quieter way: the
    /// counter would stop advancing while loads kept succeeding, so a real
    /// configuration change would become permanently invisible with nothing
    /// reporting that anything was wrong.
    ///
    /// Refusing is the only answer that stays honest. It is also, for the
    /// record, unreachable in practice — a session would have to complete
    /// 2^64 loads — which is exactly why it is witnessed from a synthetic
    /// session in this module's own tests rather than from a tree.
    fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Generation)
    }
}

impl fmt::Display for Generation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Why a load could not produce a complete configuration.
///
/// One enum over the three stages rather than three unrelated errors, because
/// a caller of [`ConfigSession::reload`] has exactly one decision to make —
/// keep running on what it has, or report — and it makes that decision the
/// same way whichever stage refused. The stage is preserved for the report.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ReloadRefusal {
    /// The plan could not be walked at all.
    #[error("discovery refused: {0}")]
    Discovery(#[from] DiscoveryRefusal),
    /// The census could not be admitted as a ledger.
    #[error("admission refused: {0}")]
    Ledger(#[from] LedgerRefusal),
    /// The ledger was built and is not complete.
    #[error("the configuration is incomplete: {0}")]
    Incomplete(#[from] IncompleteLedger),
    /// The session has completed `u64::MAX` loads and cannot number another.
    ///
    /// Carried as a refusal rather than handled by wrapping or saturating, for
    /// the reasons `Generation::next` states. A session that reaches it keeps
    /// serving the configuration it holds; it simply cannot advance again.
    #[error("this session cannot number another generation")]
    GenerationExhausted,
}

/// A reload that was refused, and the generation that is still in force.
///
/// The held generation is carried BECAUSE the reload failed, not in spite of
/// it. A caller that logs "reload failed" and nothing else leaves its reader
/// unable to tell which configuration the program is now running under; the
/// answer is that it is running under the same one as before, and this says
/// which one that is.
///
/// It is the generation the session holds AFTER the refusal, which — this
/// being the law of the module — is also the one it held before.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("reload refused, generation {held} remains in force: {cause}")]
pub struct RefusedReload {
    held: Generation,
    cause: ReloadRefusal,
}

impl RefusedReload {
    /// The generation still in force.
    pub fn held(&self) -> Generation {
        self.held
    }

    /// Why the reload was refused.
    pub fn cause(&self) -> &ReloadRefusal {
        &self.cause
    }
}

/// One configuration, held across loads, with the generation that produced it.
///
/// Constructed by [`ConfigSession::load`], which is the only way in: a session
/// that existed before its first successful load would have a generation
/// numbering a configuration it did not have, and every caller would have to
/// check for that state. There is no such state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigSession {
    plan: DiscoveryPlan,
    bounds: ConfigBounds,
    source_bounds: SourceBounds,
    generation: Generation,
    held: CompleteLedger,
}

impl ConfigSession {
    /// Load a configuration for the first time, or refuse.
    ///
    /// The plan and both bound sets are captured here and are FIXED for the
    /// life of the session. A reload that could be handed a different plan
    /// would not be a reload — it would be a different question, and its
    /// generation would number an answer to a question the previous generation
    /// never asked. A caller that wants a different plan builds a different
    /// session.
    ///
    /// The reader is NOT captured, and is supplied per call instead: it is the
    /// one thing that is expected to see something new each time.
    pub fn load(
        plan: DiscoveryPlan,
        reader: &dyn SourceReader,
        bounds: ConfigBounds,
        source_bounds: SourceBounds,
    ) -> Result<Self, ReloadRefusal> {
        let held = load_once(&plan, reader, bounds, source_bounds)?;
        Ok(Self {
            plan,
            bounds,
            source_bounds,
            generation: Generation::FIRST,
            held,
        })
    }

    /// The generation of the configuration in force.
    pub fn generation(&self) -> Generation {
        self.generation
    }

    /// The configuration in force.
    ///
    /// Always a complete one. There is no arrangement of calls on this type
    /// that produces a session holding an incomplete configuration, because
    /// the only two places `held` is written both write a [`CompleteLedger`]
    /// that was constructed in the same expression.
    pub fn configuration(&self) -> &CompleteLedger {
        &self.held
    }

    /// The plan this session was built for, and reloads under.
    pub fn plan(&self) -> &DiscoveryPlan {
        &self.plan
    }

    /// Look at the world again, and adopt what is found if it is complete.
    ///
    /// On success the generation advances by exactly one and the newly loaded
    /// configuration is installed. On refusal NOTHING changes: not the
    /// generation, not the held configuration, not the plan or the bounds.
    ///
    /// The ordering is the guarantee. Every fallible step runs first, against
    /// locals; the session is mutated only after the last of them has already
    /// succeeded.
    pub fn reload(&mut self, reader: &dyn SourceReader) -> Result<Generation, RefusedReload> {
        // Fallible work first, and all of it. `loaded` is a complete
        // configuration or this function has already returned.
        let loaded = match load_once(&self.plan, reader, self.bounds, self.source_bounds) {
            Ok(loaded) => loaded,
            Err(cause) => return Err(self.refused(cause)),
        };
        // The last thing that can refuse. Checked BEFORE the install so a
        // session at the ceiling keeps a configuration whose generation is the
        // one that loaded it, rather than a newer configuration under a number
        // that is a repeat.
        let Some(next) = self.generation.next() else {
            return Err(self.refused(ReloadRefusal::GenerationExhausted));
        };
        // Both mutations, adjacent, with nothing fallible between them.
        self.generation = next;
        self.held = loaded;
        Ok(next)
    }

    /// Build the refusal for a reload that changed nothing.
    ///
    /// Takes `&self` deliberately. This is the only constructor of
    /// [`RefusedReload`] reachable from [`ConfigSession::reload`], so the
    /// borrow checker — not a convention — is what keeps a refusal from being
    /// the place the held configuration or the generation is lost.
    fn refused(&self, cause: ReloadRefusal) -> RefusedReload {
        RefusedReload {
            held: self.generation,
            cause,
        }
    }
}

/// Discover, admit and complete, in that order, against locals only.
///
/// A free function rather than a method because it must not be able to touch a
/// session even by accident: the exactly-once law is a statement about when
/// `ConfigSession`'s fields are written, and the way to keep that statement
/// small is to give the code that can fail no access to them.
fn load_once(
    plan: &DiscoveryPlan,
    reader: &dyn SourceReader,
    bounds: ConfigBounds,
    source_bounds: SourceBounds,
) -> Result<CompleteLedger, ReloadRefusal> {
    let census = discover(plan, reader, source_bounds)?;
    let ledger = admit(&census, bounds)?;
    Ok(ledger.complete()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::bounds::{
        MaxAncestorDepth, MaxDocumentBytes, MaxDocumentNodes, MaxDocuments, MaxValueDepth,
    };
    use crate::source::{ConfigFileName, ConfigSourcePath, DeterministicSourceReader};
    use crate::source_bounds::MaxPathBytes;
    use std::path::PathBuf;

    /// The ceiling refusal is unreachable from any tree — a session would have
    /// to complete `u64::MAX` loads to arrive at it — so it is witnessed from a
    /// SYNTHETIC session built by writing the field directly. That is only
    /// possible from inside this module, which is why the witness lives here
    /// rather than in `tests/reload_authority.rs`.
    ///
    /// Without it, `Generation::next`'s `None` arm is a branch no input
    /// reaches, and a `checked_add` silently replaced by a wrapping one would
    /// be indistinguishable from the code that is here.
    #[test]
    fn a_session_at_the_ceiling_refuses_rather_than_repeating_a_generation() {
        let reader = reader();
        let mut session = session(&reader);

        session.generation = Generation(u64::MAX);
        let before = session.configuration().clone();

        let refusal = match session.reload(&reader) {
            Ok(generation) => panic!("a session at the ceiling advanced to {generation}"),
            Err(refusal) => refusal,
        };

        assert_eq!(
            refusal.cause(),
            &ReloadRefusal::GenerationExhausted,
            "the ceiling refuses for its own reason, not a discovery one"
        );
        assert_eq!(
            session.generation().value(),
            u64::MAX,
            "the generation did not wrap to a number a cache has already seen"
        );
        assert_eq!(
            session.configuration(),
            &before,
            "a session at the ceiling keeps serving the configuration it holds"
        );
    }

    /// The paired positive: the same session, one below the ceiling, advances
    /// normally. Without it, an implementation that refused EVERY reload would
    /// keep the witness above green.
    #[test]
    fn a_session_one_below_the_ceiling_still_advances_once() {
        let reader = reader();
        let mut session = session(&reader);

        session.generation = Generation(u64::MAX - 1);

        let advanced = match session.reload(&reader) {
            Ok(generation) => generation,
            Err(refusal) => panic!("the reload was refused: {refusal}"),
        };

        assert_eq!(
            advanced.value(),
            u64::MAX,
            "the last numberable generation is reached, not skipped"
        );
        assert_eq!(session.generation(), advanced, "the session holds it");
    }

    fn abs(parts: &[&str]) -> PathBuf {
        let mut path = PathBuf::new();
        path.push(if cfg!(windows) { "C:\\" } else { "/" });
        for part in parts {
            path.push(part);
        }
        path
    }

    fn reader() -> DeterministicSourceReader {
        DeterministicSourceReader::new()
            .with_file(&abs(&["project", "config.jsonc"]), b"{}".to_vec())
    }

    fn session(reader: &DeterministicSourceReader) -> ConfigSession {
        let Ok(path_bytes) = MaxPathBytes::try_new(4096) else {
            panic!("path byte bound refused");
        };
        let Ok(start) = ConfigSourcePath::new(&abs(&["project"]), path_bytes) else {
            panic!("start path refused");
        };
        let Some(file_name) = ConfigFileName::new("config.jsonc") else {
            panic!("config file name refused");
        };
        let plan = DiscoveryPlan::new(start, None, file_name);

        let Ok(document_bytes) = MaxDocumentBytes::try_new(1024 * 1024) else {
            panic!("document byte bound refused");
        };
        let Ok(ancestor_depth) = MaxAncestorDepth::try_new(2) else {
            panic!("ancestor depth bound refused");
        };
        let Ok(documents) = MaxDocuments::try_new(64) else {
            panic!("document count bound refused");
        };
        let Ok(value_depth) = MaxValueDepth::try_new(32) else {
            panic!("value depth bound refused");
        };
        let Ok(nodes) = MaxDocumentNodes::try_new(10_000) else {
            panic!("node bound refused");
        };
        let bounds = ConfigBounds::new(
            document_bytes,
            ancestor_depth,
            documents,
            value_depth,
            nodes,
        );
        let Ok(source_bounds) =
            SourceBounds::try_new(document_bytes, ancestor_depth, documents, path_bytes)
        else {
            panic!("source bounds refused");
        };

        match ConfigSession::load(plan, reader, bounds, source_bounds) {
            Ok(session) => session,
            Err(refusal) => panic!("the first load was refused: {refusal}"),
        }
    }
}
