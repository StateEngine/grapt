use super::*;

#[tokio::test]
async fn denied_process_creation_never_reports_command_effects_or_retains_capacity(
) -> Result<(), String> {
    let scratch = super::super::super::tests::Scratch::new()?;
    let registry = TerminalRegistry::default();
    let owner = session("ses_denied_spawn")?;
    let operation = operation("op_denied_spawn")?;
    let mut request = spec(&scratch.0, "exit 0", false)?;
    // CreateProcessW refuses a directory with ERROR_ACCESS_DENIED. It never
    // creates or resumes a command, just as a rejected executable cannot run.
    request.program = scratch.0.clone();
    let failure = match registry.exec(&owner, operation.clone(), request).await {
        Err(failure) => failure,
        Ok(_) => return Err("a directory was accepted as a terminal executable".into()),
    };
    assert_eq!(
        failure.reason,
        ToolFailureReason::EffectFailed,
        "{}",
        failure.detail
    );
    assert!(
        failure.detail.contains("did not start"),
        "{}",
        failure.detail
    );
    assert!(registry.lookup(&owner, &operation)?.evictable());

    // Independent simultaneous launch scopes must retain their own first OS
    // failure, and the next launch on each worker must start with no failure.
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let request = spec(&scratch.0, "exit 0", false);
            let directory = scratch.0.clone();
            std::thread::spawn(move || -> Result<(), String> {
                let mut request = request?;
                request.program = directory;
                let failure = match process::spawn(&request, Some(1_000)) {
                    Err(failure) => failure,
                    Ok(_) => return Err("directory unexpectedly launched".into()),
                };
                let diagnostic = failure
                    .diagnostic
                    .ok_or("native failure had no diagnostic")?;
                assert_eq!(diagnostic["call"], "CreateProcessW");
                assert_eq!(diagnostic["win32_error"], 5);
                assert_eq!(diagnostic["mode"], "pipes");
                assert_eq!(diagnostic["launch"]["suspended"], true);
                assert_eq!(diagnostic["launch"]["jobs"], 1);
                assert_eq!(
                    diagnostic["executable_observation"]["replaced_during_spawn"],
                    false
                );
                assert_eq!(diagnostic["cwd_observation"]["written_during_spawn"], false);
                let capture = ::windows_spawn::diagnostics::Capture::new();
                assert_eq!(capture.failure(), None);
                Ok(())
            })
        })
        .collect();
    for worker in workers {
        worker
            .join()
            .map_err(|_| "native diagnostic worker panicked")??;
    }
    Ok(())
}
