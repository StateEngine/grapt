//! A decode failure must not carry authored bytes across a public boundary.
//!
//! A configuration file is the ordinary home of an API key, so `Debug` and
//! `Display` over a `JsoncError` are read channels and a rendered value path is
//! another. Master §8.4 forbids publishing secret-derived material.
//!
//! Every authored fragment a refusal can carry is exercised with a canary in it:
//! the offending number literal, a duplicate key, an invalid escape, the byte
//! that was not expected, and the key names along the path.

use crate::support;

use grapt_config::{jsonc, ConfigKey, JsoncErrorKind};
use support::bounds;

const CANARY: &str = "skXliveXCANARYX9f2a4c8e";

fn refusal(body: &str) -> grapt_config::JsoncError {
    match jsonc::decode(body.as_bytes(), bounds()) {
        Ok(_) => panic!("expected {body:?} to be refused"),
        Err(error) => error,
    }
}

/// Every public rendering of every authored-byte-carrying refusal.
#[test]
fn no_public_rendering_of_a_decode_failure_reveals_authored_bytes() {
    let cases = [
        // the offending number literal, scanned to the next structural byte
        format!("{{\"a\": 01{CANARY}}}"),
        // a duplicate key
        format!("{{\"{CANARY}\":1,\"{CANARY}\":2}}"),
        // key names along the path to a nested failure
        format!("{{\"{CANARY}\": {{\"inner{CANARY}\": 01x}}}}"),
        // an unexpected byte where a key was required
        format!("{{{CANARY}}}"),
        // an unterminated string containing the canary
        format!("{{\"a\": \"{CANARY}"),
    ];
    for body in &cases {
        let error = refusal(body);
        for rendering in [
            format!("{error}"),
            format!("{error:?}"),
            format!("{:?}", error.kind()),
            error.path().render(),
            format!("{:?}", error.path()),
            format!("{}", error.path()),
        ] {
            assert!(
                !rendering.contains(CANARY),
                "a public rendering revealed authored bytes: {rendering}"
            );
            assert!(
                !rendering.contains("skXlive"),
                "a public rendering revealed an authored prefix: {rendering}"
            );
        }
    }
}

/// The redaction TEXT is pinned, so a change that starts printing a length or a
/// prefix is a visible diff rather than a quiet weakening. `grapt_policy::Secret`
/// sets the precedent: "no length, no prefix — nothing that narrows the material".
#[test]
fn a_redacted_fragment_renders_as_a_marker_carrying_no_length_and_no_prefix() {
    let error = refusal(&format!("{{\"a\": 01{CANARY}}}"));
    assert_eq!(
        format!("{error}"),
        "invalid number literal <redacted> at $.#0 (byte 6)"
    );
    assert!(matches!(error.kind(), JsoncErrorKind::InvalidNumber { .. }));
}

/// The path stays USEFUL after redaction: a key segment renders as its ordinal
/// in its own object, which locates the value exactly and leaks nothing.
#[test]
fn a_redacted_path_still_locates_the_failure_by_ordinal_and_offset() {
    let body = format!("{{\"first\":1,\"{CANARY}\":{{\"a\":1,\"b\":01x}}}}");
    let error = refusal(&body);
    assert_eq!(error.path().render(), "$.#1.#1");
    assert_eq!(&body[error.offset()..error.offset() + 3], "01x");
}

/// The typed kind survives redaction. A caller that must branch on WHY a
/// document was refused still can; only the bytes are gone.
#[test]
fn the_typed_kind_and_offset_survive_redaction() {
    for (body, expect_kind) in [
        (format!("{{\"{CANARY}\":1,\"{CANARY}\":2}}"), "DuplicateKey"),
        (format!("{{\"a\":\"\\q{CANARY}\"}}"), "InvalidEscape"),
        (format!("{{{CANARY}}}"), "Expected"),
    ] {
        let error = refusal(&body);
        let rendered = format!("{:?}", error.kind());
        assert!(
            rendered.starts_with(expect_kind),
            "expected {expect_kind}, got {rendered}"
        );
    }
}

/// `ConfigKey::new` is public, so a caller can hold a secret-bearing key and
/// format it directly — a channel no decode-path witness above reaches. The
/// marker text is pinned so a length or prefix oracle is a visible diff.
#[test]
fn direct_debug_of_a_public_config_key_reveals_no_authored_bytes() {
    let Some(key) = ConfigKey::new(CANARY) else {
        panic!("a non-empty key must be admitted");
    };
    let rendered = format!("{key:?}");
    assert_eq!(rendered, "ConfigKey(<redacted>)");
    assert!(!rendered.contains(CANARY), "revealed authored bytes");
    assert!(!rendered.contains("skXlive"), "revealed a prefix");
    assert_eq!(key.as_str(), CANARY, "the accessor still reaches it");
}

/// U1b1's surface: a discovery names paths, and a path is secret-bearing.
///
/// A configuration path carries a home directory, a user name and a project
/// name. Every public rendering the census reaches — the census itself, one
/// entry, an identity, the file name, an outcome, and a plan refusal — is
/// exercised with a canary in the directory names AND in the file name.
#[test]
fn no_public_rendering_of_a_discovered_source_reveals_path_bytes() {
    use support::source_support::{abs, file_name, identity, source_bounds};

    let secret_dir = format!("home-{CANARY}");
    let secret_file = format!("{CANARY}.jsonc");

    let start = identity(&[secret_dir.as_str(), "project"]);
    let name = file_name(&secret_file);
    let document = abs(&[secret_dir.as_str(), "project", secret_file.as_str()]);

    let reader =
        grapt_config::DeterministicSourceReader::new().with_file(&document, b"{}".to_vec());
    let plan = grapt_config::DiscoveryPlan::new(start.clone(), Some(start.clone()), name.clone());
    let census = match grapt_config::discover(&plan, &reader, source_bounds()) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };

    let Some(entry) = census.entries().last() else {
        panic!("the census is empty");
    };
    let mut renderings = vec![
        format!("{census:?}"),
        format!("{entry:?}"),
        format!("{plan:?}"),
        format!("{start:?}"),
        format!("{start}"),
        format!("{name:?}"),
        format!("{:?}", entry.outcome()),
    ];
    for entry in census.entries() {
        renderings.push(format!("{:?}", entry.path()));
    }

    for rendering in renderings {
        assert!(
            !rendering.contains(CANARY),
            "a public rendering revealed path bytes: {rendering}"
        );
        assert!(
            !rendering.contains("skXlive"),
            "a public rendering revealed a path prefix: {rendering}"
        );
    }

    assert_eq!(
        start.as_path(),
        abs(&[secret_dir.as_str(), "project"]),
        "the accessor still reaches the path"
    );
}

/// The redaction markers are pinned, so a change that starts printing a path,
/// a component, or a length is a visible diff rather than a quiet weakening.
#[test]
fn a_redacted_config_source_path_renders_as_a_marker_carrying_no_component_and_no_length() {
    use support::source_support::identity;

    let path = identity(&["home", "example", "project"]);
    assert_eq!(format!("{path:?}"), "ConfigSourcePath(<redacted>)");
    assert_eq!(format!("{path}"), "<redacted>");

    let Some(name) = grapt_config::ConfigFileName::new("config.jsonc") else {
        panic!("a well-formed file name must be admitted");
    };
    assert_eq!(format!("{name:?}"), "ConfigFileName(<redacted>)");
    assert_eq!(
        name.as_str(),
        "config.jsonc",
        "the accessor still reaches it"
    );
}

/// A captured document is the ordinary home of an API key, so its `Debug`
/// carries neither the bytes nor their LENGTH.
///
/// `document::Redacted` sets the precedent: a length is a weak oracle, not
/// harmless shape. Pinned rather than merely asserted absent, so restoring a
/// length is a diff a reviewer sees.
#[test]
fn a_captured_document_renders_without_its_bytes_and_without_their_length() {
    use support::source_support::{abs, file_name, identity, source_bounds};

    let document = abs(&["project", "config.jsonc"]);
    let reader = grapt_config::DeterministicSourceReader::new()
        .with_file(&document, format!("{{\"key\":\"{CANARY}\"}}").into_bytes());
    let plan =
        grapt_config::DiscoveryPlan::new(identity(&["project"]), None, file_name("config.jsonc"));
    let census = match grapt_config::discover(&plan, &reader, source_bounds()) {
        Ok(census) => census,
        Err(refusal) => panic!("discovery refused: {refusal}"),
    };

    let Some(captured) = census.captured().next() else {
        panic!("the declared document was not captured");
    };
    assert_eq!(format!("{:?}", captured.outcome()), "captured");
    assert!(
        captured.outcome().bytes().is_some(),
        "the accessor still reaches the bytes"
    );
    assert!(
        !format!("{census:?}").contains(CANARY),
        "the census revealed a captured value"
    );
}
