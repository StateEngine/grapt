use super::*;

#[test]
fn paths_frontmatter_matches_public_reads_and_refuses_ambiguous_aliases(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new("paths-rules")?;
    let root = &scratch.0;
    std::fs::create_dir_all(root.join(".grapt/rules"))?;
    std::fs::create_dir_all(root.join("src"))?;
    std::fs::write(root.join("src/file.rs"), "// source\n")?;
    std::fs::write(root.join("src/file.txt"), "plain text\n")?;
    std::fs::write(root.join("shared.md"), "imported paths policy")?;
    std::fs::write(
        root.join(".grapt/rules/other.md"),
        "---\npaths: '**/*.py'\n---\n@missing.md\n",
    )?;
    let rule = root.join(".grapt/rules/policy.md");
    for metadata in ["paths: 'src/**/*.rs'", "paths:\n  - 'src/**/*.rs'"] {
        std::fs::write(
            &rule,
            format!("---\n{metadata}\n---\nmatching paths policy\n@../../shared.md\n"),
        )?;
        let matched = call(
            root,
            READ_LINES_TOOL,
            serde_json::json!({"path":"src/file.rs"}),
        )?;
        let rows = matched["scopedInstructions"]
            .as_array()
            .ok_or("scoped instructions absent")?;
        assert!(rows.iter().any(|row| row["content"]
            .as_str()
            .is_some_and(|text| text.contains("matching paths policy")
                && text.contains("imported paths policy"))));
        assert!(rows.iter().all(|row| row.get("unavailable").is_none()));
        let other = call(
            root,
            READ_LINES_TOOL,
            serde_json::json!({"path":"src/file.txt"}),
        )?;
        assert!(!other.to_string().contains("paths policy"));
        assert!(!other.to_string().contains("unavailable"));
    }
    for (metadata, reason) in [
        ("paths: '*.rs'\nglobs: '*.rs'", "both paths and globs"),
        ("paths:\nglobs: '*.rs'", "both paths and globs"),
        ("paths: '*.rs'\nglobs:", "both paths and globs"),
        ("paths: []", "1 to 64 patterns"),
        ("paths: [12]", "strings"),
        ("paths: '../*.rs'", "relative"),
        ("paths: '[broken'", "malformed"),
    ] {
        std::fs::write(&rule, format!("---\n{metadata}\n---\nrefused policy\n"))?;
        let refused = call(
            root,
            READ_LINES_TOOL,
            serde_json::json!({"path":"src/file.rs"}),
        )?;
        assert!(refused["scopedInstructions"]
            .as_array()
            .ok_or("scoped refusal absent")?
            .iter()
            .any(|row| row["unavailable"]
                .as_str()
                .is_some_and(|error| error.contains(reason))));
        assert!(!refused.to_string().contains("refused policy"));
    }
    Ok(())
}

#[test]
fn file_reads_attach_only_matching_rules_in_stable_scope_order(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new("scoped-rules")?;
    let root = &scratch.0;
    std::fs::create_dir_all(root.join(".grapt/rules"))?;
    std::fs::create_dir_all(root.join("src/.grapt/rules"))?;
    std::fs::write(root.join("src/file.rs"), "// source\n")?;
    std::fs::write(root.join("src/file.txt"), "plain text\n")?;
    std::fs::write(root.join("shared.md"), "scoped imported policy")?;
    std::fs::write(root.join("src/AGENTS.md"), "@../shared.md\n")?;
    for (name, body) in [("b.md", "root second"), ("a.md", "root first")] {
        std::fs::write(
            root.join(".grapt/rules").join(name),
            format!("---\nglobs: ['src/**/*.rs']\n---\n{body}\n"),
        )?;
    }
    std::fs::write(
        root.join("src/.grapt/rules/near.md"),
        "---\nglobs: '*.rs'\n---\nnear policy\n",
    )?;
    // A nonmatching rule must not open its imports, even for another rule's read.
    std::fs::write(
        root.join(".grapt/rules/other.md"),
        "---\nglobs: '**/*.py'\n---\n@missing.md\n",
    )?;
    let read = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"src/file.rs"}),
    )?;
    let instructions = read["scopedInstructions"]
        .as_array()
        .ok_or("scoped instructions absent")?;
    let bodies: Vec<&str> = instructions
        .iter()
        .filter_map(|row| row["content"].as_str())
        .collect();
    assert!(bodies
        .iter()
        .any(|body| body.contains("scoped imported policy")));
    let position = |policy| {
        bodies
            .iter()
            .position(|body| body.contains(policy))
            .ok_or("policy absent")
    };
    assert!(position("near policy")? < position("root first")?);
    assert!(position("root first")? < position("root second")?);
    assert!(instructions
        .iter()
        .all(|row| row.get("unavailable").is_none()));
    let other = call(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"src/file.txt"}),
    )?;
    assert!(!other.to_string().contains("near policy"));
    assert!(!other.to_string().contains("root first"));
    let compact = String::from_utf8(call_bytes(
        root,
        READ_LINES_TOOL,
        serde_json::json!({"path":"src/file.rs"}),
    )?)?;
    assert!(compact.contains("near policy"));
    let read = input(serde_json::json!({"path":"src/file.rs"}))?;
    let legacy = String::from_utf8(run_read(root, &root.join("src/file.rs"), &read)?)?;
    assert!(legacy.contains("root first") && legacy.contains("scoped imported policy"));
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn instruction_acquisition_substitutions_cannot_redirect_or_block_file_reads(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::dogfood::instructions::source::testing::{install, Phase};
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    #[cfg(unix)]
    let cases = ["ancestor", "fifo", "replace", "grow"];
    #[cfg(windows)]
    let cases = ["pinned", "grow"];
    for kind in ["import", "rule"] {
        for case in cases {
            let scratch = Scratch::new(&format!("instruction-{kind}-{case}"))?;
            let outside = Scratch::new(&format!("instruction-outside-{kind}-{case}"))?;
            let root = &scratch.0;
            let parent = root.join(if kind == "import" {
                "src/policy"
            } else {
                "src/.grapt/rules"
            });
            std::fs::create_dir_all(&parent)?;
            std::fs::write(root.join("src/file.rs"), "// source\n")?;
            if kind == "import" {
                std::fs::write(root.join("src/AGENTS.md"), "@policy/policy.md\n")?;
            }
            let target = parent.join("policy.md");
            std::fs::write(&target, "---\nglobs: '*.rs'\n---\ninside policy\n")?;
            std::fs::write(
                outside.0.join("policy.md"),
                "---\nglobs: '*.rs'\n---\nOUTSIDE-SECRET\n",
            )?;
            // A broken blocking open must fail this test, not hang the suite.
            #[cfg(unix)]
            let watchdog = if case == "fifo" {
                use std::os::unix::fs::OpenOptionsExt as _;
                use std::sync::mpsc::{sync_channel, RecvTimeoutError};
                let (stop, waiting) = sync_channel::<()>(1);
                let target = target.clone();
                Some((
                    stop,
                    std::thread::spawn(move || {
                        matches!(
                            waiting.recv_timeout(std::time::Duration::from_secs(2)),
                            Err(RecvTimeoutError::Timeout)
                        ) && std::fs::OpenOptions::new()
                            .write(true)
                            .custom_flags(crate::dogfood::source_file::NONBLOCK)
                            .open(target)
                            .is_ok()
                    }),
                ))
            } else {
                None
            };
            let reached = Arc::new(AtomicBool::new(false));
            let observed = Arc::clone(&reached);
            let selected = target.clone();
            let moved = root.join("moved");
            #[cfg(unix)]
            let outside = outside.0.clone();
            let _observer = install(Box::new(move |phase, path| {
                if path != selected.as_path() {
                    return;
                }
                observed.store(true, Ordering::Relaxed);
                #[cfg(unix)]
                match (case, phase) {
                    ("ancestor", Phase::Opening) => {
                        std::fs::rename(&parent, &moved).unwrap_or_else(|e| panic!("{e}"));
                        std::os::unix::fs::symlink(&outside, &parent)
                            .unwrap_or_else(|e| panic!("{e}"));
                    }
                    ("ancestor", Phase::Opened) => {
                        std::fs::remove_file(&parent).unwrap_or_else(|e| panic!("{e}"));
                        std::fs::rename(&moved, &parent).unwrap_or_else(|e| panic!("{e}"));
                    }
                    ("fifo", Phase::Opening) => {
                        std::fs::remove_file(&selected).unwrap_or_else(|e| panic!("{e}"));
                        assert!(std::process::Command::new("mkfifo")
                            .arg(&selected)
                            .status()
                            .unwrap_or_else(|e| panic!("{e}"))
                            .success());
                    }
                    ("replace", Phase::Reading) => {
                        std::fs::remove_file(&selected).unwrap_or_else(|e| panic!("{e}"));
                        std::os::unix::fs::symlink(outside.join("policy.md"), &selected)
                            .unwrap_or_else(|e| panic!("{e}"));
                    }
                    _ => {}
                }
                #[cfg(windows)]
                if case == "pinned" {
                    match phase {
                        Phase::Opened => assert!(std::fs::rename(&parent, &moved).is_err()),
                        Phase::Reading => assert!(std::fs::remove_file(&selected).is_err()),
                        Phase::Opening => {}
                    }
                }
                if case == "grow" && phase == Phase::Reading {
                    std::fs::OpenOptions::new()
                        .append(true)
                        .open(&selected)
                        .and_then(|mut file| file.write_all(&vec![b'x'; 65_537]))
                        .unwrap_or_else(|e| panic!("{e}"));
                }
            }));
            let result = call(
                root,
                READ_LINES_TOOL,
                serde_json::json!({"path":"src/file.rs"}),
            );
            #[cfg(unix)]
            if let Some((stop, watchdog)) = watchdog {
                let _ = stop.send(());
                assert!(
                    !watchdog.join().map_err(|_| "FIFO watchdog panicked")?,
                    "instruction acquisition waited for a FIFO writer"
                );
            }
            assert!(
                reached.load(Ordering::Relaxed),
                "{kind}/{case}: acquisition was not exercised"
            );
            let result = result?;
            assert!(!result.to_string().contains("OUTSIDE-SECRET"));
            if matches!(case, "ancestor" | "fifo" | "grow") {
                assert!(result["scopedInstructions"]
                    .as_array()
                    .ok_or("no instructions")?
                    .iter()
                    .any(|row| row.get("unavailable").is_some()));
            } else {
                assert!(result.to_string().contains("inside policy"));
            }
        }
    }
    Ok(())
}
