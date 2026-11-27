//! R4D-2E — the evidence tier a live differential run would be executed under,
//! and the authorization it does not have.
//!
//! # Why a third tier exists at all
//!
//! Two evidence tiers were already in use when this lane opened: the local
//! focused gate a seat runs on its own machine, and the governed Linux profile
//! that runs headless batteries under `remote-compute`. A live differential fits
//! neither, and the mismatch is not cosmetic:
//!
//! * it is **interactive** — a GUI process with a human present;
//! * it is **core-occupying on the machine every seat shares**;
//! * its artifacts **never pass through `remote-compute`**, so they inherit none
//!   of that profile's custody, caps or cleanup; and
//! * one of its four matrices suspends the host.
//!
//! A run executed under an unruled tier carries an evidence class a reviewer can
//! void, and this campaign has quarantined work on exactly that ground. So the
//! tier is named here, with its caps as data rather than as prose.
//!
//! # What this module deliberately does NOT do
//!
//! It does not execute anything, and it cannot. [`RunAuthorization`] has exactly
//! one variant this crate can construct — [`RunAuthorization::Parked`] — because
//! the ruling that created the tier explicitly withheld execution. The other
//! variant carries an [`AttendedWindowReceipt`] whose field is private and whose
//! only constructor is `pub(crate)`, so no caller outside this crate can mint
//! one and no automation here can fabricate an authorization it was not given.
//!
//! That sealing is the point. A tier that could authorize its own execution
//! would be a tier in name only, and the refusal it exists to produce would be a
//! comment rather than a law.

use std::num::{NonZeroU32, NonZeroU64};

/// Which evidence tier a run is executed under.
///
/// Closed. A fourth tier is a reviewed law change, not a new string — the same
/// discipline the landed [`crate::EngineSelection`] applies to engines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum EvidenceTier {
    /// A focused gate run on the seat's own machine: one crate, one check.
    LocalFocused,
    /// The governed headless profile, with `remote-compute` custody and caps.
    GovernedLinuxProfile,
    /// An interactive, core-occupying run on the live fleet host, with an
    /// operator present for the whole window. Ruled as a distinct tier and
    /// deliberately not executable here.
    AttendedFleetHost,
}

impl EvidenceTier {
    /// The stable label used in the corpus.
    ///
    /// Fixed text rather than a derived transformation of the variant name, for
    /// the reason every other label in this crate is fixed: a recorded tier must
    /// keep meaning the same thing after a rename.
    pub fn label(self) -> &'static str {
        match self {
            EvidenceTier::LocalFocused => "local-focused",
            EvidenceTier::GovernedLinuxProfile => "governed-linux-profile",
            EvidenceTier::AttendedFleetHost => "attended-fleet-host",
        }
    }

    /// The inverse of [`label`](Self::label). Unknown text is refused rather
    /// than defaulted: a corpus row naming a tier this build does not know is a
    /// fact we cannot interpret, and guessing would file a run under the wrong
    /// evidence class.
    pub fn from_label(label: &str) -> Option<Self> {
        EvidenceTier::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    /// Whether this tier occupies the machine other seats are working on.
    ///
    /// Two of the three do. The focused tier runs on the Windows PC, which is
    /// the live fleet host, and so does the attended tier; only the governed
    /// Linux profile runs on a dedicated machine. What separates the two
    /// sharing tiers is not the host but the *weight*: a focused check is a
    /// single quick gate, while an attended run starts engines and holds the
    /// machine for a window. That is why only the attended tier carries
    /// [`TierCaps`] — the numbers there are what keeps a run of that weight
    /// from becoming a fleet outage nobody agreed to.
    ///
    /// This doc previously read "only the attended tier does", which contradicted
    /// the code below and the test that pins it. Nothing consumed the predicate,
    /// so nothing behaved wrongly; a false statement in a module whose purpose is
    /// to state law as data is still a defect, and this is the correction.
    pub fn shares_the_fleet_host(self) -> bool {
        match self {
            EvidenceTier::LocalFocused => true,
            EvidenceTier::GovernedLinuxProfile => false,
            EvidenceTier::AttendedFleetHost => true,
        }
    }

    /// Whether a run under this tier needs an announced, operator-attended
    /// window before it may start.
    pub fn requires_attendance(self) -> bool {
        matches!(self, EvidenceTier::AttendedFleetHost)
    }

    pub const ALL: [EvidenceTier; 3] = [
        EvidenceTier::LocalFocused,
        EvidenceTier::GovernedLinuxProfile,
        EvidenceTier::AttendedFleetHost,
    ];
}

/// The attended tier's hard caps, in counts, bytes and milliseconds.
///
/// # Why these are values and not prose
///
/// Every cap here was a sentence in a preflight before it was a number. A
/// sentence cannot be exceeded, compared or mutated; a number can, which is why
/// the battery can prove each one is load-bearing. The ratified law that
/// bounds be stated "in bytes and counts, never as a duration or an adjective"
/// is the same law the landed [`crate::LongSessionBound`] encodes, applied to
/// the run rather than to the reads.
///
/// The values are the ruled ones. They are deliberately **not** derived from
/// the host that happens to be running: a cap computed from the machine would
/// grow silently on a bigger machine, and the reason for the cap is the other
/// seats, not the hardware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TierCaps {
    whole_run_ms: NonZeroU64,
    per_matrix_ms: NonZeroU64,
    logical_processors: NonZeroU32,
    run_tree_bytes: NonZeroU64,
    host_free_floor_bytes: NonZeroU64,
    engine_processes: NonZeroU32,
}

impl TierCaps {
    /// The attended tier's ruled caps.
    ///
    /// Each number, and why it is that number:
    ///
    /// * **90 minutes whole run.** The run competes with live seats; an
    ///   open-ended window is a fleet outage nobody agreed to. A run that
    ///   reaches this cap stops and cleans up regardless of completion.
    /// * **20 minutes per matrix.** Leaves ten minutes inside the ninety for
    ///   setup, reversal and cleanup — so the per-matrix cap can be spent in
    ///   full on every matrix a restart-scoped run can hold without the
    ///   whole-run cap firing during teardown.
    /// * **4 logical processors.** The host has sixteen; twelve stay available
    ///   to the seats that did not ask for this run.
    /// * **4 GiB run tree, 4 GiB host free floor.** The floor is what protects
    ///   the seats: the run aborts when the *host* drops below it, which is a
    ///   different fact from the run's own ceiling and is why both exist.
    /// * **2 engine processes.** A third listener makes attribution a guess,
    ///   and attribution is the one thing this whole contract is built to keep
    ///   mechanical.
    ///
    /// `NonZeroU64::MIN`-based arithmetic is avoided here: every value is
    /// written as the literal it was ruled as, so a mutant that changes a cap
    /// changes a number a reader can see.
    pub const ATTENDED: TierCaps = TierCaps {
        // 90 * 60 * 1000
        whole_run_ms: nonzero_u64(5_400_000),
        // 20 * 60 * 1000
        per_matrix_ms: nonzero_u64(1_200_000),
        logical_processors: nonzero_u32(4),
        // 4 GiB
        run_tree_bytes: nonzero_u64(4_294_967_296),
        // 4 GiB
        host_free_floor_bytes: nonzero_u64(4_294_967_296),
        engine_processes: nonzero_u32(2),
    };

    /// The caps that apply to a tier, if that tier has any of its own.
    ///
    /// Returns `Option` rather than a default set: the local and governed tiers
    /// have their own custody elsewhere, and inventing caps for them here would
    /// be this crate claiming authority over two tiers it does not own.
    pub fn for_tier(tier: EvidenceTier) -> Option<TierCaps> {
        match tier {
            EvidenceTier::AttendedFleetHost => Some(TierCaps::ATTENDED),
            EvidenceTier::LocalFocused | EvidenceTier::GovernedLinuxProfile => None,
        }
    }

    pub fn whole_run_ms(self) -> NonZeroU64 {
        self.whole_run_ms
    }

    pub fn per_matrix_ms(self) -> NonZeroU64 {
        self.per_matrix_ms
    }

    pub fn logical_processors(self) -> NonZeroU32 {
        self.logical_processors
    }

    pub fn run_tree_bytes(self) -> NonZeroU64 {
        self.run_tree_bytes
    }

    pub fn host_free_floor_bytes(self) -> NonZeroU64 {
        self.host_free_floor_bytes
    }

    pub fn engine_processes(self) -> NonZeroU32 {
        self.engine_processes
    }

    /// Whether the per-matrix cap leaves room inside the whole-run cap.
    ///
    /// Strict inequality, not `<=`: a per-matrix cap equal to the whole-run cap
    /// would let one matrix consume the entire window, leaving nothing for the
    /// reversal and cleanup that §12 requires to run on every exit path. A cap
    /// that can be satisfied only by skipping cleanup is not a cap.
    pub fn leaves_room_for_cleanup(self) -> bool {
        self.per_matrix_ms.get() < self.whole_run_ms.get()
    }
}

/// `NonZeroU64` in const position without `unwrap`, which is denied workspace-wide.
///
/// `NonZeroU64::new` returns `Option`, and the crate's lint regime denies both
/// `unwrap` and `expect` including in test code. A `match` in a `const fn` gives
/// the same compile-time evaluation with no denied call, and a zero literal
/// would fail to compile rather than panicking at runtime.
const fn nonzero_u64(value: u64) -> NonZeroU64 {
    match NonZeroU64::new(value) {
        Some(value) => value,
        None => panic!("a tier cap of zero is not a cap"),
    }
}

/// `NonZeroU32` in const position, for the same reason as [`nonzero_u64`].
const fn nonzero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => panic!("a tier cap of zero is not a cap"),
    }
}

/// Why a run is parked rather than authorized.
///
/// Closed, and every variant names a decision that belongs to someone other
/// than the automation. A park reason is not a failure: it is the automation
/// correctly declining to assume an authority it was not given.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ParkReason {
    /// The attended tier is ruled to exist, and its execution was explicitly
    /// withheld pending a scheduled window. This is the state every plan this
    /// crate can build is in.
    AttendedWindowNotScheduled,
    /// Sleep/resume needs a fleet quiesce or a second Windows host. A process
    /// suspend or a network drop is a different matrix wearing its label, and
    /// is refused rather than accepted as a substitute.
    SleepResumeAwaitsHostDecision,
    /// The Rust entrypoint that would report host bounds does not exist yet.
    EntrypointNotLanded,
}

impl ParkReason {
    pub fn label(self) -> &'static str {
        match self {
            ParkReason::AttendedWindowNotScheduled => "attended-window-not-scheduled",
            ParkReason::SleepResumeAwaitsHostDecision => "sleep-resume-awaits-host-decision",
            ParkReason::EntrypointNotLanded => "entrypoint-not-landed",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        ParkReason::ALL
            .into_iter()
            .find(|candidate| candidate.label() == label)
    }

    pub const ALL: [ParkReason; 3] = [
        ParkReason::AttendedWindowNotScheduled,
        ParkReason::SleepResumeAwaitsHostDecision,
        ParkReason::EntrypointNotLanded,
    ];
}

/// Proof that an attended window was scheduled and announced.
///
/// # Why this type has no public constructor
///
/// The ruling that created the attended tier said to encode the tier and its
/// caps and **not** to execute it. A type this crate could mint would make that
/// instruction advisory: any code path here could authorize the very run the
/// ruling withheld.
///
/// So the field is private and `sealed_for_witness`
/// is `pub(crate)`. Outside this crate the variant that carries it is
/// unconstructible, which means [`RunAuthorization::Parked`] is the only
/// authorization an external caller — including the operator automation and the
/// integration suite — can ever produce.
///
/// The witnesses for the authorized path therefore live in `src/`, not in
/// `tests/`. An integration test cannot reach a `pub(crate)` constructor at all,
/// so a sealed path witnessed only from `tests/` would be witnessed nowhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttendedWindowReceipt {
    announced_window: String,
}

impl AttendedWindowReceipt {
    /// The announced window this receipt records.
    pub fn announced_window(&self) -> &str {
        &self.announced_window
    }

    /// Mint a receipt for an in-crate witness only.
    ///
    /// `pub(crate)` **and** `#[cfg(test)]` on purpose. The visibility keeps it
    /// out of every other crate; the `cfg` means that in any non-test build this
    /// type has no constructor at all, so [`RunAuthorization::Scheduled`] is
    /// unconstructible everywhere — not merely inconvenient to construct. That
    /// is the strongest form of "the tier is declared and its execution is
    /// withheld" this crate can express.
    ///
    /// The lane that eventually schedules a window opens the seam by editing
    /// this file, which is a reviewed law change rather than a call somebody
    /// makes. The witness for the open branch lives in this module's own tests
    /// because a `cfg(test)` route in `src/` is unreachable from `tests/`.
    #[cfg(test)]
    pub(crate) fn sealed_for_witness(announced_window: impl Into<String>) -> Self {
        Self {
            announced_window: announced_window.into(),
        }
    }
}

/// Whether a run may proceed past its announcement.
///
/// Two variants, and only one of them is reachable from outside this crate. See
/// [`AttendedWindowReceipt`] for why that asymmetry is the mechanism rather than
/// an accident.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunAuthorization {
    /// Declared, and not authorized to execute.
    Parked(ParkReason),
    /// A scheduled, announced attended window.
    Scheduled(AttendedWindowReceipt),
}

impl RunAuthorization {
    /// Whether this authorization admits execution past the announcement phase.
    pub fn admits_execution(&self) -> bool {
        match self {
            RunAuthorization::Parked(_) => false,
            RunAuthorization::Scheduled(_) => true,
        }
    }

    /// The park reason, if this authorization is parked.
    pub fn park_reason(&self) -> Option<ParkReason> {
        match self {
            RunAuthorization::Parked(reason) => Some(*reason),
            RunAuthorization::Scheduled(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_tier_label_round_trips_and_the_set_is_closed() {
        for tier in EvidenceTier::ALL {
            assert_eq!(
                EvidenceTier::from_label(tier.label()),
                Some(tier),
                "a tier label must name the tier it came from"
            );
        }
        assert_eq!(
            EvidenceTier::ALL.len(),
            3,
            "adding an evidence tier is a reviewed law change, not a new label"
        );
    }

    #[test]
    fn every_park_reason_label_round_trips() {
        for reason in ParkReason::ALL {
            assert_eq!(
                ParkReason::from_label(reason.label()),
                Some(reason),
                "a park reason label must name the reason it came from"
            );
        }
    }

    #[test]
    fn only_the_attended_tier_carries_caps_of_its_own() {
        assert_eq!(
            TierCaps::for_tier(EvidenceTier::AttendedFleetHost),
            Some(TierCaps::ATTENDED),
            "the attended tier's caps are this crate's to state"
        );
        assert_eq!(
            TierCaps::for_tier(EvidenceTier::LocalFocused),
            None,
            "inventing caps for a tier this crate does not own would be claiming its authority"
        );
        assert_eq!(
            TierCaps::for_tier(EvidenceTier::GovernedLinuxProfile),
            None,
            "the governed profile's custody is remote-compute's, not this crate's"
        );
    }

    #[test]
    fn the_attended_caps_are_the_exact_ruled_numbers() {
        let caps = TierCaps::ATTENDED;
        assert_eq!(caps.whole_run_ms().get(), 5_400_000, "90 minutes");
        assert_eq!(caps.per_matrix_ms().get(), 1_200_000, "20 minutes");
        assert_eq!(caps.logical_processors().get(), 4, "4 of 16 logical");
        assert_eq!(caps.run_tree_bytes().get(), 4_294_967_296, "4 GiB");
        assert_eq!(caps.host_free_floor_bytes().get(), 4_294_967_296, "4 GiB");
        assert_eq!(caps.engine_processes().get(), 2, "exactly two engines");
    }

    #[test]
    fn the_per_matrix_cap_leaves_room_for_reversal_and_cleanup() {
        assert!(
            TierCaps::ATTENDED.leaves_room_for_cleanup(),
            "a cap that can be satisfied only by skipping cleanup is not a cap"
        );
    }

    #[test]
    fn only_the_attended_tier_requires_attendance() {
        assert!(EvidenceTier::AttendedFleetHost.requires_attendance());
        assert!(!EvidenceTier::LocalFocused.requires_attendance());
        assert!(!EvidenceTier::GovernedLinuxProfile.requires_attendance());
    }

    #[test]
    fn the_governed_profile_is_the_only_tier_off_the_fleet_host() {
        assert!(!EvidenceTier::GovernedLinuxProfile.shares_the_fleet_host());
        assert!(EvidenceTier::AttendedFleetHost.shares_the_fleet_host());
        assert!(EvidenceTier::LocalFocused.shares_the_fleet_host());
    }

    /// The sealed branch, exercised from `src/` because `tests/` cannot reach it.
    ///
    /// This is the whole reason the witness lives here: `sealed_for_witness` is
    /// `pub(crate)`, so an integration test could not construct a `Scheduled`
    /// authorization even to prove it behaves correctly. Witnessing it from
    /// `tests/` would witness nothing.
    #[test]
    fn a_parked_authorization_refuses_execution_and_a_scheduled_one_admits_it() {
        let parked = RunAuthorization::Parked(ParkReason::AttendedWindowNotScheduled);
        assert!(
            !parked.admits_execution(),
            "the ruling encoded the tier and withheld its execution"
        );
        assert_eq!(
            parked.park_reason(),
            Some(ParkReason::AttendedWindowNotScheduled)
        );

        let scheduled = RunAuthorization::Scheduled(AttendedWindowReceipt::sealed_for_witness(
            "2026-08-14T18:00Z/PT90M",
        ));
        assert!(
            scheduled.admits_execution(),
            "a scheduled window is the only thing that admits execution"
        );
        assert_eq!(
            scheduled.park_reason(),
            None,
            "a scheduled run is not parked, so it has no park reason to report"
        );
        assert_eq!(
            match &scheduled {
                RunAuthorization::Scheduled(receipt) => receipt.announced_window(),
                RunAuthorization::Parked(_) => "",
            },
            "2026-08-14T18:00Z/PT90M",
            "the receipt carries the announced window verbatim"
        );
    }
}
