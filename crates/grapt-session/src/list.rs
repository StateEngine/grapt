//! Typed bounded session-list projection, projector, and exact keyset query.

mod keyset;
mod projection;
mod query;
mod request;
mod row;

pub use projection::SessionListProjector;
// Crate-internal, not part of the list's public surface: the create path needs
// the projector's own placement rule so the two cannot disagree about a
// directory, but a consumer has no business canonicalizing one.
pub(crate) use projection::{location_of, validate_location};
pub use query::SessionListStore;
pub use request::{
    BoundaryKey, BoundedSearch, KeyDigest, NonNegativeMs, ParentFilter, SessionListBoundary,
    SessionListCriteria, SessionListDegradedRow, SessionListDirection, SessionListError,
    SessionListItem, SessionListKey, SessionListLimits, SessionListOrder, SessionListPage,
    SessionListProvenance, SessionListScope, MAX_PAGE_ROWS, MAX_PAGE_TEXT_BYTES,
    MAX_SEARCH_UTF8_BYTES,
};
