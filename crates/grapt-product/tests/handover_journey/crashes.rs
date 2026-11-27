use super::*;

fn cut(root: &Path, boundary: &str) -> Output {
    grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["handover", "--service"])
        .arg(root)
        .arg("--run-root")
        .arg(root.join("run"))
        .args([
            "--run-id",
            "shv-crash",
            "--engine-version",
            "secode-rust@crash",
        ])
        .env("GRAPT_HANDOVER_CRASH_AT", boundary)
        .output()
        .unwrap_or_else(|error| panic!("crash child: {error}"))
}

fn authorize_ready(root: &Path) {
    let path = root.join("grapt.jsonc");
    let config = fs::read_to_string(&path).unwrap_or_else(|error| panic!("config: {error}"));
    fs::write(path, config.replace("\"migrate\"", "\"open-ready\""))
        .unwrap_or_else(|error| panic!("recovery authority: {error}"));
}

fn restart(root: &Path, crash: bool) -> Output {
    let mut command =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
    command
        .args(["serve", "--service"])
        .arg(root)
        .args(["--port", "0"]);
    if crash {
        command.env("GRAPT_HANDOVER_CRASH_AT", "recovery");
    }
    command
        .output()
        .unwrap_or_else(|error| panic!("recovery child: {error}"))
}

#[test]
fn named_crash_boundaries_reopen_truthful_durable_state() {
    for (boundary, migrated, committed) in [
        ("before-retirement", false, false),
        ("after-retirement", false, false),
        ("before-commit", true, false),
        ("after-commit", true, true),
    ] {
        let root = scratch(boundary);
        write_inputs(&root, "secode-rust@crash");
        let source = root.join("database.sqlite3");
        legacy_database(&source);
        register_incumbent(&root, ABSENT_PID);
        let original =
            fs::read(root.join("registration")).unwrap_or_else(|error| panic!("original: {error}"));
        let source_before = fs::read(&source).unwrap_or_else(|error| panic!("source: {error}"));
        let output = cut(&root, boundary);
        assert_eq!(
            output.status.code(),
            Some(86),
            "{boundary}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let observed = fs::read(root.join("registration"))
            .unwrap_or_else(|error| panic!("{boundary} registration: {error}"));
        assert_eq!(
            observed != original,
            committed,
            "{boundary}: registration commit truth"
        );
        assert_eq!(
            user_version(&source),
            if migrated { 2 } else { 0 },
            "{boundary}: migration truth"
        );
        assert!(
            !root.join("serve-ready.json").exists(),
            "{boundary}: fabricated readiness"
        );
        if !migrated {
            assert_eq!(
                fs::read(&source).unwrap_or_else(|error| panic!("{error}")),
                source_before
            );
        }
        let reopened = handover(&root, &root.join("run"), "shv-crash", "secode-rust@crash");
        if migrated {
            assert!(
                !reopened.status.success(),
                "{boundary}: stale evidence fabricated recovery"
            );
            assert_eq!(
                fs::read(root.join("registration")).unwrap_or_else(|error| panic!("{error}")),
                observed
            );
            authorize_ready(&root);
            if committed {
                let domain = grapt_release::domain_fingerprint(&source)
                    .unwrap_or_else(|error| panic!("domain: {error}"));
                let recovery_cut = restart(&root, true);
                assert_eq!(
                    recovery_cut.status.code(),
                    Some(86),
                    "recovery hook was not reached"
                );
                assert_eq!(
                    fs::read(root.join("registration")).unwrap_or_else(|error| panic!("{error}")),
                    observed
                );
                assert_eq!(
                    grapt_release::domain_fingerprint(&source)
                        .unwrap_or_else(|error| panic!("{error}")),
                    domain
                );
            }
            let recovered = restart(&root, false);
            assert!(
                recovered.status.success(),
                "{boundary}: {}",
                String::from_utf8_lossy(&recovered.stdout)
            );
        } else {
            assert!(
                reopened.status.success(),
                "{boundary}: {}",
                String::from_utf8_lossy(&reopened.stdout)
            );
        }
        assert!(
            !root.join("registration").exists(),
            "{boundary}: leaked registration"
        );
        assert!(
            !root.join("serve-ready.json").exists(),
            "{boundary}: stale ready receipt"
        );
        println!(
            "SHV_CRASH boundary={boundary} migrated={migrated} committed={committed} reopened=true"
        );
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }
}

#[test]
fn missing_or_corrupt_recovery_checkpoints_never_authorize_handover() {
    for missing in [false, true] {
        let root = scratch(if missing {
            "missing-checkpoint"
        } else {
            "corrupt-checkpoint"
        });
        write_inputs(&root, "secode-rust@crash");
        let source = root.join("database.sqlite3");
        legacy_database(&source);
        register_incumbent(&root, ABSENT_PID);
        let original = fs::read(root.join("registration"))
            .unwrap_or_else(|error| panic!("registration: {error}"));
        let source_before = fs::read(&source).unwrap_or_else(|error| panic!("source: {error}"));
        grapt_release::rehearse(
            &source,
            &root.join("run"),
            grapt_types::MigrationId::new("shv-crash").unwrap_or_else(|| panic!("run id")),
            "secode-rust@crash",
            &grapt_store::SystemClock,
        )
        .unwrap_or_else(|error| panic!("rehearsal: {error}"));
        let mut paths = fs::read_dir(root.join("run/checkpoints"))
            .unwrap_or_else(|error| panic!("checkpoints: {error}"))
            .map(|entry| {
                entry
                    .unwrap_or_else(|error| panic!("checkpoint entry: {error}"))
                    .path()
            })
            .take(9)
            .collect::<Vec<_>>();
        paths.sort();
        assert_eq!(paths.len(), 8, "expected the complete checkpoint chain");
        let first = paths
            .first()
            .unwrap_or_else(|| panic!("missing checkpoint witness"));
        if missing {
            fs::remove_file(first).unwrap_or_else(|error| panic!("remove checkpoint: {error}"));
        } else {
            fs::write(first, b"corrupt checkpoint")
                .unwrap_or_else(|error| panic!("corrupt checkpoint: {error}"));
        }
        let output = handover(&root, &root.join("run"), "shv-crash", "secode-rust@crash");
        let result = receipt(&output);
        assert!(
            !output.status.success(),
            "invalid recovery evidence admitted: {result}"
        );
        assert!(
            result["reason"]
                .as_str()
                .is_some_and(|reason| reason.contains("checkpoint")),
            "{result}"
        );
        assert_eq!(
            fs::read(&source).unwrap_or_else(|error| panic!("source after: {error}")),
            source_before
        );
        assert_eq!(
            fs::read(root.join("registration"))
                .unwrap_or_else(|error| panic!("registration after: {error}")),
            original
        );
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }
}

#[test]
fn build_changed_during_retirement_refuses_before_first_write_contact() {
    let root = scratch("build-race");
    write_inputs(&root, "secode-rust@crash");
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    let before = fs::read(&source).unwrap_or_else(|error| panic!("source: {error}"));
    // The fixture holds the election lock across the delayed config rewrite
    // on every platform (a real serving process keeps the lock until it is
    // gone), so the retirement wait spans the rewrite. Without it the
    // incumbent is judged retired at once and the changed build is admitted.
    let incumbent_lock = ELECTION_LOCK_PRELUDE.replace(
        "{path}",
        "os.path.join(os.path.dirname(sys.argv[1]),'registration.elected')",
    );
    let mut incumbent = Held::spawn(
        "build-race incumbent",
        Command::new(PYTHON)
            .arg("-c").arg(format!("{incumbent_lock}print('incumbent-ready',flush=True)\n{AFTER_REHEARSAL}p.write_text(p.read_text(encoding='utf-8').replace('secode-rust@crash','secode-rust@changed'),encoding='utf-8')"))
            .arg(root.join("grapt.jsonc"))
            .arg(root.join("run/checkpoints/08-ready-for-handoff.json"))
            .stdout(std::process::Stdio::piped()),
    );
    // Spawn is not lock ownership: the successor must wait for the incumbent
    // to acknowledge its election lock before it can observe retirement.
    let ready = lifecycle::Pipe::watch(incumbent.stdout(), Some("incumbent-ready"));
    let (marker, _) = ready
        .await_marker(OBSERVATION_BUDGET)
        .unwrap_or_else(|state| panic!("{}", incumbent.failure(&state)));
    assert_eq!(marker, "incumbent-ready");
    register_incumbent(&root, incumbent.pid());
    let reaper = std::thread::spawn(move || {
        incumbent
            .await_exit(OBSERVATION_BUDGET)
            .map(|(status, _)| status)
    });
    let output = handover(
        &root,
        &root.join("run"),
        "shv-build-race",
        "secode-rust@crash",
    );
    assert!(reaper
        .join()
        .unwrap_or_else(|_| panic!("reaper panicked"))
        .unwrap_or_else(|error| panic!("reap: {error}"))
        .success());
    let result = receipt(&output);
    assert!(!output.status.success(), "changed build admitted: {result}");
    assert!(
        result["reason"].as_str().is_some_and(
            |reason| reason.contains("handover build changed before store preparation")
        ),
        "{result}"
    );
    assert_eq!(
        fs::read(&source).unwrap_or_else(|error| panic!("source after: {error}")),
        before
    );
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn post_commit_crash_restarts_without_discarding_postactivation_work() {
    let root = scratch("post-commit-crash");
    let build = "secode-rust@post-commit";
    write_inputs(&root, build);
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    let run_root = root.join("run");
    let rehearsal = grapt_release::rehearse(
        &source,
        &run_root,
        grapt_types::MigrationId::new("shv-post-commit").unwrap_or_else(|| panic!("run id")),
        build,
        &grapt_store::SystemClock,
    )
    .unwrap_or_else(|error| panic!("rehearsal: {error}"));
    let eligibility = rehearsal
        .lowered_eligibility()
        .unwrap_or_else(|error| panic!("eligibility: {error}"));
    let service_path = root.join("service.conf");
    let service =
        fs::read_to_string(&service_path).unwrap_or_else(|error| panic!("service: {error}"));
    fs::write(
        &service_path,
        service.replace("serve_window_ms=250", "serve_window_ms=1"),
    )
    .unwrap_or_else(|error| panic!("window: {error}"));
    // E2: a 1 ms window held at its end, never released before the crash cut,
    // so a clean exit cannot release the registration first.
    let mut child = Held::spawn(
        "post-commit-candidate",
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["handover", "--service"])
            .arg(&root)
            .arg("--run-root")
            .arg(&run_root)
            .args(["--run-id", "shv-post-commit", "--engine-version", build])
            .env("GRAPT_FIXTURE_HOLD_AT", "serve-window-end")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    );
    let ready = await_ready(&root.join("serve-ready.json"), OBSERVATION_BUDGET, || {
        child.exited()
    })
    .map(|ready| ready.elapsed.as_millis())
    .map_err(|state| child.failure(&state));
    println!("SHV_POST_COMMIT readiness_elapsed_ms={ready:?}");
    let write = if ready.is_ok() {
        Some(Command::new(PYTHON).args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.execute(\"UPDATE kv SET value='postactivation' WHERE key='theme'\"); c.commit(); c.close()"])
            .arg(&source).status().unwrap_or_else(|error| panic!("postactivation write: {error}")))
    } else {
        None
    };
    // The crash cut, held until here. A failed readiness was already cut.
    if ready.is_ok() {
        child
            .kill()
            .unwrap_or_else(|error| panic!("crash cut: {error}"));
    }
    assert!(
        ready.is_ok(),
        "RT3 candidate did not hold readiness to the crash cut: {ready:?}"
    );
    assert!(write.is_some_and(|status| status.success()));
    let observed = grapt_release::domain_fingerprint(&source)
        .unwrap_or_else(|error| panic!("observed domain: {error}"));
    let disposition = eligibility
        .recovery_disposition(&observed, grapt_release::path_identity(&source))
        .unwrap_or_else(|| panic!("unmeasured recovery disposition"));
    assert!(matches!(
        disposition,
        grapt_types::RecoveryDisposition::DiscardsPostActivationWork { .. }
    ));
    let registration = fs::read(root.join("registration"))
        .unwrap_or_else(|error| panic!("RT3 registration did not survive the crash cut: {error}"));
    let refused = handover(&root, &run_root, "shv-post-commit", build);
    assert!(
        !refused.status.success(),
        "stale migration evidence was reused: {}",
        String::from_utf8_lossy(&refused.stdout)
    );
    assert_eq!(
        fs::read(root.join("registration"))
            .unwrap_or_else(|error| panic!("registration after refusal: {error}")),
        registration
    );
    assert_eq!(
        grapt_release::domain_fingerprint(&source)
            .unwrap_or_else(|error| panic!("domain after refusal: {error}")),
        observed
    );
    let config_path = root.join("grapt.jsonc");
    let config = fs::read_to_string(&config_path).unwrap_or_else(|error| panic!("config: {error}"));
    fs::write(
        &config_path,
        config.replace("\"migrate\"", "\"open-ready\""),
    )
    .unwrap_or_else(|error| panic!("recovery authority: {error}"));
    fs::write(&service_path, service).unwrap_or_else(|error| panic!("recovery window: {error}"));
    let restarted =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["serve", "--service"])
            .arg(&root)
            .args(["--port", "0"])
            .output()
            .unwrap_or_else(|error| panic!("restart: {error}"));
    assert!(
        restarted.status.success(),
        "restart refused: {}",
        String::from_utf8_lossy(&restarted.stdout)
    );
    assert_eq!(
        grapt_release::domain_fingerprint(&source)
            .unwrap_or_else(|error| panic!("restarted domain: {error}")),
        observed
    );
    assert!(!root.join("registration").exists());
    assert!(!root.join("serve-ready.json").exists());
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}
