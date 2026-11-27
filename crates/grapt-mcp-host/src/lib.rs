//! MCP transport, lifecycle, catalog projection and user-content conversion.
//! The application supplies roots, subscription effects, publication metadata,
//! live notifications and nested requests through explicit interfaces.
pub mod channel;
pub mod connection;
pub mod declaration;
pub mod entry;
pub mod publication;
pub mod startup;
pub mod startup_lease;
pub mod startup_trace;
pub mod timeouts;
pub mod user_content;

use serde_json::Value;

/// A nested server request keeps its asynchronous work scoped to the exchange.
pub type RequestFuture<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, String>> + Send + 'a>>;

/// Application-owned workspace disclosure for one approved connection.
pub trait WorkspaceRoots: std::fmt::Debug + Send + Sync {
    fn response(&self) -> Result<Value, String>;
    fn take_changed(&self) -> Result<bool, String>;
}

/// Application-owned refresh hints; the protocol channel remains the sole reader.
pub trait CatalogNotifications: std::fmt::Debug + Send + Sync {
    fn accept(&self, notification: &Value);
    fn clear(&self, kind: grapt_mcp::content::CatalogKind);
    fn changed(&self);
}
