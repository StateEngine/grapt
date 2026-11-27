//! The prompt bound and the validity rule the transcript and the composer
//! share.

/// The largest draft the composer accepts, in bytes.
pub const MAX_DRAFT: usize = 65_536;

pub fn valid_prompt(text: &str) -> bool {
    !text.trim().is_empty()
        && text.len() <= MAX_DRAFT
        && !text
            .chars()
            .any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t'))
}
