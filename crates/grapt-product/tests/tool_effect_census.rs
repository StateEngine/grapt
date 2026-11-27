//! Structural census for the one production concrete-effect entry.

const TOOLS: &str = include_str!("../src/dogfood/tools.rs");
const RUNNER: &str = include_str!("../src/dogfood/tools/runner.rs");

#[test]
fn concrete_executor_is_reached_once_and_only_inside_permission_consume_and_run() {
    assert_eq!(
        TOOLS.matches("governed.run_effect(").count()
            + RUNNER.matches("governed.run_effect(").count(),
        1,
        "production must have exactly one concrete bounded executor invocation"
    );
    let gate = match RUNNER.find(".consume_and_run(move |capability|") {
        Some(gate) => gate,
        None => panic!("permission consume-and-run gate is absent"),
    };
    let invocation = match RUNNER.find("governed.run_effect(") {
        Some(invocation) => invocation,
        None => panic!("concrete bounded executor invocation is absent"),
    };
    assert!(
        gate < invocation && invocation - gate < 256,
        "concrete executor invocation escaped the permission closure"
    );
    assert!(
        !TOOLS.contains("run_authorized")
            && !TOOLS.contains("AuthorityLease")
            && !RUNNER.contains("run_authorized")
            && !RUNNER.contains("AuthorityLease"),
        "product must not own a lease or a second authorized entry"
    );
}

/// Supplements the real-path journey, which proves the command raises its own
/// shell permission: no plugin host module may reach a process directly.
#[test]
fn plugin_host_modules_cannot_spawn_integration_commands_directly() {
    let sources = plugin_product_sources();
    // plugins.rs, the seven plugin host modules and execution/plugin.rs.
    assert!(
        sources.len() >= 9,
        "the plugin product census lost its sources: {:?}",
        sources.iter().map(|(name, _)| name).collect::<Vec<_>>()
    );
    for (name, source) in sources {
        let compact: String = source.chars().filter(|c| !c.is_whitespace()).collect();
        for spawn in PROCESS_SPAWNS {
            assert!(
                !compact.contains(spawn),
                "{name} spawns plugin-declared argv outside the governed effect path ({spawn})"
            );
        }
    }
}

/// Every spelling by which product code reaches a child process directly.
const PROCESS_SPAWNS: [&str; 8] = [
    "run_integration_command",
    "process::Command",
    "Command::new",
    "tokio::process",
    "std::process::",
    "spawn_with_required_environment",
    "windows_spawn",
    "processkit",
];

/// Every plugin product module, walked from disk so a new module cannot escape
/// the census, plus the executor entry that runs plugin host effects.
fn plugin_product_sources() -> Vec<(String, String)> {
    let dogfood = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/dogfood");
    let mut pending = vec![dogfood.join("plugins")];
    let mut files = vec![
        dogfood.join("plugins.rs"),
        dogfood.join("execution").join("plugin.rs"),
    ];
    while let Some(directory) = pending.pop() {
        let entries = std::fs::read_dir(&directory)
            .unwrap_or_else(|error| panic!("{} is unreadable: {error}", directory.display()));
        for entry in entries {
            let path = entry
                .unwrap_or_else(|error| panic!("plugin source entry is unreadable: {error}"))
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files
        .into_iter()
        .map(|path| {
            let name = path
                .strip_prefix(&dogfood)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            let source = std::fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{name} is unreadable: {error}"));
            (name, source)
        })
        .collect()
}
