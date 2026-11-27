//! Optional read acceleration with the same commit boundary as durable facts.
//!
//! Preparation sees the writer's transaction. Publication is infallible and
//! occurs only after COMMIT succeeds, before the mutation returns. Implementors
//! must prepare bounded immutable data, represent preparation failure as an
//! unavailable accelerator, and retain no connection or transaction borrow.

use std::fmt::Debug;
use std::sync::Arc;

use grapt_event::EventKind;
/// The borrowed connection already exposed by the store's query interface.
/// Naming the callback's argument does not require a new consumer dependency.
pub use rusqlite::Connection as ProjectionConnection;

/// A fully allocated candidate; publication must only replace shared read data.
pub trait PreparedReadProjection: Debug + Send {
    fn publish(self: Box<Self>);
}

#[cfg(test)]
#[path = "commit_projection_tests.rs"]
mod tests;

/// Domain queries stay with their owner; the store owns commit ordering.
pub trait CommittedReadProjection: Debug + Send + Sync {
    fn affected(&self, kind: EventKind) -> bool;
    fn prepare(&self, connection: &ProjectionConnection) -> Box<dyn PreparedReadProjection>;
}

impl crate::EventStore {
    /// Reconstructs before serving, then observes this store's governed commits.
    /// A projection cannot refuse startup or a durable write on capacity grounds.
    pub fn install_read_projection(&mut self, projection: Arc<dyn CommittedReadProjection>) {
        projection.prepare(self.connection()).publish();
        self.read_projection = Some(projection);
    }
}
