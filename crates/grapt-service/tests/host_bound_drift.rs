//! Binds [`HostBoundName`] to the host's real limits struct, so this crate's
//! list of host bounds cannot rot into a quiet under-recording.
//!
//! # The defect this exists to make impossible
//!
//! `grapt-service` deliberately does **not** depend on `grapt-api`. That
//! keeps the recording honest — no `ApiHostLimits` is constructed here and none
//! of its eight newtypes is re-declared, so there is no second authority on
//! whether a bound value is valid. But it also means the compiler cannot see
//! the struct this crate claims to mirror. On the day a tenth bound lands in
//! `grapt-api`, a hand-maintained list of eight would keep admitting
//! recordings that silently omit it, and every witness in
//! `differential_contract.rs` would stay green: they all check this crate's
//! list against itself.
//!
//! So the real struct is the oracle here. This gate reads
//! `crates/grapt-api/src/limits.rs` and refuses unless the declared field set
//! equals [`HostBoundName::ALL`], in order. The list becomes a **checked
//! projection** of the authoritative struct, which is the ratified direction
//! for a generated-versus-authoritative pair, rather than a parallel truth.
//!
//! # Reading source rather than depending on it
//!
//! This is a read, not an edge: no `grapt-api` symbol is imported, no value of
//! its types is constructed, and the crate graph is unchanged — the
//! dependency-direction gate still sees `grapt-types` as this crate's only
//! `grapt-` dependency. What crosses the boundary is the field *names*, which
//! is exactly the thing this crate must not get wrong.
//!
//! # Two ways this gate could lie, both closed
//!
//! * **It could pass because it found nothing.** An extractor that silently
//!   returned an empty field set would make every comparison trivially true, so
//!   [`fields_of`] refuses any body line it does not recognise instead of
//!   skipping it, and the tests below drive it against synthetic sources where
//!   the real file cannot reach — an unparsable field, a missing struct, an
//!   unterminated body, and a near-miss struct name.
//! * **It could pass or fail on line endings.** The source is checked out with
//!   CRLF under `core.autocrlf=true` and with LF otherwise. Every line is
//!   normalised before matching, and a test drives the extractor over a CRLF
//!   copy of the real file to prove the two agree.

use std::path::{Path, PathBuf};

use grapt_service::HostBoundName;

/// The authoritative struct this crate's bound list mirrors.
const HOST_LIMITS_STRUCT: &str = "ApiHostLimits";

fn host_limits_source_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../grapt-api/src/limits.rs")
}

/// Read the host's limits source, or fail loudly.
///
/// An absent file is a loud failure and never a skip: a gate that quietly
/// passed when it could not find its oracle would be worse than no gate, since
/// its green would be read as agreement.
fn read_host_limits_source() -> String {
    let path = host_limits_source_path();
    match std::fs::read_to_string(&path) {
        Ok(source) => source,
        Err(error) => panic!(
            "cannot read the authoritative host limits at {}: {error}. \
             This gate binds this crate's host bound names to that struct; \
             without it the list is unverified, which is a failure, not a skip.",
            path.display()
        ),
    }
}

/// The `pub` field names declared by `struct_name`, in declaration order.
///
/// Refuses rather than skips. Every line inside the body must be blank, a
/// comment/doc line, or a `pub <name>: <type>,` field — anything else (an
/// attribute, a private field, a nested item) returns an error instead of being
/// passed over, because a skipped line is how an extractor quietly under-reads
/// the struct it is checking.
///
/// The struct header is matched on the whole trimmed line, so
/// `ApiHostLimitsBuilder` cannot satisfy a search for `ApiHostLimits`.
fn fields_of(source: &str, struct_name: &str) -> Result<Vec<String>, String> {
    let normalised = source.replace("\r\n", "\n");
    let header = format!("pub struct {struct_name} {{");

    let mut lines = normalised.lines();
    loop {
        match lines.next() {
            Some(line) if line.trim() == header => break,
            Some(_) => continue,
            None => return Err(format!("`{header}` is not declared in this source")),
        }
    }

    let mut fields = Vec::new();
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "}" {
            return Ok(fields);
        }
        if trimmed.is_empty() || trimmed.starts_with("//") {
            continue;
        }
        let field = trimmed
            .strip_suffix(',')
            .and_then(|declaration| declaration.split_once(':'))
            .and_then(|(name, _)| name.trim().strip_prefix("pub "))
            .map(|name| name.trim().to_owned());
        match field {
            Some(name) if !name.is_empty() && !name.contains(' ') => fields.push(name),
            _ => {
                return Err(format!(
                    "unrecognised line in `{struct_name}`: {trimmed:?}. \
                     Refused rather than skipped — a skipped line is how this gate would under-read the struct."
                ))
            }
        }
    }
    Err(format!("`{struct_name}` body is never closed"))
}

#[test]
fn this_crates_bound_list_equals_the_hosts_real_field_set_in_order() {
    let source = read_host_limits_source();
    let declared = match fields_of(&source, HOST_LIMITS_STRUCT) {
        Ok(fields) => fields,
        Err(problem) => panic!("cannot read the authoritative field set: {problem}"),
    };
    let mirrored: Vec<String> = HostBoundName::ALL
        .into_iter()
        .map(|name| name.field_name().to_owned())
        .collect();

    // The divergence is described IN the assertion message, not left to the
    // `left`/`right` dump. Two different drifts — a bound omitted and a bound
    // renamed — otherwise produce a byte-identical failure record, and a
    // mutation battery that scores evidence would read them as one law
    // witnessed twice rather than two laws witnessed once. (The QS battery
    // caught exactly that: QS09 and QS10 collided until this named the drift.)
    let absent_here: Vec<&String> = declared
        .iter()
        .filter(|name| !mirrored.contains(name))
        .collect();
    let unknown_upstream: Vec<&String> = mirrored
        .iter()
        .filter(|name| !declared.contains(name))
        .collect();
    let misordered = absent_here.is_empty() && unknown_upstream.is_empty() && declared != mirrored;
    assert!(
        declared == mirrored,
        "this crate's bound list has drifted from `{HOST_LIMITS_STRUCT}`: \
         declared upstream but absent here {absent_here:?}; \
         named here but unknown upstream {unknown_upstream:?}; \
         same names in a different order: {misordered}. \
         The struct is the authority; update HostBoundName to match it (and the corpus with it). \
         A silent divergence here means a differential under-records the engine's bounds."
    );
}

#[test]
fn the_gate_agrees_with_itself_across_line_endings() {
    // A source-scanning gate that matched raw bytes would pass under one
    // checkout policy and fail under the other. Both forms must extract the
    // same field set.
    let source = read_host_limits_source();
    let lf = source.replace("\r\n", "\n");
    let crlf = lf.replace('\n', "\r\n");
    assert!(
        crlf.contains("\r\n"),
        "the CRLF form must actually contain CRLF or this control proves nothing"
    );
    assert_eq!(
        fields_of(&lf, HOST_LIMITS_STRUCT),
        fields_of(&crlf, HOST_LIMITS_STRUCT),
        "the extractor disagrees with itself across line endings"
    );
}

#[test]
fn a_tenth_field_is_detected_rather_than_ignored() {
    // The drift this gate exists to catch, driven synthetically because the
    // real struct cannot be made to have ten fields from here.
    let synthetic = "\
pub struct ApiHostLimits {
    /// doc
    pub max_connections: MaxConnections,
    pub max_requests_per_connection: MaxRequestsPerConnection,
    pub max_inflight_requests: MaxInflightRequests,
    pub max_inflight_streams: MaxInflightStreams,
    pub max_request_head_bytes: MaxRequestHeadBytes,
    pub max_headers: MaxHeaders,
    pub max_request_body_bytes: MaxRequestBodyBytes,
    pub header_read_allowance: HeaderReadAllowance,
    pub shutdown_drain_allowance: ShutdownDrainAllowance,
    pub max_concurrent_streams: MaxConcurrentStreams,
}
";
    let declared = match fields_of(synthetic, HOST_LIMITS_STRUCT) {
        Ok(fields) => fields,
        Err(problem) => panic!("the synthetic source must parse: {problem}"),
    };
    let mirrored: Vec<String> = HostBoundName::ALL
        .into_iter()
        .map(|name| name.field_name().to_owned())
        .collect();
    assert_ne!(
        declared, mirrored,
        "a tenth bound must be visible to this gate; if it is not, the gate cannot detect drift at all"
    );
    assert_eq!(declared.len(), 10);
}

#[test]
fn a_renamed_field_is_detected_rather_than_ignored() {
    // Drift is not only additive: a bound renamed upstream must also fail,
    // otherwise the corpus would keep recording a name the host no longer has.
    let synthetic = "\
pub struct ApiHostLimits {
    pub max_connections: MaxConnections,
    pub max_requests_per_connection: MaxRequestsPerConnection,
    pub max_inflight_requests: MaxInflightRequests,
    pub max_inflight_streams: MaxInflightStreams,
    pub max_request_head_bytes: MaxRequestHeadBytes,
    pub max_header_count: MaxHeaders,
    pub max_request_body_bytes: MaxRequestBodyBytes,
    pub header_read_allowance: HeaderReadAllowance,
    pub shutdown_drain_allowance: ShutdownDrainAllowance,
}
";
    let declared = match fields_of(synthetic, HOST_LIMITS_STRUCT) {
        Ok(fields) => fields,
        Err(problem) => panic!("the synthetic source must parse: {problem}"),
    };
    assert_eq!(declared.len(), 9, "a rename keeps the count at nine");
    let mirrored: Vec<String> = HostBoundName::ALL
        .into_iter()
        .map(|name| name.field_name().to_owned())
        .collect();
    assert_ne!(
        declared, mirrored,
        "a renamed bound keeps the field COUNT at nine, so a count-only gate would pass it"
    );
}

#[test]
fn a_reordered_field_set_is_detected_because_the_recording_order_is_the_projection_order() {
    // `RecordedHostBounds` stores bounds in `HostBoundName::ALL` order, so the
    // order is part of the recorded projection and not a cosmetic detail.
    let synthetic = "\
pub struct ApiHostLimits {
    pub max_requests_per_connection: MaxRequestsPerConnection,
    pub max_connections: MaxConnections,
    pub max_inflight_requests: MaxInflightRequests,
    pub max_inflight_streams: MaxInflightStreams,
    pub max_request_head_bytes: MaxRequestHeadBytes,
    pub max_headers: MaxHeaders,
    pub max_request_body_bytes: MaxRequestBodyBytes,
    pub header_read_allowance: HeaderReadAllowance,
    pub shutdown_drain_allowance: ShutdownDrainAllowance,
    pub stream_write_allowance: StreamWriteAllowance,
}
";
    let declared = match fields_of(synthetic, HOST_LIMITS_STRUCT) {
        Ok(fields) => fields,
        Err(problem) => panic!("the synthetic source must parse: {problem}"),
    };
    let mirrored: Vec<String> = HostBoundName::ALL
        .into_iter()
        .map(|name| name.field_name().to_owned())
        .collect();
    assert_ne!(declared, mirrored, "a reordering must be visible");
    let mut sorted_declared = declared.clone();
    let mut sorted_mirrored = mirrored.clone();
    sorted_declared.sort();
    sorted_mirrored.sort();
    assert_eq!(
        sorted_declared, sorted_mirrored,
        "the reordering case must differ ONLY in order, or it is testing something else"
    );
}

#[test]
fn a_near_miss_struct_name_does_not_satisfy_the_search() {
    // `ApiHostLimitsBuilder` starts with the name this gate looks for. Matching
    // a substring would let an unrelated struct answer for the authority.
    let synthetic = "\
pub struct ApiHostLimitsBuilder {
    pub whatever: Whatever,
}
";
    match fields_of(synthetic, HOST_LIMITS_STRUCT) {
        Ok(fields) => panic!("a near-miss struct name must not answer the search: {fields:?}"),
        Err(problem) => assert!(
            problem.contains("is not declared"),
            "expected a not-declared refusal, got: {problem}"
        ),
    }
}

#[test]
fn an_unrecognised_body_line_is_refused_rather_than_skipped() {
    // The under-reading failure mode, driven directly. A private field or an
    // attribute must stop the gate, not be passed over — skipping is precisely
    // how an extractor reports agreement it did not establish.
    for hostile in [
        "pub struct ApiHostLimits {\n    #[serde(default)]\n    pub max_connections: MaxConnections,\n}\n",
        "pub struct ApiHostLimits {\n    private_field: usize,\n}\n",
        "pub struct ApiHostLimits {\n    pub max_connections: MaxConnections,\n",
    ] {
        if let Ok(fields) = fields_of(hostile, HOST_LIMITS_STRUCT) {
            panic!("an unrecognised body must be refused, but it yielded {fields:?}");
        }
    }
}

#[test]
fn the_extractor_finds_real_fields_so_a_passing_comparison_is_not_vacuous() {
    // The control that stops "both sides empty" from reading as agreement.
    let source = read_host_limits_source();
    let declared = match fields_of(&source, HOST_LIMITS_STRUCT) {
        Ok(fields) => fields,
        Err(problem) => panic!("the real source must parse: {problem}"),
    };
    assert!(
        !declared.is_empty(),
        "the extractor returned no fields from the real struct; an empty comparison is vacuously equal"
    );
    assert!(
        declared.contains(&"max_connections".to_owned()),
        "the extractor did not find a field known to exist upstream: {declared:?}"
    );
}
