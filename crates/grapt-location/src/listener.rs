//! Socket acquisition: turning a validated [`ListenerBindPlan`] into a single
//! owned listener plus a truthful receipt.
//!
//! # What a receipt is, and what it is not
//!
//! [`BindReceipt`] records two facts that are routinely confused: the intent
//! the caller stated, and the address the operating system actually gave back.
//! For [`BindPort::Ephemeral`] those differ by construction — the request is
//! port zero and the answer never is — so a receipt that echoed the request
//! would be worse than no receipt, because it would look authoritative while
//! being wrong in exactly the case anyone would consult it.
//!
//! A receipt is emphatically NOT a readiness claim. A bound socket means one
//! thing: this process holds this address. It does not mean an HTTP server
//! exists, that a router is composed, or that any request can be served.
//! Readiness belongs to the API host lane and is an out-of-band receipt there.
//!
//! # One owner, and cleanup you can observe
//!
//! [`BoundListener`] is not `Clone`. There is exactly one owner of the socket,
//! which is what lets a later host take it by value and makes reuse by the
//! caller a compile error rather than a runtime surprise.
//!
//! Cleanup has two paths and they are not equivalent. [`BoundListener::close`]
//! is the explicit, observable one and returns a [`ClosedListener`]. `Drop` is
//! a backstop: it releases the socket, but it performs no I/O beyond that and
//! produces no receipt, because durable finalization that needs I/O may not be
//! assumed to complete inside `Drop`. Both paths are separately witnessed —
//! neither is allowed to stand in as evidence for the other.
//!
//! # Failures are classified structurally
//!
//! Every operating-system failure is classified from [`io::ErrorKind`]. No
//! branch in this module inspects an error's text. OS messages are localized,
//! versioned and platform-specific; a string match against one is a defect
//! waiting for a different machine.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

use thiserror::Error;
use tokio::net::{TcpListener, TcpStream};

use crate::config::{
    BindHost, BindPort, ListenerBindPlan, NameResolver, ResolveError, ResolvedAddresses,
};

/// A typed bind failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BindError {
    /// The plan's host could not be turned into candidate addresses.
    #[error("the bind plan could not be resolved: {0}")]
    Resolve(#[from] ResolveError),
    /// Another socket already holds this address.
    #[error("{address} is already in use")]
    AddressInUse {
        /// The address that was refused.
        address: SocketAddr,
    },
    /// The operating system refused the bind on authority grounds.
    #[error("binding {address} was refused: permission denied")]
    PermissionDenied {
        /// The address that was refused.
        address: SocketAddr,
    },
    /// The address is not assigned to this host.
    #[error("{address} is not available on this host")]
    AddressNotAvailable {
        /// The address that was refused.
        address: SocketAddr,
    },
    /// Any other I/O failure, carried by kind.
    #[error("binding {address} failed with I/O kind {kind:?}")]
    Io {
        /// The address that was refused.
        address: SocketAddr,
        /// The classified kind. Never matched from OS text.
        kind: io::ErrorKind,
    },
    /// The attempt bound was reached while candidates remained.
    ///
    /// Distinct from the classified failure of a final candidate: this says the
    /// plan ran out of permission to keep trying, not that the last address was
    /// unusable.
    #[error("the bind attempt bound of {limit} was reached after {attempted} attempt(s)")]
    AttemptsExhausted {
        /// How many attempts were actually made.
        attempted: usize,
        /// The bound that stopped them.
        limit: usize,
        /// The kind the most recent attempt failed with, if one was made.
        last: Option<io::ErrorKind>,
    },
    /// The socket bound, but its local address could not be read back.
    ///
    /// A listener whose address cannot be observed cannot produce a truthful
    /// receipt, so this fails rather than reporting the requested address.
    #[error("the listener bound but its local address could not be observed: {kind:?}")]
    Observation {
        /// The classified kind.
        kind: io::ErrorKind,
    },
}

/// Classifies one operating-system bind failure by kind alone.
fn classify(address: SocketAddr, kind: io::ErrorKind) -> BindError {
    match kind {
        io::ErrorKind::AddrInUse => BindError::AddressInUse { address },
        io::ErrorKind::PermissionDenied => BindError::PermissionDenied { address },
        io::ErrorKind::AddrNotAvailable => BindError::AddressNotAvailable { address },
        other => BindError::Io {
            address,
            kind: other,
        },
    }
}

/// What was asked for, and what was actually obtained.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BindReceipt {
    requested_host: BindHost,
    requested_port: BindPort,
    observed: SocketAddr,
}

impl BindReceipt {
    fn new(plan: &ListenerBindPlan, observed: SocketAddr) -> Self {
        Self {
            requested_host: plan.host().clone(),
            requested_port: plan.port(),
            observed,
        }
    }

    /// The host the caller asked for.
    pub fn requested_host(&self) -> &BindHost {
        &self.requested_host
    }

    /// The port intent the caller asked for.
    ///
    /// Survives binding unchanged. An ephemeral plan that received port 51234
    /// still reports [`BindPort::Ephemeral`] here, because the intent is what
    /// the caller stated and the observed address is where it landed.
    pub fn requested_port(&self) -> BindPort {
        self.requested_port
    }

    /// The address the operating system actually assigned.
    pub fn observed(&self) -> SocketAddr {
        self.observed
    }
}

/// The receipt of an explicit close.
///
/// Returned only by [`BoundListener::close`]. Dropping a listener produces no
/// `ClosedListener`, which is the point: an explicit shutdown is observable and
/// a backstop is not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ClosedListener {
    receipt: BindReceipt,
}

impl ClosedListener {
    /// The bind receipt of the listener that was closed.
    pub fn receipt(&self) -> &BindReceipt {
        &self.receipt
    }
}

/// A typed accept failure.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum AcceptError {
    /// The listener held no socket to accept on.
    ///
    /// Unreachable through the public API — [`BoundListener::close`] consumes
    /// the owner, so a closed listener cannot be accepted on — and stated as a
    /// typed refusal rather than a panic or a fabricated I/O kind. Its witness
    /// is the in-crate unit test below, which constructs the state directly.
    #[error("the listener bound to {local} holds no socket and can accept nothing")]
    Closed {
        /// The address the listener had been bound to.
        local: SocketAddr,
    },
    /// The operating system refused the accept, carried by kind.
    #[error("accepting on {local} failed with I/O kind {kind:?}")]
    Io {
        /// The address the accept was attempted on.
        local: SocketAddr,
        /// The classified kind. Never matched from OS text.
        kind: io::ErrorKind,
    },
}

/// One admitted inbound connection, and the two addresses it was admitted on.
///
/// Not `Clone`: a connection has one owner for the same reason a listener does.
/// `local` is copied from the listener's OBSERVED address, never from what was
/// requested — an ephemeral plan requests port zero and no connection is ever
/// admitted on port zero.
#[derive(Debug)]
pub struct AcceptedConnection {
    stream: TcpStream,
    peer: SocketAddr,
    local: SocketAddr,
}

impl AcceptedConnection {
    /// The address of the peer that connected.
    pub fn peer(&self) -> SocketAddr {
        self.peer
    }

    /// The observed local address this connection was admitted on.
    pub fn local(&self) -> SocketAddr {
        self.local
    }

    /// Hands the stream to whatever will serve it.
    ///
    /// Consumes the connection: the caller that serves the bytes is the owner,
    /// and this crate keeps no second handle to it. What protocol is spoken
    /// afterwards is not this crate's concern.
    pub fn into_stream(self) -> TcpStream {
        self.stream
    }
}

/// Sole ownership of one acquired listening socket.
///
/// Deliberately not `Clone`: two owners of one socket cannot both be
/// responsible for closing it, and a later host takes this by value.
#[derive(Debug)]
pub struct BoundListener {
    listener: Option<TcpListener>,
    receipt: BindReceipt,
}

impl BoundListener {
    fn new(listener: TcpListener, receipt: BindReceipt) -> Self {
        Self {
            listener: Some(listener),
            receipt,
        }
    }

    /// What was requested and what was obtained.
    pub fn receipt(&self) -> &BindReceipt {
        &self.receipt
    }

    /// Releases the socket explicitly and returns the receipt of the close.
    ///
    /// Consumes the owner, so the socket cannot be used afterwards. The
    /// subsequent `Drop` sees nothing left to release.
    pub fn close(mut self) -> ClosedListener {
        drop(self.listener.take());
        ClosedListener {
            receipt: self.receipt.clone(),
        }
    }

    /// Admits exactly one inbound connection.
    ///
    /// This is the whole seam a host lane needs, and deliberately no more. It
    /// borrows `&self`, so this owner keeps the listener and its receipt: there
    /// is no raw accessor, no `into_inner`, and no way to obtain a second
    /// accepting owner. Nothing here clones, retries, spawns, reads a clock, or
    /// knows what protocol the connection will speak.
    ///
    /// Exactly one connection, exactly one await. A caller that wants a serving
    /// loop writes the loop; a loop hidden in here would own a scheduling policy
    /// this crate has no business holding, and would decide on the caller's
    /// behalf which failures are worth retrying.
    pub async fn accept(&self) -> Result<AcceptedConnection, AcceptError> {
        let local = self.receipt.observed();
        let Some(listener) = self.listener.as_ref() else {
            return Err(AcceptError::Closed { local });
        };
        match listener.accept().await {
            Ok((stream, peer)) => Ok(AcceptedConnection {
                stream,
                peer,
                local,
            }),
            Err(error) => Err(AcceptError::Io {
                local,
                kind: error.kind(),
            }),
        }
    }
}

impl Drop for BoundListener {
    fn drop(&mut self) {
        // Backstop for the owner that was never closed explicitly. It releases
        // the socket and does nothing else: no receipt, no logging, no durable
        // write. `Drop` cannot be relied on to complete I/O, so nothing that
        // must be observed is placed here.
        drop(self.listener.take());
    }
}

/// Acquires listeners for validated plans, using an injected resolver.
#[derive(Clone, Debug)]
pub struct ListenerBinder {
    resolver: Arc<dyn NameResolver>,
}

impl ListenerBinder {
    /// Binds using `resolver` for every named host.
    pub fn new(resolver: Arc<dyn NameResolver>) -> Self {
        Self { resolver }
    }

    /// Turns a plan's host into bounded candidates.
    ///
    /// A literal address reaches no resolver at all. That is a real property
    /// and not an optimisation: a loopback plan must not acquire a dependency
    /// on name resolution being installed, configured or reachable.
    async fn candidates(&self, plan: &ListenerBindPlan) -> Result<ResolvedAddresses, BindError> {
        let limit = plan.limits().max_resolved_addresses();
        match plan.host() {
            BindHost::Ip(address) => Ok(ResolvedAddresses::new(vec![*address], limit)?),
            BindHost::Name(name) => Ok(self.resolver.resolve(name, limit).await?),
        }
    }

    /// Acquires a listener for `plan`, or fails with a typed reason.
    ///
    /// Candidates are tried in resolution order, and never more than
    /// `max_bind_attempts` of them.
    pub async fn bind(&self, plan: &ListenerBindPlan) -> Result<BoundListener, BindError> {
        let candidates = self.candidates(plan).await?;
        let limit = plan.limits().max_bind_attempts().get();
        let mut attempted = 0usize;
        let mut last: Option<io::ErrorKind> = None;

        for address in candidates.as_slice() {
            if attempted == limit {
                return Err(BindError::AttemptsExhausted {
                    attempted,
                    limit,
                    last,
                });
            }
            attempted += 1;
            let requested = SocketAddr::new(*address, plan.port().requested());
            match TcpListener::bind(requested).await {
                Ok(listener) => {
                    let observed = match listener.local_addr() {
                        Ok(observed) => observed,
                        Err(error) => {
                            return Err(BindError::Observation { kind: error.kind() });
                        }
                    };
                    return Ok(BoundListener::new(
                        listener,
                        BindReceipt::new(plan, observed),
                    ));
                }
                Err(error) => {
                    last = Some(error.kind());
                    if attempted == candidates.len() {
                        return Err(classify(requested, error.kind()));
                    }
                }
            }
        }

        // Unreachable while `ResolvedAddresses` refuses an empty list, but
        // stated as a typed outcome rather than an `unwrap` on `last`.
        Err(BindError::AttemptsExhausted {
            attempted,
            limit,
            last,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{IpAddr, Ipv4Addr};
    use std::num::NonZeroU16;

    /// Witnesses [`AcceptError::Closed`], which the public API cannot reach.
    ///
    /// `close` consumes the owner, so no caller can accept on a closed
    /// listener. The variant exists so the state is a typed refusal rather than
    /// a panic or a fabricated I/O kind, and this constructs that state directly
    /// to prove the refusal is real rather than decorative. Without this the
    /// variant would be a kind with no producer, which is not a boundary.
    #[tokio::test]
    async fn a_listener_holding_no_socket_refuses_to_accept() {
        let local = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 1);
        let Some(port) = NonZeroU16::new(1) else {
            panic!("1 is a valid port");
        };
        let listener = BoundListener {
            listener: None,
            receipt: BindReceipt {
                requested_host: BindHost::Ip(IpAddr::V4(Ipv4Addr::LOCALHOST)),
                requested_port: BindPort::Fixed(port),
                observed: local,
            },
        };
        match listener.accept().await {
            Err(error) => assert_eq!(
                error,
                AcceptError::Closed { local },
                "a listener with no socket must refuse by kind, naming the address it had"
            ),
            Ok(_) => panic!("a listener holding no socket must accept nothing"),
        }
    }
}
