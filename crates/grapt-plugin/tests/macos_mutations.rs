#![cfg(target_os = "macos")]
//! Native mutation witnesses for macOS watchdog containment and its
//! journalled reclaim (SC-MAC-02a, SC-MAC-02b).
//!
//! The macOS workflow's all-target plugin tests find this target with no
//! workflow change. It copies the workspace into fresh scratch it owns,
//! proves the native witnesses green there, applies each mutation of
//! macOS-only behaviour, requires its named witness (and only witnesses
//! allowed to depend on that behaviour) to fail as a test, never as a build
//! error, restores the source and proves them green again. The nested cargo
//! runs only the named lib witnesses (`--lib`), so it can never invoke this
//! target. Every phase has a deadline and a log bound; a phase that overruns
//! is killed with its whole process group. Jobs and test threads stay at
//! two, and logs are kept under `$RUNNER_TEMP/macos-results`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::os::unix::process::CommandExt as _;

const WITNESSES: &str = "supervisor::process::macos::tests";
/// The native witnesses the baseline must run and pass, no fewer.
const NATIVE_WITNESSES: usize = 11;
const BASELINE_BOUND: Duration = Duration::from_secs(420);
const MUTANT_BOUND: Duration = Duration::from_secs(180);
const LOG_BYTES: u64 = 32 * 1024 * 1024;
/// The whole harness, inside the workflow's 15-minute job cap.
const TOTAL_BOUND: Duration = Duration::from_secs(12 * 60);
/// How long a killed nested cargo group may take to be reaped.
const REAP_BOUND: Duration = Duration::from_secs(10);
const COPY_FILES: usize = 20_000;
const COPY_BYTES: u64 = 512 * 1024 * 1024;
const COPY_DEPTH: usize = 32;

const FORKED: &str = "a_forked_descendant_is_in_the_tree_and_dies_with_it";
const LEADER_EXITS: &str = "a_leader_that_exits_before_any_sample_cannot_strand_its_group";
const SETSID: &str =
    "a_setsid_child_is_contained_by_its_chain_and_reported_and_killed_once_it_escapes";
const ONE_TICK: &str = "one_tick_terminates_the_largest_tree_past_the_aggregate_budget";
const FAILED_START: &str = "a_fixture_that_fails_to_start_is_cleaned_up_within_its_bounds";
const GROUP_MEMBER: &str = "a_group_member_outside_the_parent_chain_is_in_the_tree_by_its_group";
const JOURNEY: &str = "a_journalled_orphan_is_reclaimed_by_its_group";

struct Mutation {
    name: &'static str,
    file: &'static str,
    original: &'static str,
    mutated: &'static str,
    /// The witness that must fail.
    required: &'static str,
    /// Every witness allowed to fail with it; any other red rejects the run.
    allowed: &'static [&'static str],
}

const MUTATIONS: [Mutation; 3] = [
    // The teardown loses its group kill, authorized by the held sentinel.
    Mutation {
        name: "N1-custody-never-signals-the-group",
        file: "src/supervisor/process/macos.rs",
        original: "        targets.push(Target::Group { group, holder });",
        mutated: "        let _ = (group, holder);",
        required: FORKED,
        allowed: &[FORKED, LEADER_EXITS, SETSID, ONE_TICK, FAILED_START],
    },
    // The bridge leads a group of its own instead of joining its sentinel's,
    // so the sentinel's group no longer holds it.
    Mutation {
        name: "N2-bridge-escapes-its-sentinels-group",
        file: "src/supervisor/process/macos.rs",
        original: "    command.env_clear().process_group(group);",
        mutated: "    command.env_clear().process_group({ let _ = group; 0 });",
        required: JOURNEY,
        allowed: &[JOURNEY],
    },
    // The snapshot loses every process's group.
    Mutation {
        name: "N3-snapshot-loses-groups",
        file: "src/supervisor/process/macos.rs",
        original: "                group,\n                started: process.start_time(),",
        mutated: "                group: { let _ = group; None },\n                started: process.start_time(),",
        required: GROUP_MEMBER,
        allowed: &[
            GROUP_MEMBER,
            FORKED,
            LEADER_EXITS,
            SETSID,
            ONE_TICK,
            FAILED_START,
            JOURNEY,
        ],
    },
];

/// Scratch this run created and owns; removed on every exit, panics included.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Copied {
    /// Files and directories alike count against the entry ceiling.
    entries: usize,
    bytes: u64,
}

/// Copy the workspace, minus build output and history, within stated
/// bounds. Symlinks are not followed or copied.
fn copy_tree(from: &Path, to: &Path, depth: usize, copied: &mut Copied) -> Result<(), String> {
    if depth > COPY_DEPTH {
        return Err(format!("{} exceeds the copy depth bound", from.display()));
    }
    std::fs::create_dir(to).map_err(|error| format!("{}: {error}", to.display()))?;
    for entry in std::fs::read_dir(from).map_err(|error| error.to_string())? {
        let entry = entry.map_err(|error| error.to_string())?;
        let name = entry.file_name();
        if name == "target" || name == ".git" {
            continue;
        }
        let metadata =
            std::fs::symlink_metadata(entry.path()).map_err(|error| error.to_string())?;
        let destination = to.join(&name);
        if metadata.is_dir() || metadata.is_file() {
            copied.entries += 1;
            copied.bytes += metadata.len();
            if copied.entries > COPY_FILES || copied.bytes > COPY_BYTES {
                return Err("the workspace exceeds the copy bounds".to_owned());
            }
        }
        if metadata.is_dir() {
            copy_tree(&entry.path(), &destination, depth + 1, copied)?;
        } else if metadata.is_file() {
            std::fs::copy(entry.path(), destination).map_err(|error| error.to_string())?;
        }
    }
    Ok(())
}

/// The nested cargo, owned: however the run ends (success, overrun, a probe
/// error, a panic), its whole process group is killed and the cargo is
/// reaped within `REAP_BOUND`. It is never waited on without a bound.
struct Nested(Option<std::process::Child>);

impl Nested {
    /// The exit, once observed; the child is then reaped and released.
    fn exited(&mut self) -> Result<Option<std::process::ExitStatus>, String> {
        let Some(child) = self.0.as_mut() else {
            return Err("nested cargo custody was already released".to_owned());
        };
        let status = child.try_wait().map_err(|error| error.to_string())?;
        if status.is_some() {
            self.0 = None;
        }
        Ok(status)
    }

    /// Kill the whole group and reap within the bound; report a survivor.
    fn stop(&mut self) -> Result<(), String> {
        let Some(mut child) = self.0.take() else {
            return Ok(());
        };
        if let Ok(group) = i32::try_from(child.id()) {
            if group > 1 {
                let _ = killpg(Pid::from_raw(group), Signal::SIGKILL);
            }
        }
        let _ = child.kill();
        let deadline = tokio::time::Instant::now() + REAP_BOUND;
        loop {
            if matches!(child.try_wait(), Ok(Some(_))) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err("the nested cargo survived its kill within the reap bound".to_owned());
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Nested {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// One nested witness run's evidence.
struct Run {
    success: bool,
    text: String,
}

impl Run {
    fn built(&self) -> bool {
        !self.text.contains("could not compile") && !self.text.contains("error[E")
    }

    fn ran(&self) -> Option<usize> {
        self.text.lines().find_map(|line| {
            line.strip_prefix("running ")?
                .strip_suffix(" tests")?
                .parse()
                .ok()
        })
    }

    fn failed(&self) -> Vec<String> {
        self.text
            .lines()
            .filter_map(|line| line.strip_prefix("test ")?.strip_suffix(" ... FAILED"))
            .map(|name| name.rsplit("::").next().unwrap_or(name).to_owned())
            .collect()
    }
}

struct Harness {
    tree: PathBuf,
    target: PathBuf,
    results: PathBuf,
    /// No phase runs past this, whatever its own bound.
    deadline: tokio::time::Instant,
}

impl Harness {
    /// Run the native witnesses in their own process group, logging live to
    /// a file. Past `bound`, or past the log bound, the whole group is
    /// killed and reaped and the phase is an error.
    fn run(&self, log: &str, bound: Duration) -> Result<Run, String> {
        let path = self.results.join(log);
        let file = std::fs::File::create(&path).map_err(|error| error.to_string())?;
        let errors = file.try_clone().map_err(|error| error.to_string())?;
        let child = Command::new(option_env!("CARGO").unwrap_or("cargo"))
            .current_dir(&self.tree)
            .env("CARGO_TARGET_DIR", &self.target)
            .env("CARGO_BUILD_JOBS", "2")
            .args(["test", "--locked", "-p", "grapt-plugin", "--lib", WITNESSES])
            .args(["--", "--test-threads=2"])
            .process_group(0)
            .stdout(file)
            .stderr(errors)
            .spawn()
            .map_err(|error| format!("nested cargo did not start: {error}"))?;
        let mut nested = Nested(Some(child));
        let deadline = (tokio::time::Instant::now() + bound).min(self.deadline);
        let status = loop {
            // A probe error returns through `?`; the guard still stops the group.
            if let Some(status) = nested.exited()? {
                break status;
            }
            let oversized = std::fs::metadata(&path).is_ok_and(|meta| meta.len() > LOG_BYTES);
            if tokio::time::Instant::now() >= deadline || oversized {
                let stopped = nested.stop();
                return Err(format!(
                    "{log}: exceeded its time or log bound; cleanup: {stopped:?}"
                ));
            }
            std::thread::sleep(Duration::from_millis(200));
        };
        let mut text = String::new();
        std::io::Read::read_to_string(
            &mut std::io::Read::take(
                std::fs::File::open(&path).map_err(|error| error.to_string())?,
                LOG_BYTES,
            ),
            &mut text,
        )
        .map_err(|error| error.to_string())?;
        Ok(Run {
            success: status.success(),
            text,
        })
    }

    fn green(&self, log: &str, bound: Duration) -> Result<(), String> {
        let run = self.run(log, bound)?;
        let passed = format!("test result: ok. {NATIVE_WITNESSES} passed");
        if run.success && run.ran() == Some(NATIVE_WITNESSES) && run.text.contains(&passed) {
            Ok(())
        } else {
            Err(format!(
                "{log}: expected all {NATIVE_WITNESSES} native witnesses to run and pass"
            ))
        }
    }
}

#[test]
fn native_mutations_each_fail_their_witness() -> Result<(), String> {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .parent()
        .and_then(Path::parent)
        .ok_or("the plugin crate is not inside the workspace")?;
    let temp = std::env::var_os("RUNNER_TEMP")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let root = temp.join(format!("grapt-macos-mutations-{}", std::process::id()));
    // Fresh scratch only: an existing directory is refused, never reused.
    std::fs::create_dir(&root).map_err(|error| format!("{}: {error}", root.display()))?;
    let scratch = Scratch(root);
    let harness = Harness {
        tree: scratch.0.join("source"),
        target: scratch.0.join("target"),
        results: temp.join("macos-results"),
        deadline: tokio::time::Instant::now() + TOTAL_BOUND,
    };
    std::fs::create_dir_all(&harness.results).map_err(|error| error.to_string())?;
    copy_tree(workspace, &harness.tree, 0, &mut Copied::default())?;
    let crate_dir = harness.tree.join("crates/grapt-plugin");
    harness.green("mutation-baseline-green.log", BASELINE_BOUND)?;
    let mut verdicts = Vec::new();
    for mutation in &MUTATIONS {
        let path = crate_dir.join(mutation.file);
        let source = std::fs::read_to_string(&path).map_err(|error| error.to_string())?;
        if source.matches(mutation.original).count() != 1 {
            return Err(format!("{}: mutation anchor is not unique", mutation.name));
        }
        std::fs::write(
            &path,
            source.replacen(mutation.original, mutation.mutated, 1),
        )
        .map_err(|error| error.to_string())?;
        let run = harness.run(&format!("mutation-{}-red.log", mutation.name), MUTANT_BOUND);
        std::fs::write(&path, &source).map_err(|error| error.to_string())?;
        let run = run?;
        let failed = run.failed();
        let unrelated: Vec<&String> = failed
            .iter()
            .filter(|name| !mutation.allowed.contains(&name.as_str()))
            .collect();
        let verdict = if !run.built() {
            Err("did not build".to_owned())
        } else if run.success || !failed.iter().any(|name| name == mutation.required) {
            Err(format!("survived: {} did not fail", mutation.required))
        } else if !unrelated.is_empty() {
            Err(format!("unrelated witnesses failed: {unrelated:?}"))
        } else {
            Ok(())
        };
        verdicts.push((mutation.name, verdict));
    }
    harness.green("mutation-restored-green.log", BASELINE_BOUND)?;
    let wrong: Vec<_> = verdicts
        .iter()
        .filter(|(_, verdict)| verdict.is_err())
        .collect();
    assert!(
        wrong.is_empty(),
        "mutations not killed by their witness: {wrong:?}"
    );
    Ok(())
}
