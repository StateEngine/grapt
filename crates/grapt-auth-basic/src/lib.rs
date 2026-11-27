//! Bounded HTTP Basic parsing and verification.
//!
//! The parser owns malformed-input refusal before a caller can grow any decoded
//! value. Credential material is never exposed through `Debug`, `Display`, or
//! an accessor; verification returns only the username after both fields match.

pub const MAX_HEADER_BYTES: usize = 1024;
pub const MAX_USERNAME_BYTES: usize = 128;
pub const MAX_SECRET_BYTES: usize = 256;

const BASIC_PREFIX: &[u8] = b"Basic ";
const MAX_DECODED_BYTES: usize = MAX_USERNAME_BYTES + 1 + MAX_SECRET_BYTES;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BasicAuthConfigError {
    EmptyUsername,
    UsernameTooLarge { max: usize },
    EmptySecret,
    SecretTooLarge { max: usize },
}

impl core::fmt::Display for BasicAuthConfigError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::EmptyUsername => f.write_str("the Basic username must not be empty"),
            Self::UsernameTooLarge { max } => {
                write!(f, "the Basic username must be at most {max} bytes")
            }
            Self::EmptySecret => f.write_str("the Basic secret must not be empty"),
            Self::SecretTooLarge { max } => {
                write!(f, "the Basic secret must be at most {max} bytes")
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BasicAuthError {
    HeaderTooLarge { max: usize },
    Malformed,
    UnsupportedScheme,
    NonUtf8Identity,
    IdentityTooLarge { max: usize },
    Rejected,
}

impl core::fmt::Display for BasicAuthError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::HeaderTooLarge { max } => {
                write!(f, "the authorization header must be at most {max} bytes")
            }
            Self::Malformed => f.write_str("the Basic credential is malformed"),
            Self::UnsupportedScheme => f.write_str("only the Basic scheme is supported"),
            Self::NonUtf8Identity => f.write_str("the decoded identity is not UTF-8"),
            Self::IdentityTooLarge { max } => {
                write!(f, "the decoded identity must be at most {max} bytes")
            }
            Self::Rejected => f.write_str("the Basic credential did not verify"),
        }
    }
}

#[derive(Clone)]
pub struct BasicAuthVerifier {
    username: String,
    secret: String,
}

impl core::fmt::Debug for BasicAuthVerifier {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("BasicAuthVerifier")
            .field("username", &"<redacted>")
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl BasicAuthVerifier {
    pub fn new(
        username: impl Into<String>,
        secret: impl Into<String>,
    ) -> Result<Self, BasicAuthConfigError> {
        let username = username.into();
        let secret = secret.into();
        if username.is_empty() {
            return Err(BasicAuthConfigError::EmptyUsername);
        }
        if username.len() > MAX_USERNAME_BYTES {
            return Err(BasicAuthConfigError::UsernameTooLarge {
                max: MAX_USERNAME_BYTES,
            });
        }
        if secret.is_empty() {
            return Err(BasicAuthConfigError::EmptySecret);
        }
        if secret.len() > MAX_SECRET_BYTES {
            return Err(BasicAuthConfigError::SecretTooLarge {
                max: MAX_SECRET_BYTES,
            });
        }
        Ok(Self { username, secret })
    }

    pub fn verify(&self, header: &[u8]) -> Result<String, BasicAuthError> {
        if header.len() > MAX_HEADER_BYTES {
            return Err(BasicAuthError::HeaderTooLarge {
                max: MAX_HEADER_BYTES,
            });
        }
        if header.len() < BASIC_PREFIX.len()
            || !eq_ascii_ignore_case(&header[..BASIC_PREFIX.len()], BASIC_PREFIX)
        {
            return Err(if starts_with_scheme_word(header) {
                BasicAuthError::UnsupportedScheme
            } else {
                BasicAuthError::Malformed
            });
        }

        let encoded = &header[BASIC_PREFIX.len()..];
        let mut decoded = [0u8; MAX_DECODED_BYTES];
        let decoded_len = decode_bounded(encoded, &mut decoded)?;
        let decoded = core::str::from_utf8(&decoded[..decoded_len])
            .map_err(|_| BasicAuthError::NonUtf8Identity)?;
        let Some((username, secret)) = decoded.split_once(':') else {
            return Err(BasicAuthError::Malformed);
        };
        if username.is_empty() || username.len() > MAX_USERNAME_BYTES {
            return Err(BasicAuthError::Malformed);
        }
        let username_matches = matches_constant_time(username.as_bytes(), self.username.as_bytes());
        let secret_matches = matches_constant_time(secret.as_bytes(), self.secret.as_bytes());
        if !username_matches || !secret_matches {
            return Err(BasicAuthError::Rejected);
        }
        Ok(username.to_owned())
    }
}

fn eq_ascii_ignore_case(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len()
        && left
            .iter()
            .zip(right)
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn starts_with_scheme_word(header: &[u8]) -> bool {
    const SCHEMES: [&[u8]; 3] = [b"Bearer", b"Digest", b"Negotiate"];
    SCHEMES.iter().any(|scheme| {
        header.len() >= scheme.len()
            && eq_ascii_ignore_case(&header[..scheme.len()], scheme)
            && header
                .get(scheme.len())
                .is_some_and(|byte| byte.is_ascii_whitespace())
    })
}

fn decode_bounded(encoded: &[u8], output: &mut [u8]) -> Result<usize, BasicAuthError> {
    if encoded.is_empty() || !encoded.len().is_multiple_of(4) {
        return Err(BasicAuthError::Malformed);
    }

    let mut written = 0usize;
    for (index, quantum) in encoded.chunks_exact(4).enumerate() {
        let last = index + 1 == encoded.len() / 4;
        let padding = quantum
            .iter()
            .rev()
            .take_while(|byte| **byte == b'=')
            .count();
        if (!last && padding != 0) || padding > 2 {
            return Err(BasicAuthError::Malformed);
        }
        let data_len = 4 - padding;
        if quantum[..data_len].contains(&b'=') {
            return Err(BasicAuthError::Malformed);
        }

        let mut values = [0u8; 4];
        for (value, byte) in values.iter_mut().zip(&quantum[..data_len]) {
            *value = decode_six_bits(*byte)?;
        }
        let produced = data_len - 1;
        if written + produced > output.len() {
            return Err(BasicAuthError::IdentityTooLarge {
                max: MAX_DECODED_BYTES,
            });
        }
        output[written] = (((values[0] as u16) << 2 | (values[1] as u16) >> 4) & 0xff) as u8;
        if produced > 1 {
            output[written + 1] =
                (((values[1] as u16) << 4 | (values[2] as u16) >> 2) & 0xff) as u8;
        }
        if produced > 2 {
            output[written + 2] = (((values[2] as u16) << 6 | (values[3] as u16)) & 0xff) as u8;
        }
        written += produced;

        if padding != 0 {
            let unused_bits = padding * 2;
            let mask = if unused_bits >= u8::BITS as usize {
                0
            } else {
                (1 << unused_bits) - 1
            };
            if values[data_len - 1] & mask != 0 {
                return Err(BasicAuthError::Malformed);
            }
        }
    }
    Ok(written)
}

fn decode_six_bits(byte: u8) -> Result<u8, BasicAuthError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'+' => Ok(62),
        b'/' => Ok(63),
        _ => Err(BasicAuthError::Malformed),
    }
}

fn matches_constant_time(presented: &[u8], expected: &[u8]) -> bool {
    let mut difference = (presented.len() ^ expected.len()) as u8;
    let bound = presented.len().max(expected.len());
    for index in 0..bound {
        let presented_byte = presented.get(index).copied().unwrap_or(0);
        let expected_byte = expected.get(index).copied().unwrap_or(0);
        difference |= presented_byte ^ expected_byte;
    }
    difference == 0 && presented.len() == expected.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn verifier() -> BasicAuthVerifier {
        match BasicAuthVerifier::new("alpha", "zephyr") {
            Ok(value) => value,
            Err(error) => panic!("fixture verifier was refused: {error}"),
        }
    }

    #[test]
    fn verifies_a_canonical_basic_header_without_exposing_the_secret() {
        let verifier = verifier();
        let verified = verifier.verify(b"Basic YWxwaGE6emVwaHly");
        assert_eq!(verified, Ok("alpha".to_owned()));
        assert!(!format!("{verifier:?}").contains("zephyr"));
    }

    #[test]
    fn rejects_malformed_unsupported_non_utf8_and_duplicate_shaped_inputs() {
        let verifier = verifier();
        for (header, expected) in [
            (b"Basic".as_slice(), BasicAuthError::Malformed),
            (b"Basic YWxwaGE6emVwaHly=", BasicAuthError::Malformed),
            (b"Basic YWxwaGE6emVwaHlyA===", BasicAuthError::Malformed),
            (
                b"Bearer YWxwaGE6emVwaHly",
                BasicAuthError::UnsupportedScheme,
            ),
            (&b" Bic"[..], BasicAuthError::Malformed),
            (&b"Basic /w=="[..], BasicAuthError::NonUtf8Identity),
        ] {
            assert_eq!(
                verifier.verify(header),
                Err(expected),
                "unexpected result for hostile Basic input"
            );
        }
    }

    #[test]
    fn bounds_the_header_before_decoding_or_identity_growth() {
        let verifier = verifier();
        let oversized = [b'A'; MAX_HEADER_BYTES + 1];
        assert_eq!(
            verifier.verify(&oversized),
            Err(BasicAuthError::HeaderTooLarge {
                max: MAX_HEADER_BYTES
            })
        );
        let long_quantums = &b"A".repeat(MAX_HEADER_BYTES)[..];
        let mut long_header = Vec::new();
        long_header.extend_from_slice(b"Basic ");
        long_header.extend_from_slice(long_quantums);
        assert!(verifier.verify(&long_header).is_err());
    }

    #[test]
    fn rejects_wrong_username_and_secret_with_one_generic_result() {
        let verifier = verifier();
        assert_eq!(
            verifier.verify(b"Basic YmV0YTp6ZXBoeXI="),
            Err(BasicAuthError::Rejected)
        );
        assert_eq!(
            verifier.verify(b"Basic YWxwaGE6d3Jvbmc="),
            Err(BasicAuthError::Rejected)
        );
    }

    #[test]
    fn rejects_noncanonical_unused_bits_when_one_padding_byte_is_present() {
        let verifier = match BasicAuthVerifier::new("alpha", "gusty") {
            Ok(value) => value,
            Err(error) => panic!("one-padding fixture verifier was refused: {error}"),
        };
        assert_eq!(
            verifier.verify(b"Basic YWxwaGE6Z3VzdHl="),
            Err(BasicAuthError::Malformed)
        );
    }

    #[test]
    fn rejects_noncanonical_unused_bits_when_two_padding_bytes_are_present() {
        let verifier = match BasicAuthVerifier::new("alpha", "wind") {
            Ok(value) => value,
            Err(error) => panic!("two-padding fixture verifier was refused: {error}"),
        };
        assert_eq!(
            verifier.verify(b"Basic YWxwaGE6d2luZE=="),
            Err(BasicAuthError::Malformed)
        );
    }

    #[test]
    fn refuses_invalid_configuration() {
        for error in [
            match BasicAuthVerifier::new("", "secret") {
                Err(error) => error,
                Ok(_) => panic!("empty username was admitted"),
            },
            match BasicAuthVerifier::new("a".repeat(MAX_USERNAME_BYTES + 1), "secret") {
                Err(error) => error,
                Ok(_) => panic!("oversized username was admitted"),
            },
            match BasicAuthVerifier::new("alpha", "") {
                Err(error) => error,
                Ok(_) => panic!("empty secret was admitted"),
            },
            match BasicAuthVerifier::new("alpha", "s".repeat(MAX_SECRET_BYTES + 1)) {
                Err(error) => error,
                Ok(_) => panic!("oversized secret was admitted"),
            },
        ] {
            assert!(!format!("{error:?}").contains('\u{0}'));
        }
    }
}
