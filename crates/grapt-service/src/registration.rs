//! The registration file: the discovery contract, as exact bytes.
//!
//! # Why the bytes are the contract
//!
//! Master §"Service replacement lifecycle" makes one requirement that decides
//! this whole module: *"Registration commit is compare-and-swap against
//! registration-file contents, not a health-probe approximation."* A CAS over
//! *contents* is only as trustworthy as the relationship between those contents
//! and the owner they name. If two different byte sequences could decode to the
//! same record, a CAS that succeeded on bytes would have compared something
//! weaker than the record; if one byte sequence could decode to two records, it
//! would have compared something stronger. Either way the CAS would not mean
//! what the law says it means.
//!
//! So this encoding is **canonical in both directions**:
//!
//! * `encode` is a total function from a record to exactly one byte sequence;
//! * `decode` refuses every byte sequence its own encoder would not have
//!   produced.
//!
//! Together those give `encode(decode(b)?) == b` for every accepted `b`, which
//! is the property that makes byte-CAS and record-CAS the same operation.
//! `a_decoded_record_re_encodes_to_the_exact_original_bytes` holds it, and
//! `non_canonical_spellings_of_the_same_values_are_refused` holds the half
//! that is easy to lose: `generation=01` and `pid=+7` *parse* in Rust and would
//! decode to the same values as their canonical spellings while being different
//! bytes. Accepting them would silently break the equivalence.
//!
//! # Carriage returns are refused, loudly
//!
//! This file is written and read by a controller that runs on Windows, and a
//! registration that round-tripped through anything line-aware would come back
//! CRLF-terminated. Under a byte-CAS that is not a cosmetic difference: the
//! incumbent's own registration would stop matching itself, every CAS would
//! report a lost race, and the slot would look permanently contended by a
//! phantom. Refusing `\r` with its exact offset turns that into one typed
//! decode failure instead of an unexplained livelock.
//!
//! # What a malformed record may never become
//!
//! Every refusal below lands on "this slot names no owner I can read", never on
//! "this slot names some other owner". That direction is the whole point: a
//! torn, truncated or corrupted registration must be reclaimable through the
//! explicit recovery path on [`ServiceController`](crate::ServiceController), and must never be decodable
//! as a *different* live owner, because that is the one error that would let a
//! reused PID or a stale generation inherit a slot.

use grapt_types::authority::Timestamp;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};
use thiserror::Error;

/// The exact first line of every registration file.
///
/// A version header rather than a bare field list so that a future change to
/// *what the record covers* is a visible refusal against old bytes rather than
/// a silent reinterpretation of them.
///
/// This is also the extension seam the D2C addendum
/// asks this contract to leave open. A later
/// lane that must record how a *retained* engine maps into this identity scheme
/// adds its fields under a new header, and every reader of that record either
/// understands the new version or refuses it by name. What it may not do is
/// widen the identity: the addendum is explicit that PID, URL and version are
/// never R4D-1 identity, and this record keeps all three where
/// [`OwnerReceipt`] already puts them — as attributes travelling beside
/// `{ServiceSlotId, ServiceInstanceId, EnvironmentReceipt}`, never inside it.
const HEADER: &str = "secode-service/registration/1";

/// Maximum bytes in one textual registration field.
///
/// Checked before a record is constructed and before a decoded value is
/// copied into an owned identity type. The bound is deliberately about UTF-8
/// bytes, which are what the file and its CAS actually retain.
pub const MAX_REGISTRATION_FIELD_BYTES: usize = 1_024;

/// Maximum bytes in the complete registration file.
///
/// The file store applies this before allocating for a read, and the record
/// constructor proves its encoder cannot cross it.
pub const MAX_REGISTRATION_BYTES: usize = 8_512;

/// The field keys, in the exact order they must appear.
///
/// Order is fixed rather than tolerated so that there is exactly one byte
/// sequence per record. A decoder that accepted any order would admit N!
/// spellings of one record and break the CAS equivalence above.
const KEYS: [&str; 8] = [
    "slot",
    "instance",
    "environment",
    "generation",
    "pid",
    "endpoint",
    "version",
    "registered_at",
];

/// Why a byte sequence is not a registration record.
///
/// Each variant is a distinct law with its own message. They are separate
/// because a witness for one must not be able to pass by tripping another —
/// and because the offsets and names carried here are what make a corrupted
/// registration diagnosable rather than merely rejected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistrationDecodeError {
    /// The complete file crossed the allocation/record ceiling.
    #[error("registration is {found} bytes, above the finite {maximum}-byte record bound")]
    RecordTooLong { maximum: usize, found: usize },
    /// A carriage return appeared. Under a byte-CAS this would make the
    /// incumbent's registration stop matching itself.
    #[error(
        "registration byte {at} is a carriage return: this record has been rewritten by something \
         line-aware, and under a contents compare-and-swap it would no longer match itself"
    )]
    CarriageReturn { at: usize },
    /// The bytes are not UTF-8.
    #[error("registration is not UTF-8, so it names no owner that can be read")]
    NotUtf8,
    /// The trailing newline is missing.
    #[error(
        "registration does not end with exactly one newline: a record without its terminator may \
         be a truncated write and is never read as a shorter record"
    )]
    MissingTrailingNewline,
    /// The wrong number of lines.
    #[error(
        "registration has {found} lines, not the exact {expected} this version defines: a record \
         with a missing or extra field is refused rather than partially read"
    )]
    WrongLineCount { expected: usize, found: usize },
    /// The header line is not this version's header.
    #[error(
        "registration header is {found:?}, not {expected:?}: these bytes were written by a \
         different version of this contract and are not reinterpreted under this one"
    )]
    WrongHeader {
        expected: &'static str,
        found: String,
    },
    /// A field line carried no `=`.
    #[error("registration line {line} carries no `=`, so it declares no field at all")]
    NotAField { line: usize },
    /// A field appeared out of the fixed order.
    #[error(
        "registration line {line} declares field {found:?} where {expected:?} is required: field \
         order is fixed so that one record has exactly one spelling"
    )]
    WrongField {
        line: usize,
        expected: &'static str,
        found: String,
    },
    /// A field contains a byte reserved by the line encoding.
    #[error(
        "registration field {field:?} contains reserved byte {byte:?} at byte {at}: CR, LF and = \
         are never embedded in a canonical field value"
    )]
    ReservedFieldByte {
        field: &'static str,
        byte: char,
        at: usize,
    },
    /// A field crossed its finite byte ceiling.
    #[error(
        "registration field {field:?} is {found} bytes, above the finite {maximum}-byte field bound"
    )]
    FieldTooLong {
        field: &'static str,
        maximum: usize,
        found: usize,
    },
    /// A value failed its own type's validating constructor.
    #[error(
        "registration field {field:?} holds {value:?}, which its own type refuses: an identity \
         this build cannot construct is never approximated by a string"
    )]
    InvalidValue { field: &'static str, value: String },
    /// A numeric value was spelled in a way this encoder would not produce.
    #[error(
        "registration field {field:?} holds the non-canonical numeral {value:?}, which this \
         encoder would render as {canonical:?}: two spellings of one value would break the \
         equivalence between a contents compare-and-swap and a record comparison"
    )]
    NonCanonicalNumber {
        field: &'static str,
        value: String,
        canonical: String,
    },
}

/// Why a constructible owner/version cannot enter the canonical registration
/// domain.
///
/// The underlying identity types intentionally accept opaque strings. This
/// boundary is therefore fallible: delimiters and oversized values are
/// rejected here, before the encoder can create bytes its decoder refuses.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RegistrationBuildError {
    #[error(
        "registration field {field:?} contains reserved byte {byte:?} at byte {at}: CR, LF and = \
         are outside the canonical registration domain"
    )]
    ReservedFieldByte {
        field: &'static str,
        byte: char,
        at: usize,
    },
    #[error(
        "registration field {field:?} is {found} bytes, above the finite {maximum}-byte field bound"
    )]
    FieldTooLong {
        field: &'static str,
        maximum: usize,
        found: usize,
    },
    #[error(
        "the canonical registration would be {found} bytes, above the finite {maximum}-byte record bound"
    )]
    RecordTooLong { maximum: usize, found: usize },
}

/// The durable discovery record for one registration slot.
///
/// It carries the landed owner identity verbatim — this crate mints no identity
/// of its own — plus the registration's own version and the instant it was
/// written. The owner is an [`OwnerReceipt`], so slot, instance, environment,
/// generation, PID and endpoint all travel under the identity law that already
/// governs them: the first three *are* the owner, and the last three are
/// attributes a probe may check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistrationRecord {
    owner: OwnerReceipt,
    version: ServiceRegistrationVersion,
    registered_at: Timestamp,
}

impl RegistrationRecord {
    pub fn new(
        owner: OwnerReceipt,
        version: ServiceRegistrationVersion,
        registered_at: Timestamp,
    ) -> Result<Self, RegistrationBuildError> {
        let record = Self {
            owner,
            version,
            registered_at,
        };
        for (field, value) in record.text_values() {
            validate_build_field(field, value)?;
        }
        let found = record.encoded_len();
        if found > MAX_REGISTRATION_BYTES {
            return Err(RegistrationBuildError::RecordTooLong {
                maximum: MAX_REGISTRATION_BYTES,
                found,
            });
        }
        Ok(record)
    }

    pub fn owner(&self) -> &OwnerReceipt {
        &self.owner
    }

    pub fn version(&self) -> &ServiceRegistrationVersion {
        &self.version
    }

    pub fn registered_at(&self) -> Timestamp {
        self.registered_at
    }

    /// The one byte sequence that represents this record.
    pub fn encode(&self) -> Vec<u8> {
        let values = self.encoded_values();
        let mut out = String::with_capacity(self.encoded_len());
        out.push_str(HEADER);
        out.push('\n');
        for (key, value) in KEYS.iter().zip(values.iter()) {
            out.push_str(key);
            out.push('=');
            out.push_str(value);
            out.push('\n');
        }
        out.into_bytes()
    }

    fn text_values(&self) -> [(&'static str, &str); 5] {
        [
            (KEYS[0], self.owner.slot().as_str()),
            (KEYS[1], self.owner.instance().as_str()),
            (KEYS[2], self.owner.environment().as_str()),
            (KEYS[5], self.owner.endpoint().as_str()),
            (KEYS[6], self.version.as_str()),
        ]
    }

    fn encoded_values(&self) -> [String; 8] {
        [
            self.owner.slot().as_str().to_owned(),
            self.owner.instance().as_str().to_owned(),
            self.owner.environment().as_str().to_owned(),
            self.owner.generation().value().to_string(),
            self.owner.pid().value().to_string(),
            self.owner.endpoint().as_str().to_owned(),
            self.version.as_str().to_owned(),
            self.registered_at.as_millis().to_string(),
        ]
    }

    fn encoded_len(&self) -> usize {
        HEADER.len()
            + 1
            + KEYS
                .iter()
                .zip(self.encoded_values())
                .map(|(key, value)| key.len() + 1 + value.len() + 1)
                .sum::<usize>()
    }

    /// Read a record, or say exactly why these bytes are not one.
    ///
    /// Refuses everything [`encode`](Self::encode) would not have produced. See
    /// the module note for why that strictness is load-bearing rather than
    /// fastidious.
    pub fn decode(bytes: &[u8]) -> Result<Self, RegistrationDecodeError> {
        if bytes.len() > MAX_REGISTRATION_BYTES {
            return Err(RegistrationDecodeError::RecordTooLong {
                maximum: MAX_REGISTRATION_BYTES,
                found: bytes.len(),
            });
        }
        // Before anything else, and before UTF-8: a carriage return is the
        // failure most likely to be mistaken for an ordinary mismatch, so it is
        // reported by offset rather than swept into a generic parse error.
        if let Some(at) = bytes.iter().position(|byte| *byte == b'\r') {
            return Err(RegistrationDecodeError::CarriageReturn { at });
        }
        let text = core::str::from_utf8(bytes).map_err(|_| RegistrationDecodeError::NotUtf8)?;
        let Some(body) = text.strip_suffix('\n') else {
            return Err(RegistrationDecodeError::MissingTrailingNewline);
        };
        // After stripping the single terminator, a well-formed record splits
        // into the header plus one line per field with no empty tail. A second
        // trailing newline therefore produces an empty final line and is caught
        // here rather than silently tolerated.
        let expected_lines = KEYS.len() + 1;
        let found_lines = body.bytes().filter(|byte| *byte == b'\n').count() + 1;
        if found_lines != expected_lines {
            return Err(RegistrationDecodeError::WrongLineCount {
                expected: expected_lines,
                found: found_lines,
            });
        }
        let mut lines = body.split('\n');
        let Some(header) = lines.next() else {
            return Err(RegistrationDecodeError::WrongLineCount {
                expected: expected_lines,
                found: 0,
            });
        };
        if header != HEADER {
            return Err(RegistrationDecodeError::WrongHeader {
                expected: HEADER,
                found: header.to_owned(),
            });
        }
        let mut values = [""; KEYS.len()];
        for (index, key) in KEYS.iter().enumerate() {
            let line_number = index + 1;
            let Some(line) = lines.next() else {
                return Err(RegistrationDecodeError::WrongLineCount {
                    expected: expected_lines,
                    found: line_number,
                });
            };
            let Some((found_key, value)) = line.split_once('=') else {
                return Err(RegistrationDecodeError::NotAField { line: line_number });
            };
            if found_key != *key {
                return Err(RegistrationDecodeError::WrongField {
                    line: line_number,
                    expected: key,
                    found: found_key.to_owned(),
                });
            }
            validate_decoded_field(key, value)?;
            values[index] = value;
        }

        let slot = ServiceSlotId::new(values[0]).ok_or_else(|| invalid(KEYS[0], values[0]))?;
        let instance =
            ServiceInstanceId::new(values[1]).ok_or_else(|| invalid(KEYS[1], values[1]))?;
        let environment =
            EnvironmentReceipt::new(values[2]).ok_or_else(|| invalid(KEYS[2], values[2]))?;
        let generation = ServiceGeneration::new(canonical_u64(KEYS[3], values[3])?);
        let pid = ProcessId::new(canonical_u32(KEYS[4], values[4])?)
            .ok_or_else(|| invalid(KEYS[4], values[4]))?;
        let endpoint = Endpoint::new(values[5]).ok_or_else(|| invalid(KEYS[5], values[5]))?;
        let version = ServiceRegistrationVersion::new(values[6])
            .ok_or_else(|| invalid(KEYS[6], values[6]))?;
        let registered_at = Timestamp::from_millis(canonical_i64(KEYS[7], values[7])?);

        Self::new(
            OwnerReceipt::new(slot, instance, environment, generation, pid, endpoint),
            version,
            registered_at,
        )
        .map_err(build_as_decode)
    }
}

fn reserved_byte(value: &str) -> Option<(usize, char)> {
    value
        .char_indices()
        .find(|(_, character)| matches!(character, '\r' | '\n' | '='))
}

fn validate_build_field(field: &'static str, value: &str) -> Result<(), RegistrationBuildError> {
    if value.len() > MAX_REGISTRATION_FIELD_BYTES {
        return Err(RegistrationBuildError::FieldTooLong {
            field,
            maximum: MAX_REGISTRATION_FIELD_BYTES,
            found: value.len(),
        });
    }
    if let Some((at, byte)) = reserved_byte(value) {
        return Err(RegistrationBuildError::ReservedFieldByte { field, byte, at });
    }
    Ok(())
}

fn validate_decoded_field(field: &'static str, value: &str) -> Result<(), RegistrationDecodeError> {
    if value.len() > MAX_REGISTRATION_FIELD_BYTES {
        return Err(RegistrationDecodeError::FieldTooLong {
            field,
            maximum: MAX_REGISTRATION_FIELD_BYTES,
            found: value.len(),
        });
    }
    if let Some((at, byte)) = reserved_byte(value) {
        return Err(RegistrationDecodeError::ReservedFieldByte { field, byte, at });
    }
    Ok(())
}

fn build_as_decode(error: RegistrationBuildError) -> RegistrationDecodeError {
    match error {
        RegistrationBuildError::ReservedFieldByte { field, byte, at } => {
            RegistrationDecodeError::ReservedFieldByte { field, byte, at }
        }
        RegistrationBuildError::FieldTooLong {
            field,
            maximum,
            found,
        } => RegistrationDecodeError::FieldTooLong {
            field,
            maximum,
            found,
        },
        RegistrationBuildError::RecordTooLong { maximum, found } => {
            RegistrationDecodeError::RecordTooLong { maximum, found }
        }
    }
}

fn invalid(field: &'static str, value: &str) -> RegistrationDecodeError {
    RegistrationDecodeError::InvalidValue {
        field,
        value: value.to_owned(),
    }
}

/// Parse a numeral only if it is spelled the way this encoder spells it.
///
/// Rust's integer parser accepts `+7` and `007`, both of which would decode to
/// the same value as `7` from different bytes. The round trip is what is
/// checked — not a hand-written charset rule — so the acceptance test is
/// exactly "would `encode` have written this?" and cannot drift away from the
/// encoder it is defending.
macro_rules! canonical_int {
    ($fn_name:ident, $ty:ty) => {
        fn $fn_name(field: &'static str, value: &str) -> Result<$ty, RegistrationDecodeError> {
            let parsed: $ty = value.parse().map_err(|_| invalid(field, value))?;
            let canonical = parsed.to_string();
            if canonical != value {
                return Err(RegistrationDecodeError::NonCanonicalNumber {
                    field,
                    value: value.to_owned(),
                    canonical,
                });
            }
            Ok(parsed)
        }
    };
}

canonical_int!(canonical_u64, u64);
canonical_int!(canonical_u32, u32);
canonical_int!(canonical_i64, i64);

#[cfg(test)]
mod tests {
    use super::*;

    type R = Result<(), Box<dyn std::error::Error>>;

    fn record(instance: &str, generation: u64, pid: u32) -> Option<RegistrationRecord> {
        RegistrationRecord::new(
            OwnerReceipt::new(
                ServiceSlotId::new("slot-1")?,
                ServiceInstanceId::new(instance)?,
                EnvironmentReceipt::new("env-1")?,
                ServiceGeneration::new(generation),
                ProcessId::new(pid)?,
                Endpoint::new("http://127.0.0.1:8080")?,
            ),
            ServiceRegistrationVersion::new("0.1.0")?,
            Timestamp::from_millis(1_700_000_000_000),
        )
        .ok()
    }

    #[test]
    fn a_decoded_record_re_encodes_to_the_exact_original_bytes() -> R {
        let original = record("inst-1", 3, 4242).ok_or("record")?;
        let bytes = original.encode();
        let decoded = RegistrationRecord::decode(&bytes)?;
        assert_eq!(
            decoded, original,
            "decode must recover the record it was given"
        );
        assert_eq!(
            decoded.encode(),
            bytes,
            "a contents compare-and-swap is only a record comparison when the round trip is exact"
        );
        Ok(())
    }

    #[test]
    fn non_canonical_spellings_of_the_same_values_are_refused() -> R {
        // Each of these PARSES in Rust and would yield exactly the values the
        // canonical record holds, from different bytes. Accepting any one of
        // them would put two byte sequences behind one record.
        let bytes = record("inst-1", 3, 4242).ok_or("record")?.encode();
        let text = String::from_utf8(bytes)?;
        for (from, to, field) in [
            ("generation=3", "generation=03", "generation"),
            ("generation=3", "generation=+3", "generation"),
            ("pid=4242", "pid=+4242", "pid"),
            (
                "registered_at=1700000000000",
                "registered_at=+1700000000000",
                "registered_at",
            ),
        ] {
            let mutated = text.replace(from, to);
            assert_ne!(
                mutated, text,
                "the {field} rewrite must actually change the bytes"
            );
            match RegistrationRecord::decode(mutated.as_bytes()) {
                Err(RegistrationDecodeError::NonCanonicalNumber { field: got, .. }) => {
                    assert_eq!(got, field, "the refusal must name the field it rejected");
                }
                other => {
                    return Err(format!(
                        "the non-canonical {field} spelling {to:?} was not refused as \
                         non-canonical: {other:?}"
                    )
                    .into())
                }
            }
        }
        Ok(())
    }

    #[test]
    fn a_carriage_return_is_refused_by_offset_rather_than_as_a_mismatch() -> R {
        let bytes = record("inst-1", 1, 7).ok_or("record")?.encode();
        let text = String::from_utf8(bytes)?;
        let crlf = text.replace('\n', "\r\n");
        match RegistrationRecord::decode(crlf.as_bytes()) {
            Err(RegistrationDecodeError::CarriageReturn { at }) => {
                assert_eq!(
                    at,
                    HEADER.len(),
                    "the reported offset must be the first carriage return, which follows the header"
                );
            }
            other => {
                return Err(format!(
                    "a CRLF-rewritten registration decoded instead of being refused: {other:?}"
                )
                .into())
            }
        }
        Ok(())
    }

    #[test]
    fn every_corruption_refuses_rather_than_naming_a_different_owner() -> R {
        let original = record("inst-1", 1, 7).ok_or("record")?;
        let bytes = original.encode();
        let text = String::from_utf8(bytes.clone())?;

        // Truncation at every byte boundary: the crash cut a partial write
        // leaves behind. Not one prefix may decode to a record, because a
        // shorter record that still parsed would name an owner nobody wrote.
        for cut in 0..bytes.len() {
            let prefix = &bytes[..cut];
            assert!(
                RegistrationRecord::decode(prefix).is_err(),
                "the {cut}-byte prefix of a registration decoded as a record"
            );
        }

        // Structural corruptions, each landing on "no readable owner".
        let cases: [(&str, String); 6] = [
            ("a second trailing newline", format!("{text}\n")),
            ("a dropped field", text.replace("version=0.1.0\n", "")),
            (
                "a reordered pair",
                text.replace(
                    "slot=slot-1\ninstance=inst-1\n",
                    "instance=inst-1\nslot=slot-1\n",
                ),
            ),
            (
                "a foreign header",
                text.replace(HEADER, "secode-service/registration/2"),
            ),
            ("a field line with no `=`", text.replace("pid=7\n", "pid\n")),
            (
                "an empty value",
                text.replace("instance=inst-1\n", "instance=\n"),
            ),
        ];
        for (name, mutated) in cases {
            assert_ne!(
                mutated, text,
                "the {name} case must actually change the bytes"
            );
            let decoded = RegistrationRecord::decode(mutated.as_bytes());
            assert!(
                decoded.is_err(),
                "{name} produced a readable record instead of a refusal: {decoded:?}"
            );
        }
        Ok(())
    }

    #[test]
    fn every_constructible_record_round_trips_and_reserved_bytes_are_unconstructible() -> R {
        let instances = [
            "i".to_owned(),
            "instance-with-unicode-λ".to_owned(),
            "x".repeat(MAX_REGISTRATION_FIELD_BYTES),
        ];
        for instance in &instances {
            let owner = OwnerReceipt::new(
                ServiceSlotId::new("slot-1").ok_or("slot")?,
                ServiceInstanceId::new(instance).ok_or("instance")?,
                EnvironmentReceipt::new("env-1").ok_or("environment")?,
                ServiceGeneration::new(u64::MAX),
                ProcessId::new(u32::MAX).ok_or("pid")?,
                Endpoint::new("http://127.0.0.1:8080").ok_or("endpoint")?,
            );
            let original = RegistrationRecord::new(
                owner,
                ServiceRegistrationVersion::new("0.1.0").ok_or("version")?,
                Timestamp::from_millis(i64::MIN),
            )?;
            assert_eq!(RegistrationRecord::decode(&original.encode())?, original);
        }
        for reserved in ['\r', '\n', '='] {
            let value = format!("instance{reserved}tail");
            let owner = OwnerReceipt::new(
                ServiceSlotId::new("slot-1").ok_or("slot")?,
                ServiceInstanceId::new(value).ok_or("instance")?,
                EnvironmentReceipt::new("env-1").ok_or("environment")?,
                ServiceGeneration::FIRST,
                ProcessId::new(7).ok_or("pid")?,
                Endpoint::new("http://127.0.0.1:8080").ok_or("endpoint")?,
            );
            assert!(matches!(
                RegistrationRecord::new(
                    owner,
                    ServiceRegistrationVersion::new("0.1.0").ok_or("version")?,
                    Timestamp::from_millis(-1),
                ),
                Err(RegistrationBuildError::ReservedFieldByte { byte, .. }) if byte == reserved
            ));
        }
        Ok(())
    }

    #[test]
    fn construction_refuses_a_field_before_it_exceeds_the_declared_byte_bound() -> R {
        let oversized = "x".repeat(MAX_REGISTRATION_FIELD_BYTES + 1);
        let owner = OwnerReceipt::new(
            ServiceSlotId::new("slot-1").ok_or("slot")?,
            ServiceInstanceId::new(oversized).ok_or("instance")?,
            EnvironmentReceipt::new("env-1").ok_or("environment")?,
            ServiceGeneration::FIRST,
            ProcessId::new(7).ok_or("pid")?,
            Endpoint::new("http://127.0.0.1:8080").ok_or("endpoint")?,
        );
        assert!(matches!(
            RegistrationRecord::new(
                owner,
                ServiceRegistrationVersion::new("0.1.0").ok_or("version")?,
                Timestamp::from_millis(0),
            ),
            Err(RegistrationBuildError::FieldTooLong {
                field: "instance",
                maximum: MAX_REGISTRATION_FIELD_BYTES,
                found,
            }) if found == MAX_REGISTRATION_FIELD_BYTES + 1
        ));
        Ok(())
    }
}
