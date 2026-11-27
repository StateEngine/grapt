//! Architecture gate: the R4H-H host is what it claims to be, in SOURCE.
//!
//! Behavioural witnesses prove what the host does with the composition it has.
//! They cannot prove what it is NOT built out of — a host that quietly used
//! `axum::serve` would pass every bound test that `axum::serve` happens to
//! satisfy, and would fail only the ones nobody wrote. This gate is the other
//! half: it reads the production source and refuses the constructions the lane
//! is forbidden to contain.
//!
//! # Prose is not code, so prose is removed first
//!
//! Every module here DISCUSSES `axum::serve`, HTTP/2 and upgrades at length,
//! because explaining why they are refused is part of the argument. A gate that
//! searched raw text would therefore be red on its own documentation, and the
//! obvious fix — deleting the explanations — would be the worst possible
//! outcome. So [`executable_source`] strips line comments, block comments,
//! string literals and character literals before anything is searched, and
//! [`the_stripper_removes_only_prose`] holds that stripper to its own contract.
//!
//! `#[cfg(test)]` modules are stripped too. What is being asserted is a property
//! of the SHIPPED library; a witness that constructs a deterministic double is
//! not a production call site, and folding the two together would either
//! forbid the doubles or excuse the production code.

/// One production module, embedded at compile time.
///
/// `include_str!` rather than a filesystem read: deleting or renaming one of
/// these stops this target compiling, so a module cannot leave the gate's sight
/// by being moved.
const PRODUCTION: [(&str, &str); 5] = [
    ("limits.rs", include_str!("../src/limits.rs")),
    ("deadline.rs", include_str!("../src/deadline.rs")),
    ("admission.rs", include_str!("../src/admission.rs")),
    ("http1.rs", include_str!("../src/http1.rs")),
    ("host.rs", include_str!("../src/host.rs")),
];

/// Strip comments, literals and `#[cfg(test)]` modules.
///
/// A small scanner rather than a regex, because the distinction between a token
/// in code and the same token inside a `//` explanation or a `"string"` is
/// exactly what a regex cannot make.
fn executable_source(source: &str) -> String {
    let source = strip_test_module(source);
    let bytes: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(bytes.len());
    let mut index = 0usize;
    while index < bytes.len() {
        let current = bytes[index];
        let next = bytes.get(index + 1).copied();
        match (current, next) {
            ('/', Some('/')) => {
                while index < bytes.len() && bytes[index] != '\n' {
                    index += 1;
                }
            }
            ('/', Some('*')) => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == '*' && bytes[index + 1] == '/') {
                    index += 1;
                }
                index = (index + 2).min(bytes.len());
            }
            ('"', _) => {
                // A string literal. Escapes are honoured so that a `\"` does not
                // end it early and leave the rest of the file mis-scanned.
                index += 1;
                while index < bytes.len() && bytes[index] != '"' {
                    if bytes[index] == '\\' {
                        index += 1;
                    }
                    index += 1;
                }
                index += 1;
                out.push_str("\"\"");
            }
            ('\'', Some(_)) => {
                // A character literal or a lifetime. Lifetimes are identifiers
                // and must survive; only a real `'x'` or `'\n'` is stripped.
                let closes_at = if bytes.get(index + 1) == Some(&'\\') {
                    index + 3
                } else {
                    index + 2
                };
                if bytes.get(closes_at) == Some(&'\'') {
                    index = closes_at + 1;
                    out.push_str("''");
                } else {
                    out.push(current);
                    index += 1;
                }
            }
            _ => {
                out.push(current);
                index += 1;
            }
        }
    }
    out
}

/// Cut a file at its `#[cfg(test)]` module, if it has one.
///
/// Newline-agnostic, and R1 HOLD-2 is why. This used to search for the literal
/// `"#[cfg(test)]\nmod tests {"`. On a CRLF checkout the file contains `\r\n`,
/// the search found nothing, NOTHING was stripped, and `admission.rs`'s
/// `#[cfg(test)]` module — which legitimately calls `tokio::time::timeout` in a
/// witness — was scanned as production source. The clock-placement gate then
/// fired on test code: the candidate did not pass its own focused gate on a
/// clean checkout by anyone else on this platform.
///
/// `crates/grapt-api/.gitattributes` now pins these paths to LF, but this is
/// deliberately fixed independently of that. A gate that is correct only because
/// the checkout was configured correctly is a gate that fails silently the first
/// time it is not, and this one reads bytes from disk for a living.
///
/// Scanning lines rather than matching a joined literal: `trim` removes the
/// `\r`, so LF and CRLF sources cut at the same place.
fn strip_test_module(source: &str) -> &str {
    let mut offset = 0usize;
    let mut cfg_test_at: Option<usize> = None;
    for line in source.split_inclusive('\n') {
        let trimmed = line.trim();
        if let Some(start) = cfg_test_at {
            if trimmed == "mod tests {" {
                return &source[..start];
            }
            // `#[cfg(test)]` on anything else — a fixture route, a single item —
            // is not the trailing module, so the marker resets rather than
            // arming until it happens to meet a `mod tests {` further down.
            cfg_test_at = None;
        }
        if trimmed == "#[cfg(test)]" {
            cfg_test_at = Some(offset);
        }
        offset += line.len();
    }
    source
}

/// Every production module's executable source, paired with its name.
fn executable_modules() -> Vec<(&'static str, String)> {
    PRODUCTION
        .iter()
        .map(|(name, source)| (*name, executable_source(source)))
        .collect()
}

#[test]
fn the_stripper_removes_only_prose() {
    // The gate's own contract, asserted directly. Every check below is worth
    // exactly what this is: a stripper that removed too much would make every
    // forbidden-construction test vacuous, and one that removed too little would
    // make the documentation unwritable.
    let source = "\
// a comment naming axum::serve
/* a block comment naming http2 */
let message = \"a string naming .unwrap()\";
let quote = \"an escaped \\\" quote naming unsafe\";
let newline = '\\n';
fn borrow<'a>(value: &'a str) -> &'a str { value }
let real = tokio::spawn(work);
#[cfg(test)]
mod tests {
    fn a_double_may_call_axum::serve() {}
}
";
    let stripped = executable_source(source);
    for removed in [
        "axum::serve",
        "http2",
        ".unwrap()",
        "unsafe",
        "a_double_may_call",
    ] {
        assert!(
            !stripped.contains(removed),
            "the stripper left {removed:?} behind: {stripped:?}"
        );
    }
    for kept in ["tokio::spawn(work)", "borrow<'a>", "&'a str"] {
        assert!(
            stripped.contains(kept),
            "the stripper removed executable code {kept:?}: {stripped:?}"
        );
    }
}

#[test]
fn the_forbidden_transport_constructions_are_absent_from_production() {
    // Each entry is a construction the preflight names, paired with why it is
    // refused. A bare list would be a list; this is the ruling transcribed.
    let forbidden: [(&str, &str); 12] = [
        (
            "axum::serve",
            "it accepts and spawns with no host-owned permit and hides the connection set",
        ),
        (
            "with_upgrades",
            "an upgraded connection leaves HTTP/1 and leaves every bound below it",
        ),
        (
            "conn::auto",
            "the automatic protocol builder cannot be configured with max_headers or a timer",
        ),
        (
            "http2",
            "H is an HTTP/1 lane; an HTTP/2 connection has different bounds and none of them are these",
        ),
        (
            "unbounded_channel",
            "bounded-everything: an unbounded channel is backpressure that is neither bounded nor visible",
        ),
        (
            "unbounded",
            "the same law, stated for every unbounded constructor rather than one name",
        ),
        (
            "SystemTime::now",
            "H2: production time comes from the injected authority, never the ambient wall clock",
        ),
        (
            "std::time::Instant::now",
            "the same, for the monotonic clock hyper adds its allowance to",
        ),
        (
            ".unwrap()",
            "the workspace denies unwrap_used; a transport boundary that panics on a value has turned a defect into a process kill",
        ),
        (
            ".expect(",
            "the same, for expect_used",
        ),
        (
            "unsafe",
            "the workspace forbids unsafe_code",
        ),
        (
            "#[allow(",
            "a lint suppression in this lane is an exception nobody reviewed; the two argument-count sites were fixed by bundling instead",
        ),
    ];
    let mut problems = Vec::new();
    for (module, source) in executable_modules() {
        for (construction, why) in forbidden {
            if source.contains(construction) {
                problems.push(format!("{module}: {construction:?} — {why}"));
            }
        }
    }
    assert!(
        problems.is_empty(),
        "these forbidden constructions are present in production source: {problems:#?}"
    );
}

#[test]
fn the_connection_task_is_spawned_exactly_once_and_only_under_a_permit() {
    // The connection bound is an ordering claim, and an ordering claim needs the
    // spawn site to be unique: a second one could be reached without the permit
    // and no behavioural witness would necessarily see it under load.
    let mut sites = Vec::new();
    for (module, source) in executable_modules() {
        for (index, line) in source.lines().enumerate() {
            if line.contains("spawn(") {
                sites.push((module, index + 1, line.trim().to_owned()));
            }
        }
    }

    // Two: the root task, and the one connection task. Anything else is a task
    // the host does not own.
    assert_eq!(
        sites.len(),
        2,
        "the host must spawn in exactly two places — the root and one connection \
         task — but found: {sites:#?}"
    );

    let connection_site = sites
        .iter()
        .find(|(_, _, line)| line.contains("tasks.spawn("));
    let Some((module, _, line)) = connection_site else {
        panic!("no connection-task spawn site was found among {sites:#?}");
    };
    assert_eq!(*module, "host.rs");
    assert!(
        line.contains("permit"),
        "the connection spawn must MOVE the already-acquired permit into the \
         task; a spawn that did not name it would leave the permit to be dropped \
         at the end of the accept iteration, and the bound would count nothing: \
         {line}"
    );
}

/// The clock names no production module may contain outside the named adapter.
///
/// One list, shared by the gate below and by its negative at the end of this
/// file, so the two cannot drift into agreeing about different things.
const CLOCK_NAMES: [&str; 3] = ["tokio::time", "TokioTimer", "Instant::now"];

/// The serialization names no production module may contain at all.
const SERIALIZATION_NAMES: [&str; 3] = ["Serialize", "Deserialize", "serde"];

/// Every name from `names` that appears in `source`.
fn present(source: &str, names: &[&str]) -> Vec<String> {
    names
        .iter()
        .filter(|name| source.contains(**name))
        .map(|name| (*name).to_owned())
        .collect()
}

#[test]
fn tokio_time_lives_only_in_the_named_deadline_adapter() {
    // H2 again, as a structural property rather than a review habit. `deadline.rs`
    // is the named adapter; every other production module must be unable to
    // reach a clock at all.
    let mut problems = Vec::new();
    for (module, source) in executable_modules() {
        if module == "deadline.rs" {
            continue;
        }
        for construction in present(&source, &CLOCK_NAMES) {
            problems.push(format!("{module}: {construction:?}"));
        }
    }
    assert!(
        problems.is_empty(),
        "these modules reach for a clock outside the named adapter: {problems:#?}"
    );

    // The same-file control: the adapter itself DOES contain them, so the check
    // above is about placement and not about the strings being absent from the
    // crate.
    let Some((_, adapter)) = executable_modules()
        .into_iter()
        .find(|(name, _)| *name == "deadline.rs")
    else {
        panic!("the named deadline adapter is not in the production set");
    };
    for construction in ["tokio::time", "TokioTimer"] {
        assert!(
            adapter.contains(construction),
            "the adapter must be where {construction:?} lives, or the placement \
             check above passes because nothing uses a clock at all"
        );
    }
}

#[test]
fn the_hyper_timer_overrides_now_rather_than_inheriting_it() {
    // `hyper::rt::Timer::now` has a DEFAULT implementation returning
    // `std::time::Instant::now()`, and hyper's HTTP/1 connection computes its
    // header deadline as `timer.now() + header_read_timeout` before handing it
    // to `sleep_until`. An adapter that inherited that default would mix an
    // ambient reading with a virtual-clock sleep.
    //
    // All four trait methods are asserted, not just `now`: a timer that
    // delegated three and inherited one is precisely the shape this is guarding
    // against, and only the enumeration says which four.
    let Some((_, adapter)) = executable_modules()
        .into_iter()
        .find(|(name, _)| *name == "deadline.rs")
    else {
        panic!("the named deadline adapter is not in the production set");
    };
    let Some(implementation) = adapter.split("impl Timer for HostTimer").nth(1) else {
        panic!("the hyper timer adapter `impl Timer for HostTimer` was not found");
    };
    for method in ["fn sleep(", "fn sleep_until(", "fn reset(", "fn now("] {
        assert!(
            implementation.contains(method),
            "the hyper timer must override {method:?} rather than inherit it"
        );
    }
}

#[test]
fn no_production_host_type_reaches_a_serialization_surface() {
    // I1/I2: the host, listener, principal, timer, permit and exit types are
    // private transport types, not generated DTOs. This crate takes no `serde`
    // dependency at all — `allowed_external_dependencies.rs` holds that — so
    // this gate states the consequence directly: there is no derive, no manual
    // impl, and no re-export that could grow one.
    let mut problems = Vec::new();
    for (module, source) in executable_modules() {
        for construction in present(&source, &SERIALIZATION_NAMES) {
            problems.push(format!("{module}: {construction:?}"));
        }
    }
    assert!(
        problems.is_empty(),
        "these production modules name a serialization surface: {problems:#?}"
    );
}

// ---------------------------------------------------------------------------
// The negative half for the two gates above.
//
// `QH18` and `QH22` witness the forbidden-construction and spawn-site gates
// firing, so the `executable_modules()` machinery is proven. What they do not
// prove is that the two REMAINING gates carry a token list that finds anything:
// a gate whose list had been emptied would pass every run and every same-file
// control, and would say nothing at all. Asserting an absence with no
// counterexample is a limit a reviewer can walk straight through — stating it
// would buy inherited judgement rather than scrutiny.
//
// These drive the same predicate over crafted sources, which is cheaper than a
// mutation row and tests the predicate rather than the tree.
// ---------------------------------------------------------------------------

#[test]
fn the_stripper_cuts_the_test_module_on_crlf_as_well_as_lf() {
    // R1 HOLD-2, witnessed rather than asserted. The old stripper matched a
    // literal `"#[cfg(test)]\nmod tests {"`, and its self-test fed it an inline
    // `\n` string — so the self-test validated the stripper against input whose
    // line endings could never be the checkout's, and passed on both checkouts
    // while the gate itself failed on one.
    //
    // This drives the REAL module that reproduced the failure, in both encodings.
    // `admission.rs` is the right subject and not an arbitrary one: its
    // `#[cfg(test)]` module legitimately calls `tokio::time::timeout`, so if the
    // strip fails the clock-placement gate fires on test code, which is exactly
    // what a fresh default-config clone did.
    let Some((_, admission)) = PRODUCTION.iter().find(|(name, _)| *name == "admission.rs") else {
        panic!("admission.rs is not in the production set");
    };

    // The embedded source may itself be either encoding depending on how this
    // working tree was checked out, so both forms are constructed explicitly and
    // neither depends on how the file arrived.
    let as_lf = admission.replace("\r\n", "\n");
    let as_crlf = as_lf.replace('\n', "\r\n");
    assert!(
        as_crlf.contains("\r\n") && !as_lf.contains('\r'),
        "the two encodings under test must actually differ"
    );

    for (label, source) in [("LF", as_lf.as_str()), ("CRLF", as_crlf.as_str())] {
        let stripped = executable_source(source);
        assert!(
            present(&stripped, &CLOCK_NAMES).is_empty(),
            "on a {label} checkout the test module was not stripped, so its \
             legitimate test-only clock use is being scanned as production source \
             — which is the false red a fresh clone reported"
        );
        // And the strip is a CUT, not a wipe: production content must survive,
        // or the gate would pass because it is reading nothing at all.
        assert!(
            stripped.contains("AdmissionService"),
            "on a {label} checkout the stripper removed production code as well"
        );
    }
}

#[test]
fn the_clock_search_finds_a_reading_and_ignores_prose_about_one() {
    // Both halves matter. A search that fired on prose would make the
    // explanation unwritable — every module here DISCUSSES the clock at length —
    // and one that fired on nothing would make the placement gate vacuous.
    let prose = executable_source(
        "//! This module explains why tokio::time and Instant::now are refused.\n\
         let message = \"TokioTimer\";\n\
         fn ordinary() {}\n",
    );
    assert!(
        present(&prose, &CLOCK_NAMES).is_empty(),
        "the clock search fired on a comment and a string literal: {prose:?}"
    );

    for reading in [
        "let at = tokio::time::Instant::now();",
        "let timer = TokioTimer::new();",
        "let at = Instant::now();",
    ] {
        let executable = executable_source(reading);
        assert!(
            !present(&executable, &CLOCK_NAMES).is_empty(),
            "the clock search missed an actual reading: {reading:?}"
        );
    }
}

#[test]
fn the_serialization_search_finds_a_surface_and_ignores_prose_about_one() {
    let prose = executable_source(
        "//! This crate takes no serde dependency, so Serialize and Deserialize\n\
         //! are unreachable.\n\
         fn ordinary() {}\n",
    );
    assert!(
        present(&prose, &SERIALIZATION_NAMES).is_empty(),
        "the serialization search fired on a comment: {prose:?}"
    );

    for surface in [
        "#[derive(Serialize)]\nstruct Host;",
        "impl Deserialize for Host {}",
        "use serde::ser::Serializer;",
    ] {
        let executable = executable_source(surface);
        assert!(
            !present(&executable, &SERIALIZATION_NAMES).is_empty(),
            "the serialization search missed an actual surface: {surface:?}"
        );
    }
}
