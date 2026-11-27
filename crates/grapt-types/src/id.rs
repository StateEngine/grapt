//! Prefixed and opaque identity machinery.
//!
//! Oracle identities are ASCII strings `prefix_suffix` (e.g. `ses_04b1327e9ffe…`,
//! `per_fdc588081001TYtysR3tlCuXDO`): the suffix is non-empty ASCII
//! alphanumeric-or-underscore, mixed case observed in real rows. Underscore is
//! REAL: subagent-minted message ids use a compound `msg_sub_…` shape
//! (measured on the frozen clone, 8 distinct ids, 2026-08-08). Validation is
//! deliberately exactly that — no length or charset law the oracle does not
//! itself enforce.

use std::fmt;

/// Rejection reason for an identity parse. Carried, never stringly re-derived.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdError {
    WrongPrefix { expected: &'static str },
    EmptySuffix { expected: &'static str },
    InvalidChar { expected: &'static str, at: usize },
}

impl fmt::Display for IdError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            IdError::WrongPrefix { expected } => {
                write!(f, "identity must start with \"{expected}_\"")
            }
            IdError::EmptySuffix { expected } => {
                write!(f, "identity \"{expected}_\" has an empty suffix")
            }
            IdError::InvalidChar { expected, at } => {
                write!(
                    f,
                    "identity with prefix \"{expected}\" has a non-alphanumeric byte at {at}"
                )
            }
        }
    }
}

impl std::error::Error for IdError {}

pub(crate) fn parse_prefixed(prefix: &'static str, raw: &str) -> Result<String, IdError> {
    let rest = raw
        .strip_prefix(prefix)
        .and_then(|r| r.strip_prefix('_'))
        .ok_or(IdError::WrongPrefix { expected: prefix })?;
    if rest.is_empty() {
        return Err(IdError::EmptySuffix { expected: prefix });
    }
    if let Some(bad) = rest
        .bytes()
        .position(|b| !b.is_ascii_alphanumeric() && b != b'_')
    {
        return Err(IdError::InvalidChar {
            expected: prefix,
            at: prefix.len() + 1 + bad,
        });
    }
    Ok(raw.to_owned())
}

/// A validated `prefix_*` identity. One macro invocation per identity type —
/// each expansion is a distinct nominal type, so the compiler refuses a
/// `SessionId` where a `PermissionId` is expected (ownership probe #1).
macro_rules! prefixed_id {
    ($(#[$doc:meta])* $name:ident, $prefix:literal) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub const PREFIX: &'static str = $prefix;

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::str::FromStr for $name {
            type Err = $crate::id::IdError;
            fn from_str(raw: &str) -> Result<Self, Self::Err> {
                $crate::id::parse_prefixed($prefix, raw).map(Self)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

/// An opaque non-empty identity with no prefix law (the oracle treats the
/// value as a token, e.g. provider ids, execution ids).
macro_rules! opaque_id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(raw: impl Into<String>) -> Option<Self> {
                let raw = raw.into();
                if raw.is_empty() { None } else { Some(Self(raw)) }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str(&self.0)
            }
        }
    };
}

pub(crate) use opaque_id;
pub(crate) use prefixed_id;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_real_oracle_shapes() {
        assert!(parse_prefixed("ses", "ses_04b1327e9ffe").is_ok());
        assert!(parse_prefixed("per", "per_fdc588081001TYtysR3tlCuXDO").is_ok());
        // Subagent-minted compound message id — real clone shape.
        assert!(parse_prefixed("msg", "msg_sub_0390ce0dcffepFNqEmy2eSKVfR").is_ok());
    }

    #[test]
    fn rejects_wrong_prefix_empty_suffix_and_bad_bytes() {
        assert_eq!(
            parse_prefixed("ses", "msg_abc"),
            Err(IdError::WrongPrefix { expected: "ses" })
        );
        assert_eq!(
            parse_prefixed("ses", "ses_"),
            Err(IdError::EmptySuffix { expected: "ses" })
        );
        assert_eq!(
            parse_prefixed("ses", "ses_ab-cd"),
            Err(IdError::InvalidChar {
                expected: "ses",
                at: 6
            })
        );
    }
}
