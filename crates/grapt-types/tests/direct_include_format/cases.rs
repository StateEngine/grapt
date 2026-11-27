use super::*;

#[test]
fn every_direct_include_carrier_is_pinned_rustfmt_clean() -> Result<(), Box<dyn Error>> {
    // Held for the whole walk: a sibling test creates a real directory under
    // crates/, and an entry appearing mid-walk moves this census.
    let _tree = lock_tree();
    let root = workspace_root();
    let inventory = inventory(&root)?;
    let mut sites = Vec::new();
    for (path, bytes) in &inventory.files {
        let discovered = detect_sites(path, bytes).map_err(|error| format!("{path}: {error}"))?;
        let mut admitted = sites.len();
        add_bounded(&mut admitted, discovered.len(), MAX_SITES, "include sites")?;
        sites.extend(discovered);
    }
    sites.sort_by(|left, right| (&left.carrier, left.start).cmp(&(&right.carrier, right.start)));
    if sites.is_empty() {
        return Err("the direct-include formatting gate found no sites".into());
    }
    let targets: BTreeSet<&str> = sites.iter().map(|site| site.target.as_str()).collect();
    let mut carriers = BTreeSet::new();
    for site in &sites {
        insert_bounded_carrier(&mut carriers, &site.carrier)?;
    }
    if targets.len() != sites.len() {
        return Err("a direct-include fragment has more than one owner".into());
    }
    let carrier_count = carriers.len();
    for site in &sites {
        resolve_target(&root, &site.carrier, &site.target, &inventory.files)?;
    }
    reject_graph_cycles(&sites)?;
    let fragment_bytes = targets.iter().try_fold(0_usize, |mut total, target| {
        add_bounded(
            &mut total,
            inventory.files[*target].len(),
            MAX_FRAGMENT_BYTES,
            "fragment bytes",
        )?;
        Ok::<usize, String>(total)
    })?;
    let fragment_lines: usize = targets
        .iter()
        .map(|target| physical_lines(&inventory.files[*target]))
        .sum();
    let integration = sites
        .iter()
        .filter(|site| site.carrier.split('/').nth(2) == Some("tests"))
        .count();
    let version = run_bounded(&["--version"], None)?;
    if version.status != 0 || version.stdout != EXPECTED_VERSION || !version.stderr.is_empty() {
        return Err(format!(
            "wrong rustfmt toolchain/version: {:?} {:?}",
            version.stdout, version.stderr
        )
        .into());
    }
    let mut subprocesses = 1;
    let mut total_expanded = 0;
    for carrier in carriers {
        add_bounded(&mut subprocesses, 1, MAX_SUBPROCESSES, "subprocesses")?;
        let carrier_sites: Vec<Site> = sites
            .iter()
            .filter(|site| site.carrier == carrier)
            .cloned()
            .collect();
        let expanded = expand_carrier(
            &inventory.files[carrier],
            &carrier_sites,
            &inventory.files,
            MAX_EXPANDED_BYTES,
            &mut total_expanded,
            MAX_TOTAL_EXPANDED_BYTES,
        )?;
        if !detect_sites(carrier, &expanded)?.is_empty() {
            return Err(
                format!("logical carrier {carrier} retained an unexpanded include macro").into(),
            );
        }
        let fragments: Vec<&str> = carrier_sites
            .iter()
            .map(|site| site.target.as_str())
            .collect();
        require_carrier_format(carrier, &fragments, &expanded, format_bytes(&expanded))?;
    }
    // Counts describe this source tree; bounds and structural checks govern it.
    // Every discovered carrier above must have reached the pinned formatter.
    assert_eq!(subprocesses, carrier_count + 1);
    assert!(!inventory.files.is_empty());
    assert!(inventory.total_bytes > 0);
    println!(
        "formatted {carrier_count} carriers, {} sites, {integration} integration; \
         {fragment_lines} fragment lines/{fragment_bytes} bytes; {} Rust files, {} directories, {} entries",
        sites.len(), inventory.files.len(), inventory.directories, inventory.entries
    );
    Ok(())
}

#[test]
fn bounded_lexer_and_path_controls_are_non_vacuous() -> Result<(), Box<dyn Error>> {
    let accepted = b"// include!(\"ignored.rs\");\nconst TEXT: &str = \"include!(\\\"ignored.rs\\\");\";\n/* outer /* include!(\"ignored.rs\"); */ */\ninclude!(\"part.rs\");\n";
    let sites = detect_sites("crates/example/tests/carrier.rs", accepted)?;
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].target, "crates/example/tests/part.rs");
    for bad in [
        b"include !(\"part.rs\");\n".as_slice(),
        b"include!(r\"part.rs\");\n",
        b"include!(PATH);\n",
        b"include!(\"part.rs\"); trailing\n",
    ] {
        assert!(detect_sites("crates/example/tests/carrier.rs", bad).is_err());
    }
    for bad in [
        "",
        "/x.rs",
        "C:/x.rs",
        "a\\x.rs",
        "a//x.rs",
        "./x.rs",
        "../x.rs",
        "a/../x.rs",
        "a:x.rs",
        "x",
        "x\0.rs",
    ] {
        assert!(validate_include_path(bad).is_err(), "admitted {bad:?}");
    }
    assert!(validate_include_path("nested/part.rs").is_ok());
    assert!(require_utf8(&[0xff], "invalid.rs").is_err());
    let permission = map_read_result::<()>(
        Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied)),
        Path::new("unreadable.rs"),
        "read",
    );
    assert!(permission.is_err());
    let mut folded = BTreeMap::new();
    assert!(insert_case_folded(&mut folded, "crates/a/src/Part.rs").is_ok());
    assert!(insert_case_folded(&mut folded, "crates/a/src/part.rs").is_err());
    Ok(())
}

#[test]
fn every_declared_bound_has_exact_plus_one_and_overflow_controls() {
    for (label, cap) in [
        ("depth", MAX_DEPTH),
        ("directories", MAX_DIRECTORIES),
        ("entries", MAX_ENTRIES),
        ("files", MAX_FILES),
        ("file bytes", MAX_FILE_BYTES),
        ("total file bytes", MAX_TOTAL_FILE_BYTES),
        ("carriers", MAX_CARRIERS),
        ("sites", MAX_SITES),
        ("fragment bytes", MAX_FRAGMENT_BYTES),
        ("expanded bytes", MAX_EXPANDED_BYTES),
        ("total expanded bytes", MAX_TOTAL_EXPANDED_BYTES),
        ("diagnostics", MAX_DIAGNOSTIC_BYTES),
        ("subprocesses", MAX_SUBPROCESSES),
    ] {
        let mut exact = 0;
        assert!(add_bounded(&mut exact, cap, cap, label).is_ok());
        assert_eq!(exact, cap);
        assert!(add_bounded(&mut exact, 1, cap, label).is_err());
        let mut overflow = usize::MAX;
        assert!(add_bounded(&mut overflow, 1, usize::MAX, label).is_err());
    }

    const TEST_CAP: usize = 8;
    let exact_source = [b'x'; TEST_CAP];
    let mut exact_total = 0;
    let exact = expand_carrier(
        &exact_source,
        &[],
        &BTreeMap::new(),
        TEST_CAP,
        &mut exact_total,
        TEST_CAP,
    );
    assert_eq!(exact.map(|bytes| bytes.len()), Ok(TEST_CAP));
    assert_eq!(exact_total, TEST_CAP);

    let over_source = [b'x'; TEST_CAP + 1];
    let mut over_total = 0;
    assert!(expand_carrier(
        &over_source,
        &[],
        &BTreeMap::new(),
        TEST_CAP,
        &mut over_total,
        usize::MAX,
    )
    .is_err());

    let mut overflow_total = usize::MAX;
    assert_eq!(
        expand_carrier(
            b"x",
            &[],
            &BTreeMap::new(),
            TEST_CAP,
            &mut overflow_total,
            usize::MAX,
        ),
        Err("expanded carrier byte count overflow".to_owned()),
        "expand_carrier must refuse arithmetic overflow through its own seam"
    );
    assert_eq!(overflow_total, usize::MAX);
}

#[test]
fn resolver_and_graph_refusals_are_independent() {
    assert!(admit_entry(
        EntryFacts {
            symlink: true,
            reparse: false,
            contained: true
        },
        "symlink"
    )
    .is_err());
    assert!(admit_entry(
        EntryFacts {
            symlink: false,
            reparse: true,
            contained: true
        },
        "reparse"
    )
    .is_err());
    assert!(admit_entry(
        EntryFacts {
            symlink: false,
            reparse: false,
            contained: false
        },
        "escape"
    )
    .is_err());
    assert!(require_inventory_target(&BTreeMap::new(), "crates/a/src/missing.rs").is_err());
    let site = Site {
        carrier: "crates/a/src/a.rs".to_owned(),
        target: "crates/a/src/a.rs".to_owned(),
        start: 0,
        end: 1,
    };
    assert!(reject_graph_cycles(&[site]).is_err());
    let mutual = vec![
        Site {
            carrier: "crates/a/src/a.rs".to_owned(),
            target: "crates/a/src/b.rs".to_owned(),
            start: 0,
            end: 1,
        },
        Site {
            carrier: "crates/a/src/b.rs".to_owned(),
            target: "crates/a/src/a.rs".to_owned(),
            start: 0,
            end: 1,
        },
    ];
    assert!(reject_graph_cycles(&mutual).is_err());
}

#[test]
fn misformatted_fragment_and_skipped_expansion_cannot_mask_each_other() -> Result<(), Box<dyn Error>>
{
    let source = b"fn wrapper() {\ninclude!(\"part.rs\");\n}\n";
    let sites = detect_sites("crates/example/tests/carrier.rs", source)?;
    let files = BTreeMap::from([(
        "crates/example/tests/part.rs".to_owned(),
        b"fn badly_formatted( ){ }\n".to_vec(),
    )]);
    let mut total_expanded = 0;
    let expanded = expand_carrier(
        source,
        &sites,
        &files,
        MAX_EXPANDED_BYTES,
        &mut total_expanded,
        MAX_TOTAL_EXPANDED_BYTES,
    )?;
    assert_ne!(
        format_bytes(&expanded)?,
        expanded,
        "production-migration-class misformat was not detected"
    );
    let skipped = expand_carrier(
        source,
        &[],
        &files,
        MAX_EXPANDED_BYTES,
        &mut total_expanded,
        MAX_TOTAL_EXPANDED_BYTES,
    )?;
    assert_eq!(skipped, source);
    assert_eq!(
        detect_sites("crates/example/tests/carrier.rs", &skipped)?.len(),
        1,
        "skipped expansion erased raw/consumed inequality evidence"
    );
    Ok(())
}

#[test]
fn formatter_failure_classes_remain_distinct() {
    let nonzero = ProcessResult {
        status: 1,
        stdout: Vec::new(),
        stderr: b"bad input".to_vec(),
    };
    let failure = match validate_formatter_result(nonzero) {
        Ok(_) => panic!("nonzero rustfmt was admitted"),
        Err(error) => error,
    };
    assert!(failure.contains("bad input"));
}

#[test]
fn formatter_wait_accepts_load_delay_and_the_last_probe() -> Result<(), Box<dyn Error>> {
    assert_eq!(
        FORMATTER_POLL_DELAY.checked_mul(u32::try_from(FORMATTER_POLLS)?),
        Some(Duration::from_secs(u64::try_from(FORMATTER_WAIT_SECONDS)?)),
    );
    // 500 probes outlive the historical 400-probe budget; the final probe
    // witnesses an exit during the last permitted sleep. No wall-clock wait.
    for completed_probe in [500, FORMATTER_POLLS + 1] {
        let mut probes = 0;
        let mut pauses = 0;
        let status = poll_formatter(
            || {
                probes += 1;
                Ok((probes == completed_probe).then_some(0))
            },
            || pauses += 1,
        )?;
        assert_eq!(status, 0);
        assert_eq!(probes, completed_probe);
        assert_eq!(pauses, completed_probe - 1);
    }
    Ok(())
}

#[test]
fn formatter_timeout_and_format_diff_name_the_carrier_separately() {
    let carrier = "crates/example/tests/carrier.rs";
    let fragments = ["crates/example/tests/part.rs"];
    let source = b"fn badly_formatted( ){ }\n";
    let mut probes = 0;
    let mut pauses = 0;
    let timeout = poll_formatter(
        || {
            probes += 1;
            Ok(None::<()>)
        },
        || pauses += 1,
    )
    .map(|()| Vec::new());
    let timeout_error = require_carrier_format(carrier, &fragments, source, timeout)
        .err()
        .unwrap_or_else(|| panic!("a timed-out formatter was admitted"));
    assert_eq!(probes, FORMATTER_POLLS + 1);
    assert_eq!(pauses, FORMATTER_POLLS);
    assert!(timeout_error.contains(carrier));
    assert!(timeout_error.contains("rustfmt timed out after its 60s poll sleep budget"));
    assert!(!timeout_error.contains("not pinned-rustfmt clean"));

    let formatted = validate_formatter_result(ProcessResult {
        status: 0,
        stdout: b"fn badly_formatted() {}\n".to_vec(),
        stderr: Vec::new(),
    });
    let diff_error = require_carrier_format(carrier, &fragments, source, formatted)
        .err()
        .unwrap_or_else(|| panic!("a real format diff was admitted"));
    assert!(diff_error.contains(carrier));
    assert!(diff_error.contains(fragments[0]));
    assert!(diff_error.contains("not pinned-rustfmt clean"));
    assert!(!diff_error.contains("timed out"));
}

/// Removes the owned scratch copy, including when an assertion panics.
struct WitnessArtifact {
    directory: PathBuf,
}

impl Drop for WitnessArtifact {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

const H1_CASES: &str = "crates/grapt-types/tests/direct_include_format/cases.rs";

fn ignored_artifact_source() -> Result<WitnessArtifact, String> {
    static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let directory = (0..32)
        .find_map(|_| {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let name = format!(
                "grapt-direct-include-ignored-{}-{sequence}",
                std::process::id()
            );
            let path = std::env::temp_dir().join(name);
            match fs::create_dir(&path) {
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                created => Some(created.map(|()| path)),
            }
        })
        .ok_or("all 32 witness root names already exist")?
        .map_err(|error| format!("cannot create witness root: {error}"))?;
    let witness = WitnessArtifact {
        directory: directory.clone(),
    };
    let source = workspace_root();
    let relative = Path::new(H1_CASES);
    let copied = directory.join(relative);
    fs::create_dir_all(copied.parent().ok_or("witness source has no parent")?)
        .map_err(|error| format!("cannot create witness source directory: {error}"))?;
    for (from, to) in [
        (source.join(".gitignore"), directory.join(".gitignore")),
        (source.join(relative), copied),
    ] {
        fs::copy(&from, &to).map_err(|error| {
            format!("cannot copy {} into witness root: {error}", from.display())
        })?;
    }
    // Track the copied bytes, as they are tracked in the real checkout.
    for args in [
        &["init", "--quiet"][..],
        &["add", "--", ".gitignore", H1_CASES],
    ] {
        let status = Command::new("git")
            .arg("-C")
            .arg(&directory)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| format!("cannot run witness git {args:?}: {error}"))?;
        if !status.success() {
            return Err(format!("witness git {args:?} exited {status}"));
        }
    }
    Ok(witness)
}

#[test]
fn an_ignored_runtime_artifact_cannot_move_the_inventory() {
    // The real inventory and ignore rule over copied tracked bytes in an owned
    // Git root; the candidate checkout is never written.
    let scratch = ignored_artifact_source()
        .unwrap_or_else(|error| panic!("the ignored-artifact source must exist: {error}"));
    let root = &scratch.directory;
    let clean =
        inventory(root).unwrap_or_else(|error| panic!("the clean walk must succeed: {error}"));

    // The real __pycache__/ rule must collapse this fresh directory to one
    // ignored entry. A similarly named directory would prove a different rule.
    let directory = root
        .join("crates/grapt-types/tests/direct_include_format")
        .join("__pycache__");
    fs::create_dir(&directory)
        .unwrap_or_else(|error| panic!("the witness artifact must be creatable: {error}"));
    for name in ["run.cpython-314.pyc", "test_run.cpython-314.pyc"] {
        if let Err(error) = fs::write(directory.join(name), b"witness bytes") {
            panic!("the witness artifact must be writable: {error}");
        }
    }

    // ANTI-VACUITY, FIRST: were this an ORDINARY untracked file the counts would
    // legitimately move, so prove git ignores it before concluding anything.
    let ignored = ignored_relatives(root)
        .unwrap_or_else(|error| panic!("the ignored listing must be readable: {error}"));
    let relative = "crates/grapt-types/tests/direct_include_format/__pycache__";
    assert!(
        is_ignored(relative, &ignored),
        "`{relative}` is not git-ignored, so this witness cannot reproduce H1"
    );

    let poisoned = inventory(root)
        .unwrap_or_else(|error| panic!("the poisoned walk must still succeed: {error}"));

    // Ignored artifacts must leave the source inventory unchanged.
    assert_eq!(
        (poisoned.files.len(), poisoned.directories, poisoned.entries),
        (clean.files.len(), clean.directories, clean.entries),
        "an ignored runtime artifact moved the source inventory"
    );

    // And it was SEEN and skipped, not merely absent: EXACTLY ONE more skip,
    // because the walk counts the ignored directory once and never recurses to
    // its two `.pyc` files. A policy that recursed into ignored trees would look
    // correct and be quietly unbounded; the exact number catches it.
    assert_eq!(
        poisoned.ignored_skipped,
        clean.ignored_skipped + 1,
        "the poisoned walk did not skip exactly one more entry than the clean one \
         ({} vs {}); either the artifact was never observed, or the policy \
         recursed into an ignored tree instead of skipping it whole",
        poisoned.ignored_skipped,
        clean.ignored_skipped
    );
    fs::remove_dir_all(&directory)
        .unwrap_or_else(|error| panic!("the witness artifact must be removable: {error}"));
    let restored =
        inventory(root).unwrap_or_else(|error| panic!("the restored walk must succeed: {error}"));
    assert_eq!(restored.files, clean.files);
    assert_eq!(restored.directories, clean.directories);
    assert_eq!(restored.entries, clean.entries);
    assert_eq!(restored.ignored_skipped, clean.ignored_skipped);
    fs::remove_dir_all(root)
        .unwrap_or_else(|error| panic!("the witness root must be removable: {error}"));
}

#[test]
fn the_ignored_path_policy_matches_on_segment_boundaries() {
    // `git ls-files --directory` collapses a wholly-ignored directory to one
    // entry, so descendants must be matched by PREFIX. A raw prefix match would
    // let an ignored `foo` swallow a tracked `foobar`, silently removing real
    // source from the census — a far worse failure than the one being fixed,
    // because it makes the count too SMALL and the pin would simply be repinned
    // to the wrong number.
    let ignored: BTreeSet<String> = ["crates/a/target", "crates/b/__pycache__"]
        .into_iter()
        .map(str::to_owned)
        .collect();

    assert!(
        is_ignored("crates/a/target", &ignored),
        "the directory itself"
    );
    assert!(
        is_ignored("crates/a/target/debug/x.rs", &ignored),
        "a descendant of an ignored directory"
    );
    assert!(is_ignored("crates/b/__pycache__/x.pyc", &ignored));

    assert!(
        !is_ignored("crates/a/targets/lib.rs", &ignored),
        "`targets` must NOT be swallowed by ignored `target`"
    );
    assert!(
        !is_ignored("crates/a/target_helper.rs", &ignored),
        "`target_helper.rs` must NOT be swallowed by ignored `target`"
    );
    assert!(!is_ignored("crates/a/src/lib.rs", &ignored));
}

/// A child that writes a large, deterministic amount to STDOUT.
fn stdout_producer(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--", "crates"]);
    command
}

/// A child that writes a large, deterministic amount to STDERR and exits nonzero.
fn stderr_producer(root: &Path) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(root)
        .args(["ls-files", "--definitely-not-a-flag"]);
    command
}

/// The exact byte length one producer emits on the stream under test.
///
/// MEASURED at run time rather than pinned. The listing length moves with the
/// tree and git's usage text moves with git's version, so a literal here would be
/// a second census free to drift — and its drift would look like a bound defect.
/// Measuring is also what makes the exact/exact+1 pair genuinely exact.
fn measured_len(command: Command, stream: &'static str) -> usize {
    let capture = match capture_bounded(command, 4 * 1024 * 1024, 4 * 1024 * 1024) {
        Ok(capture) => capture,
        Err(failure) => panic!("the measuring capture must succeed: {}", failure.describe()),
    };
    let length = if stream == "stdout" {
        capture.stdout.len()
    } else {
        capture.stderr.len()
    };
    assert!(
        length > 16,
        "the {stream} producer emitted {length} bytes, too few for an exact/exact+1 pair"
    );
    length
}

#[test]
fn the_stdout_cap_is_enforced_at_acquisition_exactly_and_at_exactly_one_over() {
    // ETA'S H1-B1. The first version used `Command::output()`, which reads BOTH
    // pipes to end and only then compared a length against a constant — the
    // allocation had already happened, so the declared bound was not a resource
    // bound at all. These witnesses drive the REAL child through the REAL
    // capture path.
    let root = workspace_root();
    let exact = measured_len(stdout_producer(&root), "stdout");

    // EXACTLY AT the cap: admitted, and the bytes are all there.
    match capture_bounded(stdout_producer(&root), exact, MAX_IGNORED_STDERR_BYTES) {
        Ok(capture) => assert_eq!(
            capture.stdout.len(),
            exact,
            "a producer exactly at its cap must be admitted whole"
        ),
        Err(failure) => panic!(
            "exactly at the cap must be admitted: {}",
            failure.describe()
        ),
    }

    // EXACTLY ONE over: refused, naming the stream and the cap.
    match capture_bounded(stdout_producer(&root), exact - 1, MAX_IGNORED_STDERR_BYTES) {
        Err(CaptureFailure::StdoutOverflow { cap }) => assert_eq!(cap, exact - 1),
        other => panic!("one byte over the cap must be refused, got {other:?}"),
    }
}

#[test]
fn the_stderr_cap_is_enforced_at_acquisition_exactly_and_at_exactly_one_over() {
    // The stream that was not capped AT ALL before this correction.
    let root = workspace_root();
    let exact = measured_len(stderr_producer(&root), "stderr");

    match capture_bounded(stderr_producer(&root), MAX_IGNORED_LISTING_BYTES, exact) {
        Ok(capture) => {
            assert_eq!(
                capture.stderr.len(),
                exact,
                "a producer exactly at its cap must be admitted whole"
            );
            // And the nonzero exit is REPORTED rather than swallowed: the cap and
            // the exit status are independent facts.
            assert_ne!(
                capture.code,
                Some(0),
                "the stderr producer must exit nonzero, or this witness is measuring \
                 the wrong command"
            );
        }
        Err(failure) => panic!(
            "exactly at the cap must be admitted: {}",
            failure.describe()
        ),
    }

    match capture_bounded(stderr_producer(&root), MAX_IGNORED_LISTING_BYTES, exact - 1) {
        Err(CaptureFailure::StderrOverflow { cap }) => assert_eq!(cap, exact - 1),
        other => panic!("one byte over the cap must be refused, got {other:?}"),
    }
}

#[test]
fn a_producer_far_beyond_its_cap_is_terminated_rather_than_waited_on() {
    // THE DEADLOCK WITNESS, and the reason the drain threads are not simply
    // joined. `drain_pipe` stops reading at `cap + 1`, so a child with far more
    // to say then blocks forever writing into a pipe nobody is emptying. If this
    // test HANGS, the poll-and-kill loop is gone.
    //
    // Both streams, because each has its own pipe and its own way to hang.
    let root = workspace_root();
    match capture_bounded(stdout_producer(&root), 1, MAX_IGNORED_STDERR_BYTES) {
        Err(CaptureFailure::StdoutOverflow { cap }) => assert_eq!(cap, 1),
        other => panic!("a grossly oversized stdout must be refused, got {other:?}"),
    }
    match capture_bounded(stderr_producer(&root), MAX_IGNORED_LISTING_BYTES, 1) {
        Err(CaptureFailure::StderrOverflow { cap }) => assert_eq!(cap, 1),
        other => panic!("a grossly oversized stderr must be refused, got {other:?}"),
    }
}

#[test]
fn every_capture_failure_stays_a_distinct_named_condition() {
    // These are distinct conditions with distinct cures, and the witnesses above
    // match on the VARIANT rather than on prose. This pins that the variants stay
    // distinguishable and that each renders something a reader can act on — a
    // vocabulary whose members collapse into one another cannot diagnose
    // anything.
    let failures = [
        CaptureFailure::Spawn("no such file".to_owned()),
        CaptureFailure::StdoutPipeAbsent,
        CaptureFailure::StderrPipeAbsent,
        CaptureFailure::StdoutOverflow { cap: 7 },
        CaptureFailure::StderrOverflow { cap: 7 },
        CaptureFailure::Timeout,
        CaptureFailure::Wait("gone".to_owned()),
        CaptureFailure::Read("broken".to_owned()),
        CaptureFailure::ThreadPanicked("stdout"),
        CaptureFailure::NonZeroExit {
            code: Some(129),
            stderr: "unknown option".to_owned(),
        },
        CaptureFailure::NotUtf8("invalid".to_owned()),
    ];
    let rendered: BTreeSet<String> = failures.iter().map(CaptureFailure::describe).collect();
    assert_eq!(
        rendered.len(),
        failures.len(),
        "two capture failures render identically, so a reader cannot tell them apart"
    );
    // The two overflows differ by STREAM, not only by cap — the whole point of
    // keeping a separate flag per pipe.
    assert_ne!(
        CaptureFailure::StdoutOverflow { cap: 7 }.describe(),
        CaptureFailure::StderrOverflow { cap: 7 }.describe()
    );
    for failure in &failures {
        assert!(!failure.describe().trim().is_empty());
    }
}

#[test]
fn both_pipe_caps_are_in_the_declared_bound_inventory() {
    // Review: "the exact/exact+1 bound inventory test also omits the new listing
    // cap." These are PIPE caps rather than counter caps, so they cannot be
    // exercised through `add_bounded` the way the counters are. Their inventory
    // is `drain_pipe` at exactly the cap and at exactly one over, driven through
    // an in-memory reader so it is deterministic and needs no child at all.
    //
    // The formatter's two caps are included as well: they were always pipe caps
    // and the counter inventory could never have exercised them either.
    for (label, cap) in [
        ("ignored listing", MAX_IGNORED_LISTING_BYTES),
        ("ignored stderr", MAX_IGNORED_STDERR_BYTES),
        ("diagnostics", MAX_DIAGNOSTIC_BYTES),
        ("formatter", FORMATTER_BYTE_LIMIT),
    ] {
        assert!(cap > 0, "{label} cap must be non-zero");

        let exact = vec![b'x'; cap];
        let flag = Arc::new(AtomicBool::new(false));
        match drain_pipe(exact.as_slice(), cap, Arc::clone(&flag)) {
            Ok(bytes) => assert_eq!(bytes.len(), cap, "{label} exactly at its cap"),
            Err(error) => panic!("{label} exactly at its cap must be admitted: {error}"),
        }
        assert!(
            !flag.load(Ordering::SeqCst),
            "{label} flagged overflow while exactly at its cap"
        );

        let over = vec![b'x'; cap + 1];
        let flag = Arc::new(AtomicBool::new(false));
        match drain_pipe(over.as_slice(), cap, Arc::clone(&flag)) {
            Ok(bytes) => assert_eq!(
                bytes.len(),
                cap + 1,
                "{label} must stop reading at cap + 1 and never consume the rest"
            ),
            Err(error) => panic!("{label} one over its cap must still return: {error}"),
        }
        assert!(
            flag.load(Ordering::SeqCst),
            "{label} did not flag overflow at exactly one over its cap"
        );

        // THE ACQUISITION PROOF, and the ONLY assertion here that can tell an
        // acquisition-time bound from a post-allocation check.
        //
        // Both shapes detect overflow: reading everything and then comparing the
        // length reports the same refusal. What separates them is how much was
        // ALLOCATED to find out. A producer far beyond the cap must yield exactly
        // `cap + 1` bytes — one more than the cap, which is the least that proves
        // the limit was passed and the most a bounded reader may hold. If this
        // returns the whole input, the read is unbounded and the declared bound
        // is decoration, which is exactly the defect review named in H1-B1.
        let far = vec![b'x'; cap + 1_024];
        let flag = Arc::new(AtomicBool::new(false));
        match drain_pipe(far.as_slice(), cap, Arc::clone(&flag)) {
            Ok(bytes) => assert_eq!(
                bytes.len(),
                cap + 1,
                "{label} allocated {} bytes for a cap of {cap}; the bound is enforced \
                 AFTER the read rather than during it",
                bytes.len()
            ),
            Err(error) => panic!("{label} far over its cap must still return: {error}"),
        }
        assert!(flag.load(Ordering::SeqCst), "{label} far over its cap");
    }
}
