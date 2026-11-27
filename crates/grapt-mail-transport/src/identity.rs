//! Public compatibility path for the shared validated remote-mail vocabulary.
//! Authenticated handles and DTO decoding remain private to this mail crate.
pub use grapt_types::remote_mail::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identity_refuses_empty_and_oversize_and_never_truncates(
    ) -> Result<(), Box<dyn std::error::Error>> {
        assert!(matches!(
            SourceBindingId::try_new(""),
            Err(IdentityError::Empty { .. })
        ));
        let at_bound = "b".repeat(MAX_TRANSPORT_IDENTITY_BYTES);
        assert!(SourceBindingId::try_new(at_bound).is_ok());
        let over = "b".repeat(MAX_TRANSPORT_IDENTITY_BYTES + 1);
        match SourceBindingId::try_new(over) {
            Err(IdentityError::TooLarge { bytes, max, .. }) => {
                assert_eq!(bytes, MAX_TRANSPORT_IDENTITY_BYTES + 1);
                assert_eq!(max, MAX_TRANSPORT_IDENTITY_BYTES);
            }
            other => panic!("an oversize identity must refuse, never truncate: {other:?}"),
        }
        Ok(())
    }

    #[test]
    fn the_identity_bound_is_utf8_bytes_not_characters() -> Result<(), Box<dyn std::error::Error>> {
        // Four-byte characters make chars and bytes disagree; the bound counts
        // BYTES, which is what a wire limit actually constrains.
        let wide = "\u{1F600}".repeat(MAX_TRANSPORT_IDENTITY_BYTES / 4);
        let id = ExclusiveSourceCursor::try_new(wide)
            .map_err(|e| format!("at the byte bound: {e:?}"))?;
        assert_eq!(id.as_str().len(), MAX_TRANSPORT_IDENTITY_BYTES);
        assert!(id.as_str().chars().count() < MAX_TRANSPORT_IDENTITY_BYTES);

        let over = "\u{1F600}".repeat(MAX_TRANSPORT_IDENTITY_BYTES / 4 + 1);
        assert!(ExclusiveSourceCursor::try_new(over).is_err());
        Ok(())
    }

    #[test]
    fn a_same_project_pair_still_carries_both_halves() -> Result<(), Box<dyn std::error::Error>> {
        let pair = ProjectPair::new(
            SourceProjectId::try_new("stateengine")?,
            TargetProjectId::try_new("stateengine")?,
        );
        assert!(pair.is_same_project());
        // J8: reporting sameness must not tempt anyone to drop a half.
        assert_eq!(pair.source().as_str(), "stateengine");
        assert_eq!(pair.target().as_str(), "stateengine");
        Ok(())
    }

    #[test]
    fn a_cross_project_pair_keeps_its_direction() -> Result<(), Box<dyn std::error::Error>> {
        let pair = ProjectPair::new(
            SourceProjectId::try_new("stateengine")?,
            TargetProjectId::try_new("secode")?,
        );
        assert!(!pair.is_same_project());
        assert_ne!(pair.source().as_str(), pair.target().as_str());
        Ok(())
    }
}
