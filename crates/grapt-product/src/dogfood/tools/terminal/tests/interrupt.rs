//! Forced Ctrl-C: report the observed exit status, never an order or cause.
use super::*;

#[test]
fn a_forced_interrupt_is_reported_from_what_was_observed() {
    assert_eq!(interrupt_note(false, None), None);
    assert_eq!(
        interrupt_note(false, Some(&ShellTerminal::Exited(CONTROL_C_EXIT_CODE))),
        None
    );
    let requested = interrupt_note(true, None).unwrap_or_default();
    assert!(
        requested.contains("was requested") && !requested.contains("was terminated"),
        "{requested}"
    );
    let ctrl_c =
        interrupt_note(true, Some(&ShellTerminal::Exited(CONTROL_C_EXIT_CODE))).unwrap_or_default();
    assert!(
        ctrl_c.contains("exited with the Ctrl-C exit status"),
        "{ctrl_c}"
    );
    // An escalation kill, the shim or the command itself can produce any
    // status: the note states the observed status and claims no order or cause.
    for code in [0, 1] {
        let observed = interrupt_note(true, Some(&ShellTerminal::Exited(code))).unwrap_or_default();
        assert!(
            observed.ends_with(&format!("exited with status {code}")),
            "{observed}"
        );
        assert!(
            !observed.contains("first") && !observed.contains("own status"),
            "{observed}"
        );
    }
    let other = interrupt_note(true, Some(&ShellTerminal::TimedOut)).unwrap_or_default();
    assert!(other.contains("status reports"), "{other}");
}

#[tokio::test]
async fn a_forced_interrupt_reaches_the_result_and_is_absent_otherwise() -> Result<(), String> {
    let scratch = super::super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_forced_interrupt")?;
    let entry = registry.reserve(
        owner,
        operation("op_forced_interrupt")?,
        spec(&scratch.0, "printf ok", true)?,
    )?;
    {
        let mut data = lock(&entry.observation);
        data.exit = Some(ShellTerminal::Exited(CONTROL_C_EXIT_CODE));
        data.pipe = PipeState::Drained;
        data.cleanup = CleanupState::Complete;
    }
    let (plain, _) = observe(&entry, 0, 40_000, Some(0)).await?;
    let plain: serde_json::Value =
        serde_json::from_slice(&plain).map_err(|error| error.to_string())?;
    assert!(plain.get("interrupt").is_none(), "{plain}");
    lock(&entry.observation).forced_interrupt = true;
    let (forced, _) = observe(&entry, 0, 40_000, Some(0)).await?;
    let forced: serde_json::Value =
        serde_json::from_slice(&forced).map_err(|error| error.to_string())?;
    assert_eq!(
        forced["interrupt"],
        interrupt_note(true, Some(&ShellTerminal::Exited(CONTROL_C_EXIT_CODE))).unwrap_or_default()
    );
    assert_eq!(forced["exit_code"], CONTROL_C_EXIT_CODE);
    assert_eq!(forced["done"], true);
    Ok(())
}
