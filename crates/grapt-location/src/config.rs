//! Validated bind intent: exactly what a caller asked for, before any socket
//! exists.
//!
//! # Why the intent is a closed type rather than an option bag
//!
//! The frozen oracle at `42e4888cc0` carries three different bind
//! intents in one nullable integer: an absent port starts a bounded search from
//! 4096, an explicit `0` means "let the OS choose", and an explicit `1..=65535`
//! means that exact port. Those are three distinct decisions with three
//! distinct failure surfaces, and a single `Option<u16>` cannot keep them
//! apart — which is how "missing" and "zero" become the same bug twice.
//!
//! This module refuses the collapse. [`BindPort`] has exactly two inhabitants:
//! [`BindPort::Ephemeral`], the ONLY intent that asks the operating system for
//! port zero, and [`BindPort::Fixed`], which carries a [`NonZeroU16`] and
//! therefore cannot represent zero at all. The oracle's search-from-4096 policy
//! is deliberately absent: it is a later, explicitly bounded compatibility
//! adapter over this type, never the meaning of a missing value inside it.
//!
//! # Bounds are positive types, and there is no `Default`
//!
//! Under the bounded-everything rule no `Default` may create capacity, so
//! neither [`BindLimits`] nor its two fields implement it. A caller states its
//! bounds or it does not get a plan.
//!
//! # Name resolution is injected, never ambient
//!
//! Resolving a hostname is environmental I/O. A constructor that quietly called
//! the platform resolver would make every consumer's tests depend on DNS, so
//! resolution enters through the [`NameResolver`] seam. The production
//! implementation in this foundation, [`LiteralAddressResolver`], accepts
//! literal IP addresses only and answers every hostname with a typed
//! [`ResolveError::AdapterUnavailable`]. A real DNS adapter is a later lane;
//! until it exists, "we do not do this yet" is stated in the type system rather
//! than guessed at runtime.

use std::collections::BTreeMap;
use std::fmt;
use std::future::Future;
use std::io;
use std::net::IpAddr;
use std::num::{NonZeroU16, NonZeroUsize};
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};

use thiserror::Error;

/// The longest byte length a [`BindHostName`] may carry.
///
/// 253 is the maximum length of a presentation-format DNS name. It is a bound,
/// not a claim that every shorter string is resolvable.
pub const MAX_HOST_NAME_BYTES: usize = 253;

/// A validated, bounded hostname.
///
/// Construction is the only route in, so a `BindHostName` in hand has already
/// been length- and charset-checked. Holding one is not a claim that the name
/// resolves; that answer belongs to a [`NameResolver`].
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct BindHostName(String);

impl BindHostName {
    /// Validates `value` as a bounded hostname.
    pub fn new(value: &str) -> Result<Self, HostNameError> {
        if value.is_empty() {
            return Err(HostNameError::Empty);
        }
        let bytes = value.len();
        if bytes > MAX_HOST_NAME_BYTES {
            return Err(HostNameError::TooLong {
                bytes,
                limit: MAX_HOST_NAME_BYTES,
            });
        }
        for (index, byte) in value.bytes().enumerate() {
            if !(byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.') {
                return Err(HostNameError::InvalidByte { index, byte });
            }
        }
        Ok(Self(value.to_owned()))
    }

    /// The validated name.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BindHostName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a hostname was refused at construction.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum HostNameError {
    /// The empty string is not a hostname, and is not a sentinel for "default".
    #[error("a bind hostname cannot be empty")]
    Empty,
    /// The name is longer than [`MAX_HOST_NAME_BYTES`].
    #[error("a bind hostname of {bytes} bytes exceeds the bound of {limit}")]
    TooLong {
        /// The supplied length in bytes.
        bytes: usize,
        /// The bound that was exceeded.
        limit: usize,
    },
    /// A byte outside the accepted alphanumeric/`-`/`.` set.
    #[error("byte {byte:#04x} at index {index} is not legal in a bind hostname")]
    InvalidByte {
        /// Byte offset of the first offending byte.
        index: usize,
        /// The offending byte.
        byte: u8,
    },
}

/// Where to bind: a literal IP address, or a name that must be resolved.
///
/// The distinction is closed on purpose. A literal address needs no resolver
/// and reaches no environment; a name is I/O and must pass through the injected
/// [`NameResolver`] seam.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindHost {
    /// A literal IP address. No resolution is performed or required.
    Ip(IpAddr),
    /// A validated hostname requiring bounded resolution.
    Name(BindHostName),
}

impl BindHost {
    /// A literal-address host.
    pub fn ip(address: IpAddr) -> Self {
        BindHost::Ip(address)
    }

    /// A host named by a validated hostname.
    pub fn name(name: BindHostName) -> Self {
        BindHost::Name(name)
    }
}

impl fmt::Display for BindHost {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindHost::Ip(address) => write!(formatter, "{address}"),
            BindHost::Name(name) => write!(formatter, "{name}"),
        }
    }
}

/// Which port to ask the operating system for.
///
/// There is no third state. `Ephemeral` is the only intent that asks for port
/// zero, and `Fixed` cannot carry zero because [`NonZeroU16`] has no zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindPort {
    /// Ask the operating system to choose an unused port.
    Ephemeral,
    /// Ask for exactly this port.
    Fixed(NonZeroU16),
}

impl BindPort {
    /// The exact number handed to the operating system.
    ///
    /// Zero is produced here for exactly one reason — the caller asked for
    /// [`BindPort::Ephemeral`] — and never as a stand-in for a missing,
    /// defaulted or searched-for port.
    pub fn requested(self) -> u16 {
        match self {
            BindPort::Ephemeral => 0,
            BindPort::Fixed(port) => port.get(),
        }
    }

    /// Whether the operating system was asked to choose.
    pub fn is_ephemeral(self) -> bool {
        matches!(self, BindPort::Ephemeral)
    }
}

impl fmt::Display for BindPort {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BindPort::Ephemeral => formatter.write_str("ephemeral"),
            BindPort::Fixed(port) => write!(formatter, "{port}"),
        }
    }
}

/// Which positive bound was violated.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LimitField {
    /// [`MaxResolvedAddresses`].
    MaxResolvedAddresses,
    /// [`MaxBindAttempts`].
    MaxBindAttempts,
}

impl fmt::Display for LimitField {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LimitField::MaxResolvedAddresses => formatter.write_str("max_resolved_addresses"),
            LimitField::MaxBindAttempts => formatter.write_str("max_bind_attempts"),
        }
    }
}

/// A bound was not positive.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum LimitError {
    /// Zero is not a bound; it is the absence of permission to do anything.
    #[error("{field} must be positive, and zero was supplied")]
    NotPositive {
        /// The field that was zero.
        field: LimitField,
    },
}

/// The most addresses a resolution may yield.
///
/// Distinct from [`MaxBindAttempts`] as a type, so the two cannot be swapped at
/// a call site where both are plain integers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxResolvedAddresses(NonZeroUsize);

impl MaxResolvedAddresses {
    /// Checks positivity.
    pub fn new(value: usize) -> Result<Self, LimitError> {
        match NonZeroUsize::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(LimitError::NotPositive {
                field: LimitField::MaxResolvedAddresses,
            }),
        }
    }

    /// The bound.
    pub fn get(self) -> usize {
        self.0.get()
    }
}

/// The most bind attempts a single plan may make.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MaxBindAttempts(NonZeroUsize);

impl MaxBindAttempts {
    /// Checks positivity.
    pub fn new(value: usize) -> Result<Self, LimitError> {
        match NonZeroUsize::new(value) {
            Some(value) => Ok(Self(value)),
            None => Err(LimitError::NotPositive {
                field: LimitField::MaxBindAttempts,
            }),
        }
    }

    /// The bound.
    pub fn get(self) -> usize {
        self.0.get()
    }
}

/// The positive bounds a bind plan operates under.
///
/// Deliberately implements neither `Default` nor `new()`-with-defaults: under
/// the bounded-everything rule, capacity is stated by the caller and never
/// materialises on its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BindLimits {
    max_resolved_addresses: MaxResolvedAddresses,
    max_bind_attempts: MaxBindAttempts,
}

impl BindLimits {
    /// Both bounds, stated explicitly.
    pub fn new(
        max_resolved_addresses: MaxResolvedAddresses,
        max_bind_attempts: MaxBindAttempts,
    ) -> Self {
        Self {
            max_resolved_addresses,
            max_bind_attempts,
        }
    }

    /// The resolution bound.
    pub fn max_resolved_addresses(self) -> MaxResolvedAddresses {
        self.max_resolved_addresses
    }

    /// The attempt bound.
    pub fn max_bind_attempts(self) -> MaxBindAttempts {
        self.max_bind_attempts
    }
}

/// A complete, validated bind intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListenerBindPlan {
    host: BindHost,
    port: BindPort,
    limits: BindLimits,
}

impl ListenerBindPlan {
    /// Assembles the intent.
    pub fn new(host: BindHost, port: BindPort, limits: BindLimits) -> Self {
        Self { host, port, limits }
    }

    /// The requested host.
    pub fn host(&self) -> &BindHost {
        &self.host
    }

    /// The requested port intent.
    pub fn port(&self) -> BindPort {
        self.port
    }

    /// The positive bounds.
    pub fn limits(&self) -> BindLimits {
        self.limits
    }
}

/// A bounded, non-empty candidate sequence.
///
/// [`ResolvedAddresses::new`] is the ONLY constructor, and it enforces the
/// bound. That is deliberate: a hostile or buggy resolver cannot hand back an
/// over-long candidate list, because it has no way to build one. The bound is
/// carried by construction rather than re-checked by every caller, so there is
/// exactly one site to mutate and exactly one witness to fire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedAddresses(Vec<IpAddr>);

impl ResolvedAddresses {
    /// Refuses an empty list, and refuses more than `limit` addresses.
    ///
    /// Over-length is refused rather than truncated. Truncating would silently
    /// convert "this resolver is not behaving" into "we tried the first few",
    /// and the caller would never learn the difference.
    pub fn new(addresses: Vec<IpAddr>, limit: MaxResolvedAddresses) -> Result<Self, ResolveError> {
        if addresses.is_empty() {
            return Err(ResolveError::NoAddresses);
        }
        if addresses.len() > limit.get() {
            return Err(ResolveError::TooManyAddresses {
                produced: addresses.len(),
                limit: limit.get(),
            });
        }
        Ok(Self(addresses))
    }

    /// The candidates, in resolution order.
    pub fn as_slice(&self) -> &[IpAddr] {
        &self.0
    }

    /// How many candidates were admitted.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Always false: the constructor refuses an empty list.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// Why this build cannot resolve a name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AdapterUnavailableReason {
    /// No production DNS adapter exists yet; only literal addresses are served.
    NoProductionDnsAdapter,
}

impl fmt::Display for AdapterUnavailableReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdapterUnavailableReason::NoProductionDnsAdapter => formatter
                .write_str("no production DNS adapter is installed; literal IP addresses only"),
        }
    }
}

/// A typed resolution failure.
///
/// Every variant is classified structurally. None is derived from operating
/// system error text.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ResolveError {
    /// The configured resolver cannot answer names at all.
    ///
    /// This is a stated capability gap, never a silent fallback to a default
    /// address and never an ambient platform lookup.
    #[error("name resolution is unavailable: {reason}")]
    AdapterUnavailable {
        /// The stated gap.
        reason: AdapterUnavailableReason,
    },
    /// The resolver produced more candidates than the plan's bound permits.
    #[error("resolution produced {produced} addresses, above the bound of {limit}")]
    TooManyAddresses {
        /// How many the resolver produced.
        produced: usize,
        /// The bound it exceeded.
        limit: usize,
    },
    /// The resolver produced nothing. Absent is not an empty success.
    #[error("resolution produced no addresses")]
    NoAddresses,
    /// The resolver's own I/O failed, classified by kind.
    #[error("the resolver failed with I/O kind {kind:?}")]
    Adapter {
        /// The classified kind. Never matched OS text.
        kind: io::ErrorKind,
    },
}

/// The crate's boxed-future seam.
///
/// Mirrors the shape already landed at `grapt-runtime::supervisor::BoxFuture`
/// rather than adding an `async-trait` dependency for one trait. It is
/// re-declared instead of imported because this foundation must not depend on
/// `grapt-runtime`: the accepted direction is `grapt-types <- grapt-location
/// <- grapt-api`, and location-to-runtime is a forbidden back-edge here.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Injected, bounded name resolution.
///
/// The `limit` is passed in rather than read from ambient configuration so the
/// adapter is bounded at its own boundary, and [`ResolvedAddresses`] enforces
/// the same bound structurally on the way back.
pub trait NameResolver: fmt::Debug + Send + Sync {
    /// Resolves `name` into at most `limit` candidate addresses.
    fn resolve<'a>(
        &'a self,
        name: &'a BindHostName,
        limit: MaxResolvedAddresses,
    ) -> BoxFuture<'a, Result<ResolvedAddresses, ResolveError>>;
}

/// The production resolver: literal addresses only.
///
/// It answers every hostname with [`ResolveError::AdapterUnavailable`]. That is
/// the honest state of this foundation — there is no DNS adapter yet — and it
/// is far better than a constructor that reaches for the platform resolver and
/// makes every downstream test depend on the network.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LiteralAddressResolver;

impl NameResolver for LiteralAddressResolver {
    fn resolve<'a>(
        &'a self,
        _name: &'a BindHostName,
        _limit: MaxResolvedAddresses,
    ) -> BoxFuture<'a, Result<ResolvedAddresses, ResolveError>> {
        Box::pin(async {
            Err(ResolveError::AdapterUnavailable {
                reason: AdapterUnavailableReason::NoProductionDnsAdapter,
            })
        })
    }
}

/// A deterministic resolver for tests and for consumers building fixtures.
///
/// It has no clock, no network and no ordering surprises: a name either has a
/// declared answer or it has none. It also counts the calls it received, which
/// is how a caller proves that a literal-address plan reached no resolver at
/// all rather than merely reaching one that happened to agree.
///
/// This is a test authority. It is public because the integration tests and
/// later lanes compose it from outside the crate; it is never a production DNS
/// path.
#[derive(Debug, Default)]
pub struct DeterministicNameResolver {
    answers: BTreeMap<BindHostName, Vec<IpAddr>>,
    calls: AtomicUsize,
}

impl DeterministicNameResolver {
    /// An empty resolver that answers nothing.
    pub fn new() -> Self {
        Self::default()
    }

    /// Declares the exact answer for one name.
    pub fn with_answer(mut self, name: BindHostName, addresses: Vec<IpAddr>) -> Self {
        self.answers.insert(name, addresses);
        self
    }

    /// How many times [`NameResolver::resolve`] has been called.
    pub fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

impl NameResolver for DeterministicNameResolver {
    fn resolve<'a>(
        &'a self,
        name: &'a BindHostName,
        limit: MaxResolvedAddresses,
    ) -> BoxFuture<'a, Result<ResolvedAddresses, ResolveError>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let answer = self.answers.get(name).cloned();
        Box::pin(async move {
            match answer {
                Some(addresses) => ResolvedAddresses::new(addresses, limit),
                None => Err(ResolveError::NoAddresses),
            }
        })
    }
}
