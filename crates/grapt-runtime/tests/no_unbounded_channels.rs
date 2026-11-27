//! Architecture gate: no unbounded queue may enter this crate.
//!
//! Contract §"Bounded handoff and backpressure": capacity is "a named,
//! measured configuration ... it is not an unbounded channel or a hidden SDK
//! buffer." Contract §"Negative mutations and compile-fail gates": "Replace
//! bounded channel with unbounded send → architecture test/lint fails."
//!
//! The oracle's live feed is `PubSub.unbounded` (bus.ts:168). The contract
//! calls the bounded in-engine notifier a correction, not parity — so an
//! unbounded channel reappearing here is a regression toward the defect, not
//! a style question.

use std::fs;
use std::path::Path;

/// Constructors that create an unbounded queue. Matching any of these in this
/// crate's source fails the build.
const FORBIDDEN: &[&str] = &[
    "unbounded_channel",
    "mpsc::unbounded",
    "UnboundedSender",
    "UnboundedReceiver",
    "futures::channel::mpsc::unbounded",
];

fn rust_sources(dir: &Path, found: &mut Vec<std::path::PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {}: {error}", dir.display()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

#[test]
fn no_unbounded_channel_constructor_appears_in_this_crate() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut sources = Vec::new();
    rust_sources(&root, &mut sources);
    assert!(
        !sources.is_empty(),
        "found no sources under {}",
        root.display()
    );

    let mut violations = Vec::new();
    for path in &sources {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => panic!("cannot read {}: {error}", path.display()),
        };
        for (number, line) in text.lines().enumerate() {
            // Skip the doc comment in this very file's spirit: prose naming
            // the forbidden constructor is not a use of it.
            let code = line.trim_start();
            if code.starts_with("//") {
                continue;
            }
            for needle in FORBIDDEN {
                if code.contains(needle) {
                    violations.push(format!(
                        "{}:{}: {}",
                        path.display(),
                        number + 1,
                        code.trim()
                    ));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "unbounded queue introduced — every queue must carry a named budget:\n{}",
        violations.join("\n")
    );
}

fn channel_lines(source: &str) -> Vec<&str> {
    source
        .lines()
        .map(str::trim)
        .filter(|line| !line.starts_with("//") && line.contains("mpsc::channel"))
        .collect()
}

const SUPERVISOR_PRODUCTION_FILES: [&str; 3] = [
    "src/supervisor.rs",
    "src/supervisor/attempt_loop.rs",
    "src/supervisor/task_drivers.rs",
];

#[test]
fn every_channel_is_constructed_with_a_named_budget() {
    // Positive side of the same gate: each mpsc::channel call must be given a
    // budget constant, never a bare literal.
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    assert_eq!(
        SUPERVISOR_PRODUCTION_FILES,
        [
            "src/supervisor.rs",
            "src/supervisor/attempt_loop.rs",
            "src/supervisor/task_drivers.rs",
        ],
        "the production channel scan must remain pinned to the exact supervisor file set"
    );

    let production_text = SUPERVISOR_PRODUCTION_FILES.map(|relative| {
        let path = manifest_dir.join(relative);
        assert!(
            path.is_file(),
            "the production channel scan lost its required source: {}",
            path.display()
        );
        match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) => panic!("cannot read {}: {error}", path.display()),
        }
    });
    let production = production_text.join("\n");

    let channels = channel_lines(&production);
    assert_eq!(
        channels.len(),
        3,
        "expected exactly three bounded channels in the production topology"
    );
    assert!(
        channels
            .iter()
            .any(|line| line.contains("PROVIDER_CHANNEL_CAPACITY")),
        "the provider channel must use its named budget: {channels:?}"
    );
    assert!(
        channels
            .iter()
            .any(|line| line.contains("TOOL_CHANNEL_CAPACITY")),
        "the tool channel must use its named budget: {channels:?}"
    );

    assert!(
        channels
            .iter()
            .any(|line| line.contains("NESTED_CHANNEL_CAPACITY")),
        "the nested admission channel must use its named budget: {channels:?}"
    );
    for line in channel_lines(&production) {
        assert!(
            line.contains("PROVIDER_CHANNEL_CAPACITY")
                || line.contains("TOOL_CHANNEL_CAPACITY")
                || line.contains("NESTED_CHANNEL_CAPACITY"),
            "a channel was opened without a named budget: {line}"
        );
    }
}
