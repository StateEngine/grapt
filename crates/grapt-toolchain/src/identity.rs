//! The installed identity record: the only vouch for a binary on disk.
//!
//! Presence of a binary is NOT evidence of provenance. A binary is
//! attributable only when the record beside it decodes strictly AND matches
//! the CURRENT pin (the match law below) AND the bytes on disk still hash
//! to the record's binary digest. An undecodable record is never a vouch:
//! it does not refuse the attempt, it simply does not count, and the
//! activation re-acquires.

use serde::{Deserialize, Serialize};

use crate::pin::Pin;
use crate::value::{Sha256, SourceUrl, TargetTriple, ToolName, ToolVersion};

/// The identity record bound: 4 KiB.
pub const IDENTITY_MAX_BYTES: usize = 4096;

/// The installed identity: nine fields, strictly decoded, and typed.
///
/// Keeping the typed values — rather than decoding to strings and dropping
/// them back — is what makes the match law below type-checked: the record
/// can only hold a name, a version, a target and a source that the contract
/// already admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledIdentity {
    pub tool: ToolName,
    pub version: ToolVersion,
    pub platform: String,
    pub arch: String,
    pub target: TargetTriple,
    pub source: SourceUrl,
    pub archive_sha256: Sha256,
    pub binary_sha256: Sha256,
    /// Milliseconds since epoch, from the injected clock.
    pub installed_at_ms: u64,
}

/// The strict wire form: every field present, no foreign field.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityWire {
    tool: String,
    version: String,
    platform: String,
    arch: String,
    target: String,
    source: String,
    archive_sha256: String,
    binary_sha256: String,
    installed_at_ms: u64,
}

impl InstalledIdentity {
    /// Strict decode: the bound, then the shape, then every field through
    /// its typed value. Any failure means "no vouch", not a refusal — the
    /// caller re-acquires.
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > IDENTITY_MAX_BYTES {
            return Err("identity record exceeds the 4 KiB bound".to_owned());
        }
        let wire: IdentityWire = serde_json::from_slice(bytes)
            .map_err(|error| format!("identity record is not a strict record: {error}"))?;
        let tool =
            ToolName::new(&wire.tool).ok_or_else(|| "identity tool is out of bound".to_owned())?;
        let version = ToolVersion::new(&wire.version)
            .ok_or_else(|| "identity version is out of bound".to_owned())?;
        let target = TargetTriple::new(&wire.target)
            .ok_or_else(|| "identity target is out of bound".to_owned())?;
        let source = SourceUrl::new(&wire.source)
            .ok_or_else(|| "identity source is out of bound".to_owned())?;
        let archive_sha256 = Sha256::from_hex(&wire.archive_sha256)
            .ok_or_else(|| "identity archive digest is not 64 lowercase hex".to_owned())?;
        let binary_sha256 = Sha256::from_hex(&wire.binary_sha256)
            .ok_or_else(|| "identity binary digest is not 64 lowercase hex".to_owned())?;
        Ok(Self {
            tool,
            version,
            platform: wire.platform,
            arch: wire.arch,
            target,
            source,
            archive_sha256,
            binary_sha256,
            installed_at_ms: wire.installed_at_ms,
        })
    }

    /// The canonical encoding: exactly the nine fields.
    pub fn encode(&self) -> Result<String, serde_json::Error> {
        let wire = IdentityWire {
            tool: self.tool.as_str().to_owned(),
            version: self.version.as_str().to_owned(),
            platform: self.platform.clone(),
            arch: self.arch.clone(),
            target: self.target.as_str().to_owned(),
            source: self.source.as_str().to_owned(),
            archive_sha256: self.archive_sha256.hex(),
            binary_sha256: self.binary_sha256.hex(),
            installed_at_ms: self.installed_at_ms,
        };
        serde_json::to_string(&wire)
    }

    /// The match law: an existing activation is reusable only when ALL of
    /// these hold —
    ///
    /// 1. the record decoded strictly (a precondition of having a Self);
    /// 2. `tool`, `version`, `platform`, `arch`, `target`, `source` AND
    ///    `archive_sha256` all equal the current pin;
    /// 3. the caller separately verifies the canonical bytes hash to
    ///    `binary_sha256`.
    ///
    /// Version and target alone never match: revoking a compromised digest
    /// or upstream re-cutting a release changes the pin while leaving
    /// version and target identical, and a record written under the old pin
    /// must not keep the old binary alive.
    pub fn matches(&self, pin: &Pin, platform: &str, arch: &str) -> bool {
        self.tool == pin.tool
            && self.version == pin.version
            && self.platform == platform
            && self.arch == arch
            && self.target == pin.target
            && self.source == pin.source
            && self.archive_sha256 == pin.archive_sha256
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::value::{ArchiveFormat, EntryName};

    /// A fixture parse must succeed or the test fails loudly: the fixtures
    /// are literals, so a failure is a fixture defect, not a skip.
    fn must<T>(name: &str, value: Option<T>) -> T {
        match value {
            Some(value) => value,
            None => panic!("fixture {name} failed to parse"),
        }
    }

    fn pin_for_test() -> Pin {
        Pin {
            tool: must("tool", ToolName::new("ripgrep")),
            version: must("version", ToolVersion::new("15.1.0")),
            target: must("target", TargetTriple::new("x86_64-pc-windows-msvc")),
            source: must("source", SourceUrl::new("archives/rg.tar")),
            archive_sha256: Sha256::of(b"archive-bytes"),
            format: ArchiveFormat::Tar,
            entry: must("entry", EntryName::new("rg.exe")),
        }
    }

    fn identity_for_test() -> InstalledIdentity {
        let pin = pin_for_test();
        InstalledIdentity {
            tool: pin.tool.clone(),
            version: pin.version.clone(),
            platform: "windows".to_owned(),
            arch: "x86_64".to_owned(),
            target: pin.target.clone(),
            source: pin.source.clone(),
            archive_sha256: pin.archive_sha256,
            binary_sha256: Sha256::of(b"binary-bytes"),
            installed_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn the_round_trip_is_exact() {
        let identity = identity_for_test();
        let encoded = identity.encode().unwrap_or_default();
        let decoded = InstalledIdentity::decode(encoded.as_bytes())
            .unwrap_or_else(|e| panic!("round trip failed: {e}"));
        assert_eq!(decoded, identity);
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap_or_default();
        let keys = value.as_object().map(|o| o.len());
        assert_eq!(keys, Some(9));
        assert_eq!(value["schema"], serde_json::Value::Null);
    }

    #[test]
    fn a_foreign_or_missing_field_is_undecodable() {
        let identity = identity_for_test();
        let text = identity.encode().unwrap_or_default();
        let missing = text.replace("\"arch\"", "\"missing\"");
        assert!(InstalledIdentity::decode(missing.as_bytes()).is_err());
        let foreign = text.replace("}", ", \"surprise\": 1}");
        assert!(InstalledIdentity::decode(foreign.as_bytes()).is_err());
        let oversized = format!("{{\"pad\": \"{}\"}}", "x".repeat(IDENTITY_MAX_BYTES));
        assert!(InstalledIdentity::decode(oversized.as_bytes()).is_err());
    }

    #[test]
    fn an_out_of_bound_field_is_undecodable() {
        let identity = identity_for_test();
        let text = identity.encode().unwrap_or_default();
        let bad_tool = text.replace("ripgrep", "RipGrep");
        assert!(InstalledIdentity::decode(bad_tool.as_bytes()).is_err());
        let bad_digest = text.replace(
            &identity.binary_sha256.hex(),
            &identity.binary_sha256.hex().to_uppercase(),
        );
        assert!(InstalledIdentity::decode(bad_digest.as_bytes()).is_err());
    }

    #[test]
    fn the_match_law_requires_every_pin_fact() {
        let pin = pin_for_test();
        let identity = identity_for_test();
        assert!(identity.matches(&pin, "windows", "x86_64"));

        // A re-cut: same version and target, changed archive digest.
        let recut = Pin {
            archive_sha256: Sha256::of(b"recut-archive-bytes"),
            ..pin.clone()
        };
        assert!(!identity.matches(&recut, "windows", "x86_64"));

        // A re-homed source at the same version and digest.
        let rehome = Pin {
            source: must("rehome-source", SourceUrl::new("archives/other.tar")),
            ..pin.clone()
        };
        assert!(!identity.matches(&rehome, "windows", "x86_64"));

        // Platform or arch drift: the record was written elsewhere.
        assert!(!identity.matches(&pin, "linux", "x86_64"));
        assert!(!identity.matches(&pin, "windows", "aarch64"));

        // A version change is an upgrade, not a reuse.
        let other_version = Pin {
            version: must("other-version", ToolVersion::new("15.1.1")),
            ..pin.clone()
        };
        assert!(!identity.matches(&other_version, "windows", "x86_64"));
    }
}
