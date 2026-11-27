//! Provider-error-detail notices as system rows.
use super::IndexedRow;

/// Append every notice as its system row, oldest first.
pub(super) fn append(
    rows: &mut Vec<IndexedRow>,
    notices: Vec<grapt_session::notice::ProviderErrorDetailNotice>,
) -> Result<(), String> {
    for notice in notices {
        let (seq, id, raw) = crate::provider_error_text::notice_row(notice)?;
        rows.push(IndexedRow::rendered(seq, id, raw));
    }
    Ok(())
}
