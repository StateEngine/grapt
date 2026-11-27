//! Deterministic route composition with typed refusal before every call that
//! can panic.
//!
//! ## Why A owns the `axum::Router` rather than a neutral table
//!
//! Ruling R4HA-05. A transport-neutral route table that a host later converts
//! would be smaller and would take no `axum` dependency, but it gives the
//! composition law **two carriers** — A's table and axum's real routing — which
//! can disagree. The host would then inherit an unwitnessed panic path, because
//! `axum::Router::route` does not return an error: it expands `panic_on_err!`
//! over its internal result. This workspace forbids `unsafe_code`, denies
//! `unwrap_used` and `expect_used` including in tests, and holds that no
//! untrusted value may reach a panicking method.
//!
//! Owning the router means A validates and refuses **before** the call, so the
//! refusal is the only path a caller can take and the panic is unreachable. One
//! law, one carrier.
//!
//! ## The exact panic set this refuses ahead of
//!
//! Measured by reading the **resolved** `axum 0.8.9` (`src/routing/mod.rs:178`,
//! which wraps `PathRouter::route` in `panic_on_err!`), whose error sources are:
//!
//! 1. `validate_path` — an empty path, a path not starting with `/`, or any
//!    segment starting with `:` or `*` (the v0.7 compatibility checks);
//! 2. `MethodRouter::merge_for_path` — the same method registered twice on one
//!    path;
//! 3. `PathRouter::set_node` — the `InsertError` of `matchit`, which axum pins
//!    at exactly `0.8.4`: `Conflict`, `InvalidParamSegment`, `InvalidParam` or
//!    `InvalidCatchAll`.
//!
//! The preflight for this lane cited 0.8.8 because that was the newest version
//! cached locally; 0.8.9 is what the resolver actually chose. `path_router.rs`
//! is byte-identical between the two and both pin `matchit =0.8.4`, so the
//! measured set carries — but the version named here is the resolved one, not
//! the assumed one.
//!
//! [`RoutePath`] admits a **subset** of matchit's grammar chosen so that
//! `InvalidParamSegment`, `InvalidParam` and `InvalidCatchAll` are all
//! unreachable: every parameter occupies a whole segment, braces are balanced
//! and named, and a catch-all may appear only as the final segment. That leaves
//! `Conflict`, which under this grammar is decidable exactly — two paths
//! conflict when their segment signatures match but their texts differ, i.e.
//! when they differ only in parameter names. Narrowing the grammar rather than
//! reimplementing matchit's matcher is deliberate: a reimplementation would be
//! the second carrier this module exists to avoid.
//!
//! The version is named because the panic set is a property of it. A later
//! version must be re-measured, not assumed.
//!
//! ## Occupancy and syntax are two laws, so they are two answers
//!
//! A segment is classified by one total scan (`classify_segment`, private —
//! the module's public surface is the refusal, not the scanner) rather than by
//! its first brace pair. `{x}b{y}c` carries two balanced, validly
//! named captures, so the rule it breaks is that a parameter must occupy its
//! whole segment; `a{id}b{c` carries a brace that never closes, so the rule it
//! breaks is the brace grammar. Classifying from `split_once('{')` plus
//! `split_once('}')` cannot tell a second valid placeholder from a stray brace
//! and reported both as malformed syntax. A refusal names the law it actually
//! applied, so the scan reads the whole segment before it answers.

use std::collections::BTreeSet;

use axum::handler::Handler;
use axum::routing::{delete, get, patch, post, put, MethodRouter};
use axum::Router;

use crate::error::{ApiRefusal, Redacted};

/// The largest route path this boundary admits, in UTF-8 bytes.
pub const MAX_ROUTE_PATH_BYTES: usize = 512;

/// The largest number of `/`-separated segments a path may carry.
pub const MAX_ROUTE_SEGMENTS: usize = 32;

/// The methods A composes over. Closed: a new one is a reviewed edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RouteMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

impl RouteMethod {
    fn label(self) -> &'static str {
        match self {
            RouteMethod::Get => "GET",
            RouteMethod::Post => "POST",
            RouteMethod::Put => "PUT",
            RouteMethod::Patch => "PATCH",
            RouteMethod::Delete => "DELETE",
        }
    }

    fn method_router<H, T>(self, handler: H) -> MethodRouter<()>
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        match self {
            RouteMethod::Get => get(handler),
            RouteMethod::Post => post(handler),
            RouteMethod::Put => put(handler),
            RouteMethod::Patch => patch(handler),
            RouteMethod::Delete => delete(handler),
        }
    }
}

/// A validated route path.
///
/// See the module note for why the admitted grammar is narrower than matchit's.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RoutePath {
    raw: String,
    signature: String,
}

fn is_parameter_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// What one `/`-separated segment is, under the admitted grammar.
///
/// Closed, and derived from a single total scan of the segment rather than
/// from its first brace pair. That distinction is the whole point: reading
/// only the first `{`…`}` cannot tell a second valid placeholder from a stray
/// brace, so `{x}b{y}c` — every capture balanced, named and valid — was
/// reported as malformed syntax when its actual defect is occupancy.
#[derive(Debug)]
enum SegmentShape<'a> {
    /// No brace anywhere: ordinary static text.
    Static,
    /// Exactly one balanced, validly named placeholder and nothing else.
    WholeParameter { name: &'a str, catch_all: bool },
    /// Every brace forms a balanced, validly named placeholder, but the
    /// segment is not exactly one of them: static text shares the segment, or
    /// more than one placeholder does. An OCCUPANCY defect.
    PartialParameters,
    /// Some brace closes nothing, some brace never closes, or a placeholder
    /// name is empty or is not an ASCII identifier. A SYNTAX defect.
    Malformed,
}

/// Scan a whole segment once and name its shape.
///
/// Total by construction: every character falls into exactly one arm, and a
/// placeholder still open when the segment ends is refused explicitly rather
/// than left unexamined.
fn classify_segment(segment: &str) -> SegmentShape<'_> {
    // Byte index just past the `{` of the placeholder currently open.
    let mut open: Option<usize> = None;
    let mut placeholders = 0usize;
    let mut has_static = false;
    let mut last: Option<(&str, bool)> = None;

    for (index, character) in segment.char_indices() {
        match (character, open) {
            ('{', None) => open = Some(index + 1),
            // A `}` with nothing open closes nothing. One clause, because it
            // is one law: a stray close before any placeholder (`a}b{id}c`)
            // and one after a complete placeholder (`a{id}b}c`) break the
            // same rule, and splitting them would add a branch that decides
            // nothing.
            ('}', None) => return SegmentShape::Malformed,
            ('}', Some(start)) => {
                let inner = &segment[start..index];
                let (name, catch_all) = match inner.strip_prefix('*') {
                    Some(rest) => (rest, true),
                    None => (inner, false),
                };
                // A nested `{` lands inside `inner` and is not an identifier
                // character, so nesting is refused here rather than by a
                // second clause that would only repeat this one.
                if !is_parameter_name(name) {
                    return SegmentShape::Malformed;
                }
                placeholders += 1;
                last = Some((name, catch_all));
                open = None;
            }
            (_, None) => has_static = true,
            // Inside a placeholder: these bytes are the candidate name and
            // are judged as a whole when it closes.
            (_, Some(_)) => {}
        }
    }
    if open.is_some() {
        return SegmentShape::Malformed;
    }
    match (placeholders, last) {
        (0, _) => SegmentShape::Static,
        (1, Some((name, catch_all))) if !has_static => {
            SegmentShape::WholeParameter { name, catch_all }
        }
        _ => SegmentShape::PartialParameters,
    }
}

impl RoutePath {
    /// Validate a path against the admitted grammar.
    ///
    /// Every refusal here is a case that would otherwise reach a panicking
    /// call, so this function is the boundary the module note describes.
    /// Validate a path, naming the EXACT rule that rejected it.
    ///
    /// R1 finding H3: every refusal below used to be one `ConflictingRoute`,
    /// so a malformed path told the caller its patterns conflicted. `defect`
    /// now carries the rule actually applied, and `ConflictingRoute` is
    /// reserved for its real meaning — two well-formed paths that cannot
    /// coexist.
    pub fn try_new(raw: impl Into<String>) -> Result<Self, ApiRefusal> {
        let raw = raw.into();
        let malformed = |defect: crate::error::RoutePathDefect| ApiRefusal::RouteMalformed {
            defect,
            observed: Redacted::new(raw.clone()),
        };
        use crate::error::RoutePathDefect as Defect;
        if raw.is_empty() {
            return Err(malformed(Defect::Empty));
        }
        if !raw.starts_with('/') {
            return Err(malformed(Defect::NoLeadingSlash));
        }
        if raw.len() > MAX_ROUTE_PATH_BYTES {
            return Err(malformed(Defect::TooLong {
                max: MAX_ROUTE_PATH_BYTES,
            }));
        }
        // "/" is the root route and has no segments; every other path is split
        // on '/' with the leading empty piece discarded.
        let segments: Vec<&str> = if raw == "/" {
            Vec::new()
        } else {
            raw[1..].split('/').collect()
        };
        if segments.len() > MAX_ROUTE_SEGMENTS {
            return Err(malformed(Defect::TooManySegments {
                max: MAX_ROUTE_SEGMENTS,
            }));
        }
        let mut signature = String::from("/");
        let mut names: BTreeSet<&str> = BTreeSet::new();
        for (index, segment) in segments.iter().enumerate() {
            if segment.is_empty() {
                return Err(malformed(Defect::EmptySegment));
            }
            // axum's own v0.7 compatibility check: these panic rather than
            // route, so they are refused here first.
            if segment.starts_with(':') {
                return Err(malformed(Defect::LegacyColonCapture));
            }
            if segment.starts_with('*') {
                return Err(malformed(Defect::LegacyStarCapture));
            }
            // One entry, not two. The R3 correction reached its classifier
            // only through `starts_with('{') || ends_with('}')`, so a segment
            // that satisfied neither — the whole balanced-internal class —
            // could never be classified at all. A gate in front of a
            // classifier is a carrier of its own, so there is no gate.
            match classify_segment(segment) {
                SegmentShape::Malformed => return Err(malformed(Defect::MalformedParameter)),
                // A parameter must occupy the WHOLE segment. `{a}-b` and
                // `{x}b{y}c` are both matchit's `InvalidParamSegment`;
                // refusing them here keeps that variant unreachable, and the
                // refusal names occupancy rather than syntax because every
                // capture in them is balanced, named and valid.
                SegmentShape::PartialParameters => {
                    return Err(malformed(Defect::PartialSegmentParameter))
                }
                SegmentShape::Static => signature.push_str(segment),
                SegmentShape::WholeParameter { name, catch_all } => {
                    // A catch-all is only legal last; anywhere else is
                    // matchit's `InvalidCatchAll`.
                    if catch_all && index + 1 != segments.len() {
                        return Err(malformed(Defect::CatchAllNotFinal));
                    }
                    // Two parameters with one name in a single path is a
                    // caller mistake that matchit would accept and then match
                    // confusingly.
                    if !names.insert(name) {
                        return Err(malformed(Defect::DuplicateParameterName));
                    }
                    signature.push_str(if catch_all { "{*}" } else { "{}" });
                }
            }
            if index + 1 != segments.len() {
                signature.push('/');
            }
        }
        Ok(Self { raw, signature })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }

    /// The segment-kind signature: parameter names erased, static text kept.
    ///
    /// Two admitted paths conflict in matchit exactly when this is equal and
    /// the raw text is not.
    fn signature(&self) -> &str {
        &self.signature
    }
}

/// One registered (method, path) pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct RouteKey {
    method: RouteMethod,
    path: RoutePath,
}

impl RouteKey {
    pub fn method(&self) -> RouteMethod {
        self.method
    }

    pub fn path(&self) -> &RoutePath {
        &self.path
    }

    /// The stable rendering used for ordering and comparison.
    pub fn label(&self) -> String {
        format!("{} {}", self.method.label(), self.path.as_str())
    }
}

/// A checked positive capacity, with no `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxRouterModules(usize);

impl MaxRouterModules {
    pub fn try_new(value: usize) -> Result<Self, ApiRefusal> {
        if value == 0 {
            return Err(ApiRefusal::BoundNotPositive {
                bound: "max_router_modules",
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> usize {
        self.0
    }
}

/// Accumulates registrations, refusing every one that would panic.
///
/// `Debug` is hand-written because `axum::Router` is not `Debug`; the
/// workspace warns on a missing `Debug` implementation and a wrapper that
/// silently lost it would be the kind of quiet omission that lint exists for.
pub struct ApiRouterBuilder {
    router: Router<()>,
    keys: BTreeSet<RouteKey>,
    signatures: BTreeSet<(RouteMethod, String, String)>,
    max: MaxRouterModules,
}

impl core::fmt::Debug for ApiRouterBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ApiRouterBuilder")
            .field("registered", &self.keys.len())
            .field("max", &self.max.value())
            .finish()
    }
}

impl ApiRouterBuilder {
    pub fn new(max: MaxRouterModules) -> Self {
        Self {
            router: Router::new(),
            keys: BTreeSet::new(),
            signatures: BTreeSet::new(),
            max,
        }
    }

    /// How many routes are registered so far.
    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// Register one route, refusing before any call that can panic.
    ///
    /// The order of the checks matters: a duplicate is reported as a duplicate
    /// even when the router is also full, and capacity is checked before
    /// conflict so that a full router reports its bound rather than an
    /// incidental collision.
    ///
    /// Note there is no "registered after sealing" refusal, because
    /// [`Self::seal`] takes `self` by value. Registering after sealing is a
    /// move-checker error rather than a runtime refusal — a stronger result,
    /// and the reason `ApiRefusal` carries no `RouterSealed` variant: a
    /// refusal that can never be constructed is a vocabulary entry that lies
    /// about what can happen.
    pub fn register<H, T>(
        &mut self,
        method: RouteMethod,
        path: RoutePath,
        handler: H,
    ) -> Result<(), ApiRefusal>
    where
        H: Handler<T, ()>,
        T: 'static,
    {
        let key = RouteKey {
            method,
            path: path.clone(),
        };
        if self.keys.contains(&key) {
            return Err(ApiRefusal::DuplicateRoute {
                observed: Redacted::new(key.label()),
            });
        }
        if self.keys.len() >= self.max.value() {
            return Err(ApiRefusal::RouterBoundExceeded {
                max: self.max.value(),
            });
        }
        // Same method, same segment signature, different text: this is
        // matchit's `Conflict`, and reaching `Router::route` with it panics.
        let signature = (
            method,
            path.signature().to_owned(),
            path.as_str().to_owned(),
        );
        if self
            .signatures
            .iter()
            .any(|(m, sig, raw)| *m == method && sig == &signature.1 && raw != &signature.2)
        {
            return Err(ApiRefusal::ConflictingRoute {
                observed: Redacted::new(key.label()),
            });
        }
        // Only now, with every panic condition refused, is the panicking call
        // reached. Taking the router by value and putting it back keeps this
        // the single place `Router::route` is invoked in the crate.
        let router = core::mem::take(&mut self.router);
        self.router = router.route(path.as_str(), method.method_router(handler));
        self.signatures.insert(signature);
        self.keys.insert(key);
        Ok(())
    }

    /// Seal the builder into an immutable router, consuming it.
    pub fn seal(self) -> ApiRouter {
        ApiRouter {
            router: self.router,
            keys: self.keys,
        }
    }
}

/// The sealed, immutable route set.
pub struct ApiRouter {
    router: Router<()>,
    keys: BTreeSet<RouteKey>,
}

impl core::fmt::Debug for ApiRouter {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ApiRouter")
            .field("routes", &self.keys.len())
            .finish()
    }
}

impl ApiRouter {
    /// The composed route set, in a **canonical order**.
    ///
    /// Sorted rather than insertion-ordered, and that is the determinism law
    /// itself: two identical registration sets composed in different orders
    /// produce the same route set. Returning insertion order would make the
    /// composition depend on how the caller happened to sequence it, and the
    /// equality assertion below would silently become an assertion about the
    /// test's own ordering.
    pub fn route_set(&self) -> Vec<String> {
        let mut labels: Vec<String> = self.keys.iter().map(RouteKey::label).collect();
        // Sorted by the rendered LABEL rather than left in the key set's own
        // order. The keys order by `(RouteMethod, RoutePath)`, and
        // `RouteMethod`'s ordering is its derived declaration order — so
        // reordering the enum variants would silently reorder every composed
        // route set. Anchoring the canonical order to the rendering keeps it a
        // property of what is composed rather than of how the vocabulary
        // happens to be written down.
        labels.sort();
        labels
    }

    pub fn len(&self) -> usize {
        self.keys.len()
    }

    pub fn is_empty(&self) -> bool {
        self.keys.is_empty()
    }

    /// The composed `axum::Router`, for a host lane to serve.
    ///
    /// A never serves it. Handing it out is the whole point of the seal: the
    /// host consumes a route set it cannot add to.
    pub fn into_router(self) -> Router<()> {
        self.router
    }
}
