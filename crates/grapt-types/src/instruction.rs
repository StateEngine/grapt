//! Instruction identity. Law: `PRESERVE` — blobs once, changed hashes in
//! durable events.

use std::fmt;

crate::id::opaque_id! {
    /// Validated instruction source key.
    InstructionKey
}

/// SHA-256 digest of instruction content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct InstructionHash([u8; 32]);

impl InstructionHash {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstructionHashParseError {
    InvalidLength { actual: usize },
    InvalidHex { at: usize },
}

impl fmt::Display for InstructionHashParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength { actual } => {
                write!(
                    f,
                    "instruction hash must be 64 hexadecimal bytes, got {actual}"
                )
            }
            Self::InvalidHex { at } => {
                write!(f, "instruction hash contains invalid hex at byte {at}")
            }
        }
    }
}

impl std::error::Error for InstructionHashParseError {}

impl std::str::FromStr for InstructionHash {
    type Err = InstructionHashParseError;

    fn from_str(raw: &str) -> Result<Self, Self::Err> {
        if raw.len() != 64 {
            return Err(InstructionHashParseError::InvalidLength { actual: raw.len() });
        }
        let mut bytes = [0_u8; 32];
        for (index, pair) in raw.as_bytes().chunks_exact(2).enumerate() {
            let high = hex_nibble(pair[0])
                .ok_or(InstructionHashParseError::InvalidHex { at: index * 2 })?;
            let low = hex_nibble(pair[1])
                .ok_or(InstructionHashParseError::InvalidHex { at: index * 2 + 1 })?;
            bytes[index] = high << 4 | low;
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for InstructionHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Monotonic instruction epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct InstructionEpoch(u64);

impl InstructionEpoch {
    pub const FIRST: InstructionEpoch = InstructionEpoch(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn epochs_are_ordered() {
        assert!(InstructionEpoch(1) < InstructionEpoch(2));
    }

    #[test]
    fn hash_round_trips_strict_hex() {
        let raw = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
        let parsed = raw.parse::<InstructionHash>();
        assert!(parsed.is_ok());
        assert_eq!(parsed.map(|hash| hash.to_string()).as_deref(), Ok(raw));
        assert!("00".parse::<InstructionHash>().is_err());
        assert!(
            "zc2f24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b982"
                .parse::<InstructionHash>()
                .is_err()
        );
    }
}
