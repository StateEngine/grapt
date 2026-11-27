//! Route composition: deterministic, bounded, and refusing ahead of every call
//! that panics.
//!
//! These witnesses need only the public surface, so they live here rather than
//! in the library's `#[cfg(test)]` module: nothing in this file touches a sealed
//! mint.
//!
//! The last test in this file is the one that makes the rest mean something. A
//! builder that refuses a registration proves nothing on its own — the refusal
//! could be arbitrary caution over an input `axum` would have accepted happily.
//! `axum_really_panics_on_what_the_builder_refuses` drives the raw
//! `axum::Router` with the same input and catches the unwind, so the refusal is
//! shown to stand in front of a real panic rather than an imagined one.

use std::panic::{catch_unwind, AssertUnwindSafe};

use grapt_api::{
    ApiRefusal, ApiRouterBuilder, MaxRouterModules, RouteMethod, RoutePath, RoutePathDefect,
    MAX_ROUTE_PATH_BYTES, MAX_ROUTE_SEGMENTS,
};

async fn ok() {}

fn bound(value: usize) -> MaxRouterModules {
    match MaxRouterModules::try_new(value) {
        Ok(bound) => bound,
        Err(refusal) => panic!("fixture bound {value} was refused: {refusal}"),
    }
}

fn path(raw: &str) -> RoutePath {
    match RoutePath::try_new(raw) {
        Ok(path) => path,
        Err(refusal) => panic!("fixture path {raw:?} was refused: {refusal}"),
    }
}

fn assert_route_defect(raw: &str, expected: RoutePathDefect, rendering: &str) {
    let refusal = match RoutePath::try_new(raw) {
        Ok(_) => panic!("the malformed route {raw:?} must be refused"),
        Err(refusal) => refusal,
    };
    match &refusal {
        ApiRefusal::RouteMalformed { defect, .. } => assert_eq!(
            *defect, expected,
            "the malformed route {raw:?} must name its exact rule"
        ),
        unrelated => {
            panic!("the malformed route {raw:?} used an unrelated refusal: {unrelated:?}")
        }
    }
    assert_eq!(
        refusal.to_string(),
        rendering,
        "the malformed route {raw:?} must render the rule it actually broke"
    );
}

#[test]
fn registration_past_the_module_capacity_is_refused() {
    let mut builder = ApiRouterBuilder::new(bound(2));
    for raw in ["/one", "/two"] {
        if let Err(refusal) = builder.register(RouteMethod::Get, path(raw), ok) {
            panic!("registering {raw} within capacity was refused: {refusal}");
        }
    }
    match builder.register(RouteMethod::Get, path("/three"), ok) {
        Ok(()) => panic!("registration past the bound must be refused"),
        Err(refusal) => assert!(
            matches!(refusal, ApiRefusal::RouterBoundExceeded { max: 2 }),
            "the refusal names the BOUND, not the observed count; got {refusal:?}"
        ),
    }
    assert_eq!(builder.len(), 2, "a refused registration adds nothing");
}

#[test]
fn a_duplicate_route_registration_is_refused() {
    let mut builder = ApiRouterBuilder::new(bound(8));
    if let Err(refusal) = builder.register(RouteMethod::Get, path("/seats"), ok) {
        panic!("the first registration was refused: {refusal}");
    }
    match builder.register(RouteMethod::Get, path("/seats"), ok) {
        Ok(()) => panic!("a duplicate (method, path) must be refused"),
        Err(refusal) => assert!(matches!(refusal, ApiRefusal::DuplicateRoute { .. })),
    }

    // The same-file control: a DIFFERENT method on the same path is not a
    // duplicate, and axum merges it. Without this, a builder that refused every
    // second registration would pass the case above.
    if let Err(refusal) = builder.register(RouteMethod::Post, path("/seats"), ok) {
        panic!("a distinct method on the same path must be admitted: {refusal}");
    }
    assert_eq!(builder.len(), 2);
}

#[test]
fn a_conflicting_route_registration_is_refused() {
    let mut builder = ApiRouterBuilder::new(bound(8));
    if let Err(refusal) = builder.register(RouteMethod::Get, path("/seats/{id}"), ok) {
        panic!("the first registration was refused: {refusal}");
    }
    // Same shape, different parameter NAME. This is `matchit::InsertError::Conflict`
    // and reaching `Router::route` with it panics.
    match builder.register(RouteMethod::Get, path("/seats/{name}"), ok) {
        Ok(()) => panic!("a conflicting parameter name must be refused"),
        Err(refusal) => assert!(matches!(refusal, ApiRefusal::ConflictingRoute { .. })),
    }

    // The same-file control: a static sibling of a parameter is ordinary
    // routing, not a conflict, and must be admitted.
    if let Err(refusal) = builder.register(RouteMethod::Get, path("/seats/current"), ok) {
        panic!("a static sibling of a parameter must be admitted: {refusal}");
    }
}

#[test]
fn a_path_axum_would_panic_on_is_refused_before_registration() {
    // Every one of these reaches a panic inside `axum::Router::route`: the first
    // four via `validate_path`, the rest via `matchit`'s InsertError variants.
    let refused = [
        ("empty", ""),
        ("no leading slash", "seats"),
        ("v0.7 colon capture", "/seats/:id"),
        ("v0.7 star wildcard", "/seats/*rest"),
        ("empty segment", "/seats//current"),
        ("trailing empty segment", "/seats/"),
        ("partial-segment parameter", "/seats/{id}-suffix"),
        ("unbalanced brace", "/seats/{id"),
        ("empty parameter name", "/seats/{}"),
        ("catch-all before the end", "/seats/{*rest}/more"),
        ("duplicate parameter name", "/seats/{id}/rooms/{id}"),
    ];
    for (route, raw) in refused {
        assert!(
            RoutePath::try_new(raw).is_err(),
            "the {route} path {raw:?} must be refused before it can reach a \
             panicking registration"
        );
    }

    // The same-file control. A grammar that refused everything would satisfy
    // every case above and compose no router at all.
    let admitted = [
        "/",
        "/seats",
        "/seats/current",
        "/seats/{id}",
        "/seats/{id}/rooms/{room}",
        "/files/{*rest}",
    ];
    for raw in admitted {
        assert!(
            RoutePath::try_new(raw).is_ok(),
            "the ordinary path {raw:?} must be admitted"
        );
    }
}

#[test]
fn every_malformed_route_names_the_exact_rule_it_broke() {
    let too_long = format!("/{}", "a".repeat(MAX_ROUTE_PATH_BYTES));
    let too_many_segments = format!("/{}", vec!["a"; MAX_ROUTE_SEGMENTS + 1].join("/"));
    let cases = [
        ("", RoutePathDefect::Empty),
        ("seats", RoutePathDefect::NoLeadingSlash),
        (
            too_long.as_str(),
            RoutePathDefect::TooLong {
                max: MAX_ROUTE_PATH_BYTES,
            },
        ),
        (
            too_many_segments.as_str(),
            RoutePathDefect::TooManySegments {
                max: MAX_ROUTE_SEGMENTS,
            },
        ),
        ("/seats//current", RoutePathDefect::EmptySegment),
        (
            "/seats/{id}-suffix",
            RoutePathDefect::PartialSegmentParameter,
        ),
        ("/seats/{id", RoutePathDefect::MalformedParameter),
        ("/seats/{*rest}/more", RoutePathDefect::CatchAllNotFinal),
        (
            "/seats/{id}/rooms/{id}",
            RoutePathDefect::DuplicateParameterName,
        ),
        ("/seats/:id", RoutePathDefect::LegacyColonCapture),
        ("/seats/*rest", RoutePathDefect::LegacyStarCapture),
    ];

    for (raw, expected) in cases {
        match RoutePath::try_new(raw) {
            Ok(_) => panic!("the malformed route {raw:?} must be refused"),
            Err(ApiRefusal::RouteMalformed { defect, .. }) => assert_eq!(
                defect, expected,
                "the malformed route {raw:?} must name its exact rule"
            ),
            Err(refusal) => {
                panic!("the malformed route {raw:?} used an unrelated refusal: {refusal:?}")
            }
        }
    }
}

#[test]
fn parameter_occupancy_and_syntax_name_their_own_laws() {
    match RoutePath::try_new("/seats/{id}") {
        Ok(route) => assert_eq!(route.as_str(), "/seats/{id}"),
        Err(refusal) => panic!("a pure whole-segment parameter was refused: {refusal}"),
    }

    for raw in [
        "/seats/x-{id}",
        "/seats/{id}-suffix",
        "/seats/a{id}c",
        "/seats/-{id}-",
        "/files/a{*rest}c",
    ] {
        assert_route_defect(
            raw,
            RoutePathDefect::PartialSegmentParameter,
            "a route parameter must occupy its whole segment",
        );
    }

    for raw in [
        "/seats/a{id",
        "/seats/aid}c",
        "/seats/a}id{c",
        "/seats/a{{id}}c",
        "/seats/a{}c",
        "/seats/a{id!}c",
        "/seats/{bad-name}",
    ] {
        assert_route_defect(
            raw,
            RoutePathDefect::MalformedParameter,
            "a route parameter must be a balanced `{name}` naming ASCII alphanumerics or `_`",
        );
    }
}

#[test]
fn several_balanced_placeholders_in_one_segment_are_an_occupancy_defect() {
    // Every capture in these is balanced, named and a valid ASCII identifier,
    // so the rule each breaks is occupancy — a parameter must occupy its whole
    // segment — and not the brace grammar. The first three are the exact
    // inputs that classifying from the first brace pair regressed into
    // malformed syntax; the rest are the same class and were already wrong.
    for raw in [
        "/seats/{x}b{y}c",
        "/seats/{x}b{y}c/rooms",
        "/seats/a{x}b{y}",
        "/seats/a{x}b{y}c",
        "/seats/a{x}b{y}c{z}d",
        "/seats/a{x}{y}c",
        "/seats/{x}b{y}",
        "/seats/{x}{y}",
        "/seats/a{x}b{*rest}c",
        "/seats/{x}b{*rest}",
        "/files/{*a}{*b}",
    ] {
        assert_route_defect(
            raw,
            RoutePathDefect::PartialSegmentParameter,
            "a route parameter must occupy its whole segment",
        );
    }

    // The same-file controls. Without the first, a grammar that answered
    // "occupancy" for anything carrying two braces would satisfy every case
    // above; without the second, one that answered it for everything would.
    assert_route_defect(
        "/seats/{x}b{y",
        RoutePathDefect::MalformedParameter,
        "a route parameter must be a balanced `{name}` naming ASCII alphanumerics or `_`",
    );
    assert!(
        RoutePath::try_new("/seats/{id}/rooms/{room}").is_ok(),
        "one whole-segment parameter per segment must remain admitted"
    );
}

#[test]
fn a_brace_that_never_closes_is_malformed_syntax() {
    // Reading only the first brace pair cannot see the first four: the brace
    // that never closes sits in that pair's SUFFIX, where a scan that stopped
    // at the first `}` had already finished looking.
    for raw in [
        "/seats/a{id}b{c",
        "/seats/{id}b{c",
        "/seats/{a}b{c",
        "/seats/{id}{",
        "/seats/a{id",
        "/seats/{id",
    ] {
        assert_route_defect(
            raw,
            RoutePathDefect::MalformedParameter,
            "a route parameter must be a balanced `{name}` naming ASCII alphanumerics or `_`",
        );
    }

    // The same-file control: close that brace and the same shape becomes an
    // occupancy defect, so the answer above is about the brace rather than
    // about the static text around it.
    assert_route_defect(
        "/seats/a{id}b{c}",
        RoutePathDefect::PartialSegmentParameter,
        "a route parameter must occupy its whole segment",
    );
}

#[test]
fn a_brace_that_closes_nothing_is_malformed_syntax() {
    // A `}` with no open placeholder, both before any placeholder and after a
    // complete one. Those two families break one rule and the scan answers
    // them in one clause, so one witness covers both rather than splitting a
    // single law across two tests that a single mutation would both turn red.
    for raw in [
        "/seats/a}b{id}c",
        "/seats/}id{",
        "/seats/aid}c",
        "/seats/}{",
        "/seats/a{id}b}c",
        "/seats/a{id}c}",
    ] {
        assert_route_defect(
            raw,
            RoutePathDefect::MalformedParameter,
            "a route parameter must be a balanced `{name}` naming ASCII alphanumerics or `_`",
        );
    }

    // The same-file control: the same static text with every brace paired is
    // an occupancy defect, not a syntax one.
    assert_route_defect(
        "/seats/a{id}bc",
        RoutePathDefect::PartialSegmentParameter,
        "a route parameter must occupy its whole segment",
    );
}

#[test]
fn a_placeholder_name_must_be_an_ascii_identifier() {
    // Empty, non-identifier and nested-brace names. Nesting has no clause of
    // its own: an inner `{` lands inside the candidate name, which is then not
    // an ASCII identifier, so this predicate is the carrier for it and a
    // separate nesting branch would only repeat the answer.
    for raw in [
        "/seats/a{}c",
        "/seats/{}",
        "/seats/{*}",
        "/seats/a{id!}c",
        "/seats/{bad-name}",
        "/seats/a{{id}}c",
        "/seats/a{b{c}d",
        "/seats/{a{b}}",
    ] {
        assert_route_defect(
            raw,
            RoutePathDefect::MalformedParameter,
            "a route parameter must be a balanced `{name}` naming ASCII alphanumerics or `_`",
        );
    }

    // The same-file controls: underscores and digits ARE identifier
    // characters, so the same shapes are occupancy and admission rather than
    // syntax.
    assert_route_defect(
        "/seats/a{i_d1}c",
        RoutePathDefect::PartialSegmentParameter,
        "a route parameter must occupy its whole segment",
    );
    assert!(
        RoutePath::try_new("/seats/{i_d1}").is_ok(),
        "a name of letters, digits and underscores must be admitted"
    );
}

#[test]
fn two_registration_orders_compose_the_same_route_set() {
    let routes = [
        (RouteMethod::Get, "/seats"),
        (RouteMethod::Post, "/seats"),
        (RouteMethod::Get, "/seats/{id}"),
        (RouteMethod::Delete, "/seats/{id}"),
        (RouteMethod::Get, "/files/{*rest}"),
    ];

    let mut forward = ApiRouterBuilder::new(bound(16));
    for (method, raw) in routes {
        if let Err(refusal) = forward.register(method, path(raw), ok) {
            panic!("registering {raw} was refused: {refusal}");
        }
    }

    let mut reverse = ApiRouterBuilder::new(bound(16));
    for (method, raw) in routes.iter().rev() {
        if let Err(refusal) = reverse.register(*method, path(raw), ok) {
            panic!("registering {raw} was refused: {refusal}");
        }
    }

    let forward = forward.seal();
    let reverse = reverse.seal();

    assert_eq!(
        forward.route_set(),
        reverse.route_set(),
        "the same registrations in a different order must compose the same route \
         set; an insertion-ordered set would make composition depend on how the \
         caller happened to sequence it"
    );
    assert_eq!(forward.len(), routes.len());

    // Asserted directly against the expected set, not merely against the other
    // builder — two identically wrong orderings would agree with each other.
    assert_eq!(
        forward.route_set(),
        vec![
            "DELETE /seats/{id}".to_owned(),
            "GET /files/{*rest}".to_owned(),
            "GET /seats".to_owned(),
            "GET /seats/{id}".to_owned(),
            "POST /seats".to_owned(),
        ]
    );
}

#[test]
fn a_zero_capacity_is_refused_rather_than_defaulted() {
    // A zero bound would present as "everything is refused" and read like a
    // working deny-all, which is why there is no `Default` anywhere near it.
    match MaxRouterModules::try_new(0) {
        Ok(_) => panic!("a zero router capacity must be refused"),
        Err(refusal) => assert!(
            matches!(
                refusal,
                ApiRefusal::BoundNotPositive {
                    bound: "max_router_modules"
                }
            ),
            "the refusal names the router bound; got {refusal:?}"
        ),
    }
}

#[test]
fn axum_really_panics_on_what_the_builder_refuses() {
    // The proof that the refusals above stand in front of something real.
    //
    // Without this, `a_conflicting_route_registration_is_refused` would be
    // satisfied by a builder that refused an input axum handles perfectly well,
    // and the "one law, one carrier" argument for owning the Router would rest
    // on a reading of axum's source rather than on its behaviour.
    let previous = std::panic::take_hook();
    // The unwind is expected, so its backtrace is noise that would read as a
    // failure in the harness log.
    std::panic::set_hook(Box::new(|_| {}));

    let conflict = catch_unwind(AssertUnwindSafe(|| {
        axum::Router::<()>::new()
            .route("/seats/{id}", axum::routing::get(ok))
            .route("/seats/{name}", axum::routing::get(ok))
    }));

    let duplicate = catch_unwind(AssertUnwindSafe(|| {
        axum::Router::<()>::new()
            .route("/seats", axum::routing::get(ok))
            .route("/seats", axum::routing::get(ok))
    }));

    let colon = catch_unwind(AssertUnwindSafe(|| {
        axum::Router::<()>::new().route("/seats/:id", axum::routing::get(ok))
    }));

    // Two balanced, validly named captures sharing one segment. The builder
    // now calls this an OCCUPANCY defect rather than malformed syntax, and
    // that reclassification would be worth nothing if the input were one axum
    // accepted: the refusal has to keep standing in front of a real panic.
    let shared_segment = catch_unwind(AssertUnwindSafe(|| {
        axum::Router::<()>::new().route("/seats/{x}b{y}c", axum::routing::get(ok))
    }));

    std::panic::set_hook(previous);

    assert!(
        conflict.is_err(),
        "axum must panic on two parameter names in one position, or the \
         builder's conflict refusal is guarding nothing"
    );
    assert!(
        duplicate.is_err(),
        "axum must panic on the same method registered twice on one path"
    );
    assert!(
        colon.is_err(),
        "axum must panic on a v0.7 colon capture segment"
    );
    assert!(
        shared_segment.is_err(),
        "axum must panic on two parameters sharing one segment, or naming that \
         refusal an occupancy defect would be a distinction drawn in front of \
         nothing"
    );
}
