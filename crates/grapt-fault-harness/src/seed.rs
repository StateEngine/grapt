//! Deterministic seeded PRNG for reproducible chaos schedules.

use std::fmt;

use serde::{Deserialize, Serialize};

/// A single 64-bit seed that uniquely determines an entire chaos run.
///
/// Recording this value in a failure transcript lets any chaos failure be
/// replayed identically on any platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct FaultSeed(u64);

impl FaultSeed {
    /// Construct a new seed from an explicit 64-bit integer.
    #[must_use]
    pub const fn new(raw: u64) -> Self {
        Self(raw)
    }

    /// Return the underlying 64-bit integer for logging or replay.
    #[must_use]
    pub const fn as_u64(self) -> u64 {
        self.0
    }

    /// Create a deterministic PRNG initialized from this seed.
    #[must_use]
    pub fn rng(self) -> SeededRng {
        SeededRng::from_seed(self)
    }

    /// Derive an independent deterministic child seed for a named subsystem.
    #[must_use]
    pub fn derive(self, label: &str) -> Self {
        // FNV-1a 64-bit mix over the label, then SplitMix64 avalanche.
        let mut hash = 0xcbf2_9ce4_8422_2325_u64 ^ self.0;
        for byte in label.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
        Self(splitmix64_next(&mut hash))
    }
}

impl fmt::Display for FaultSeed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "0x{:016x} ({})", self.0, self.0)
    }
}

/// Deterministic Xoshiro256** PRNG seeded via SplitMix64.
///
/// Produces identical output across architectures and operating systems for the
/// same [`FaultSeed`] without touching OS entropy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeededRng {
    state: [u64; 4],
}

impl SeededRng {
    /// Initialize from a [`FaultSeed`].
    #[must_use]
    pub fn from_seed(seed: FaultSeed) -> Self {
        let mut sm = seed.as_u64();
        let s0 = splitmix64_next(&mut sm);
        let s1 = splitmix64_next(&mut sm);
        let s2 = splitmix64_next(&mut sm);
        let s3 = splitmix64_next(&mut sm);
        Self {
            state: [s0, s1, s2, s3],
        }
    }

    /// Return the next deterministic 64-bit value.
    #[must_use]
    pub fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let t = self.state[1] << 17;

        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];

        self.state[2] ^= t;
        self.state[3] = self.state[3].rotate_left(45);

        result
    }

    /// Return a deterministic integer in `[0, upper_exclusive)`.
    ///
    /// Returns `0` when `upper_exclusive <= 1`.
    #[must_use]
    pub fn gen_range_u64(&mut self, upper_exclusive: u64) -> u64 {
        if upper_exclusive <= 1 {
            return 0;
        }
        let threshold = upper_exclusive.wrapping_neg() % upper_exclusive;
        loop {
            let value = self.next_u64();
            if value >= threshold {
                return value % upper_exclusive;
            }
        }
    }

    /// Return a deterministic `usize` in `[0, upper_exclusive)`.
    #[must_use]
    pub fn gen_range_usize(&mut self, upper_exclusive: usize) -> usize {
        if upper_exclusive <= 1 {
            return 0;
        }
        let Ok(upper) = u64::try_from(upper_exclusive) else {
            return 0;
        };
        usize::try_from(self.gen_range_u64(upper)).unwrap_or(0)
    }

    /// Return `true` with probability `bps / 10_000` (basis points).
    #[must_use]
    pub fn chance_bps(&mut self, bps: u16) -> bool {
        if bps == 0 {
            return false;
        }
        if bps >= 10_000 {
            return true;
        }
        self.gen_range_u64(10_000) < u64::from(bps)
    }

    /// Choose one element uniformly from `items`, or `None` if empty.
    #[must_use]
    pub fn choose<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        let idx = self.gen_range_usize(items.len());
        items.get(idx)
    }
}

fn splitmix64_next(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}
