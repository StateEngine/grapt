//! EB01, EB06, EB07 and EB12 — the laws observable only at the process
//! boundary.
//!
//! These four are properties of the binary as a caller meets it: which
//! arguments it demands, what it exits with, what it refuses to resolve, and
//! which stream carries the receipt. They are driven by running the built
//! binary rather than by calling into it, because a supervisor, a CI step and a
//! shell `&&` all see exactly this surface and nothing else.

use crate::receipt_scratch;

use std::process::{Command, Output};

/// Every bound `serve` demands, with a value, so a single omission can be made
/// the only difference between two invocations.
const EVERY_SERVE_BOUND: &[(&str, &str)] = &[
    ("--bind", "127.0.0.1:0"),
    ("--max-connections", "64"),
    ("--max-inflight", "32"),
    ("--max-headers", "64"),
    ("--max-request-head-bytes", "16384"),
    ("--max-request-body-bytes", "65536"),
    ("--max-requests-per-connection", "100"),
    ("--header-read-allowance-ms", "5000"),
    ("--shutdown-drain-allowance-ms", "5000"),
    ("--bind-attempts", "3"),
    ("--max-resolved-addresses", "4"),
    // The eleventh echoed bound, and the one this crate owns. It is in this
    // table for the same reason as the other ten: EB01 omits each entry in turn
    // and demands a refusal that names it, so a window that could be defaulted
    // would fail here first.
    ("--serve-window-ms", "50"),
];

/// Run the binary with the given arguments and a receipt root outside any
/// repository, so custody never becomes the reason a case refuses.
fn run(args: &[String]) -> Output {
    let scratch = receipt_scratch::outside_temp_dir();
    let receipt_root = scratch.join("grapt-engined-eb-arguments");
    Command::new(env!("CARGO_BIN_EXE_grapt-engined"))
        .args(args)
        .env("GRAPT_RECEIPT_ROOT", &receipt_root)
        .output()
        .unwrap_or_else(|error| panic!("the binary under test must be runnable: {error}"))
}

/// `serve` with every bound supplied except the one named.
fn serve_without(omitted: &str) -> Output {
    let mut args = vec!["serve".to_owned()];
    for (flag, value) in EVERY_SERVE_BOUND {
        if *flag == omitted {
            continue;
        }
        args.push((*flag).to_owned());
        args.push((*value).to_owned());
    }
    run(&args)
}

/// EB01. `BindLimits` and `ApiHostLimits` implement no `Default` under the
/// bounded-everything rule. A binary that invented one would become a second
/// carrier of the law those types exist to hold, and the operator who omitted a
/// bound would silently get a number nobody chose.
#[test]
fn an_omitted_connection_bound_refuses_and_never_defaults() {
    let output = serve_without("--max-connections");

    assert_ne!(
        output.status.code(),
        Some(0),
        "an omitted bound must not succeed"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"refusal\":\"argument\""),
        "the receipt must classify this as an argument refusal; got {stdout}"
    );
    assert!(
        stdout.contains("--max-connections"),
        "the refusal must NAME the omitted bound; got {stdout}"
    );

    // Every other declared bound refuses on its own too, so the row cannot be
    // satisfied by special-casing one flag.
    for (flag, _) in EVERY_SERVE_BOUND {
        if *flag == "--bind" {
            continue;
        }
        let output = serve_without(flag);
        assert_ne!(
            output.status.code(),
            Some(0),
            "omitting {flag} must refuse rather than default"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(flag),
            "the refusal for {flag} must name it; got {stdout}"
        );
    }

    // The bound this crate owns is not laxer than the bounds it borrows. Zero
    // is refused by the constructor, exactly as `ShutdownDrainAllowance`
    // refuses `Duration::ZERO`, and the refusal names the flag rather than
    // reporting a generic parse failure. A window admitting zero would be a
    // host asked to serve for no time at all, which is a different verb.
    let mut args = vec!["serve".to_owned()];
    for (flag, value) in EVERY_SERVE_BOUND {
        args.push((*flag).to_owned());
        args.push(if *flag == "--serve-window-ms" {
            "0".to_owned()
        } else {
            (*value).to_owned()
        });
    }
    let output = run(&args);
    assert_ne!(
        output.status.code(),
        Some(0),
        "a zero run window must be refused rather than served"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("--serve-window-ms") && stdout.contains("\"refusal\":\"argument\""),
        "the zero-window refusal must be an argument refusal naming the flag; got {stdout}"
    );
}

/// EB06. A refusal that exited zero is indistinguishable from success to every
/// caller that checks an exit code, which is every caller that does not parse
/// the receipt.
#[test]
fn every_typed_refusal_exits_non_zero() {
    let cases: Vec<Vec<String>> = vec![
        // a missing bound
        vec!["serve".to_owned()],
        // an unknown verb
        vec!["teleport".to_owned()],
        // an unknown flag
        vec![
            "serve".to_owned(),
            "--not-a-flag".to_owned(),
            "1".to_owned(),
        ],
        // a service verb with no registration
        vec!["service".to_owned(), "status".to_owned()],
    ];

    for args in cases {
        let output = run(&args);
        assert_eq!(
            output.status.code(),
            Some(2),
            "a typed refusal exits 2; args were {args:?}, stdout {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

/// EB07. R11: a literal address only. Resolving a hostname is a network effect
/// this binary does not take, so a name is refused rather than looked up.
#[test]
fn a_hostname_is_refused_as_adapter_unavailable_and_never_bound() {
    let mut args = vec!["serve".to_owned()];
    for (flag, value) in EVERY_SERVE_BOUND {
        args.push((*flag).to_owned());
        args.push(if *flag == "--bind" {
            "localhost:8080".to_owned()
        } else {
            (*value).to_owned()
        });
    }
    let output = run(&args);

    assert_ne!(output.status.code(), Some(0), "a hostname must not bind");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("\"refusal\":\"bind\""),
        "a hostname is a bind refusal; got {stdout}"
    );
    assert!(
        stdout.contains("resolves no hostname"),
        "the refusal must say the binary resolves no hostname; got {stdout}"
    );
}

/// EB12. A caller reading stdout gets one parseable object or nothing at all.
/// Prose interleaved into that stream makes the receipt unparseable for the one
/// consumer it exists to serve.
#[test]
fn stdout_carries_exactly_one_json_object_and_prose_goes_to_stderr() {
    let output = run(&["teleport".to_owned()]);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    let lines: Vec<&str> = stdout.lines().filter(|line| !line.is_empty()).collect();
    assert_eq!(
        lines.len(),
        1,
        "stdout carries EXACTLY one line; got {lines:?}"
    );
    let object = lines[0];
    assert!(
        object.starts_with('{') && object.ends_with('}'),
        "the single stdout line is one JSON object; got {object:?}"
    );
    assert_eq!(
        object.matches("\"operation\"").count(),
        1,
        "one object, one operation key; got {object:?}"
    );

    assert!(
        !stderr.is_empty(),
        "the prose companion goes to stderr and must not be dropped"
    );
    assert!(
        !stderr.contains('{'),
        "prose must not be JSON and must not leak onto the receipt stream; got {stderr:?}"
    );
    assert!(
        !stdout.contains("refused:"),
        "the prose form must not appear on stdout; got {stdout:?}"
    );
}
