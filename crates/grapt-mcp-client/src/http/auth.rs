//! Credential acquisition is a product-owned port. This transport never opens
//! the credential database, discovers OAuth metadata, or launches a browser.
use super::HttpError;
use grapt_types::{McpLoginId, Secret};
use std::{future::Future, pin::Pin};

pub struct BearerLease {
    pub token: Secret<String>,
    pub generation: McpLoginId,
    pub revision: u64,
}
impl std::fmt::Debug for BearerLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BearerLease([REDACTED])")
    }
}
pub type BearerFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<BearerLease>, HttpError>> + Send + 'a>>;
pub trait BearerProvider: Send + Sync {
    /// A revision is supplied only after an explicit HTTP 401. Ambiguous
    /// network outcomes never authorize refreshing and replaying a POST.
    fn acquire(&self, rejected_revision: Option<u64>) -> BearerFuture<'_>;
}
