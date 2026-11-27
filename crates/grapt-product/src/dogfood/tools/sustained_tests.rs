//! SUSTAINED-WORK-REPAIR-01 J01: builtin tool contracts as accepted base
//! `20909c94` actually behaves. Reproduction receipts, replaced by the repair.

use super::*;

fn raw(value: &str) -> Result<BoundedToolInput, String> {
    BoundedToolInput::new(value.as_bytes().to_vec()).map_err(|error| error.to_string())
}

fn scratch(label: &str) -> Result<PathBuf, String> {
    let path = std::env::temp_dir().join(format!("grapt-swr-tools-{}-{label}", std::process::id()));
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
    std::fs::canonicalize(path).map_err(|error| error.to_string())
}

#[test]
fn swr_t3_builtin_definitions_carry_concise_semantic_descriptions() {
    for definition in definitions() {
        assert!(
            definition.description.as_ref().is_some_and(|description| {
                !description.trim().is_empty() && description.len() <= 512
            }),
            "{} has no bounded description",
            definition.name
        );
    }
}

#[test]
fn swr_t1_early_failure_detail_is_bounded_sanitized_and_durable() -> Result<(), String> {
    let root = scratch("prepare")?;
    let detail = prepare_effect(
        &root,
        READ_TOOL,
        // Control directories never offer the ordinary missing-read listing.
        &raw(r#"{"path":".git/missing.txt"}"#)?,
        &std::collections::BTreeMap::new(),
    )
    .err()
    .ok_or("a missing control-directory path was prepared")?;
    assert!(!detail.is_empty(), "preparation had no detail to discard");
    let artifacts = FilesystemArtifactStore::open(
        root.join("artifacts"),
        b"swr-early-detail".to_vec(),
        Box::new(SystemClock),
    )
    .map_err(|error| error.to_string())?;
    let terminal = captured_failure(
        &artifacts,
        ToolFailureReason::EffectFailed,
        &format!("{detail}\u{1b}[31m{}", "x".repeat(4_096)),
    );
    let ToolTerminalOutcome::Failed {
        reason,
        detail: Some(detail),
    } = terminal
    else {
        return Err("early failure carried no detail artifact".to_owned());
    };
    assert_eq!(reason, ToolFailureReason::EffectFailed);
    let detail_bound = u64::try_from(EARLY_FAILURE_DETAIL_BYTES)
        .map_err(|error| format!("detail bound does not fit u64: {error}"))?;
    assert!(detail.reference().length() <= detail_bound);
    let page = artifacts
        .fetch(
            detail.reference(),
            0,
            ReadBudget::new(detail_bound).ok_or("read budget")?,
        )
        .map_err(|error| error.to_string())?;
    let visible = std::str::from_utf8(page.bytes()).map_err(|error| error.to_string())?;
    assert!(visible.contains("missing.txt"));
    assert!(!visible.contains('\u{1b}'));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn swr_t2_a_bounded_read_marks_continuation_and_preserves_utf8() -> Result<(), String> {
    let root = scratch("read")?;
    let text: String = (1..=3_000).map(|number| format!("é{number}\n")).collect();
    std::fs::write(root.join("large.txt"), text).map_err(|e| e.to_string())?;
    let whole = raw(r#"{"path":"large.txt"}"#)?;
    let EffectTarget::Read(expected) =
        prepare_effect(&root, READ_TOOL, &whole, &std::collections::BTreeMap::new())?.target
    else {
        return Err("read preparation changed effect class".to_owned());
    };
    let output = run_read(&root, &expected, &whole)?;
    let output = String::from_utf8(output).map_err(|error| error.to_string())?;
    assert!(output.starts_with("1: é1\n2: é2\n"));
    assert!(
        output.contains("\n2000: é2000\n[Output truncated. Continue reading with offset: 2001]")
    );
    assert!(!output.contains("2001: é2001"));
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn swr_t2_bounded_shell_output_names_kept_and_observed_bytes() -> Result<(), String> {
    let observed = 600 * 1_024;
    let output = render_shell_output(
        BoundedStream {
            bytes: vec![b'0'; SHELL_STREAM_MAX_BYTES],
            observed,
        },
        BoundedStream {
            bytes: vec![b'1'; SHELL_STREAM_MAX_BYTES],
            observed,
        },
    )?;
    let output = String::from_utf8(output).map_err(|error| error.to_string())?;
    assert_eq!(
        output.len(),
        usize::try_from(grapt_runtime::TOOL_ARTIFACT_MAX_BYTES)
            .map_err(|_| "artifact bound exceeds usize")?
    );
    assert!(output.starts_with(&"0".repeat(SHELL_STREAM_MAX_BYTES - 128)));
    assert!(output.contains("[stdout output bounded: kept "));
    assert!(output.contains("[stderr output bounded: kept "));
    assert_eq!(output.matches("of 614400 observed bytes]").count(), 2);
    Ok(())
}
