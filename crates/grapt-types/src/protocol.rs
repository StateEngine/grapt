//! Protocol compatibility identity. Law (foundational report row
//! `protocol`/`tool`): "generated DTO/tool schemas are checked projections,
//! never hand-maintained competing truth." A [`ProtocolVersion`] is the explicit
//! compatibility identity a generated protocol/DTO layer is checked against — it
//! is never inferred from the shape of a payload.

/// Codex wire compatibility version for ChatGPT model discovery and inference.
/// Tracks OpenAI Codex CLI 0.159.0, independently of Grapt's package version:
/// the backend filters models by `minimal_client_version`.
pub const CODEX_COMPATIBILITY_VERSION: &str = "0.159.0";

crate::id::opaque_id! {
    /// An explicit protocol compatibility identity. Two peers agree only when
    /// their versions match exactly; compatibility is never guessed from a
    /// payload's fields.
    ProtocolVersion
}

#[cfg(test)]
mod tests {
    use super::ProtocolVersion;

    #[test]
    fn protocol_version_is_an_explicit_non_empty_identity() {
        assert!(ProtocolVersion::new("2026-08-08").is_some());
        assert!(ProtocolVersion::new("").is_none());
    }
}
