use super::*;

#[tokio::test]
async fn exiting_root_cleans_its_descendant_and_preserves_status() -> Result<(), String> {
    for tty in [false, true] {
        let scratch = super::super::super::tests::Scratch::new()?;
        let registry = TerminalRegistry::default();
        let owner = session("ses_macos_cleanup")?;
        let id = operation("op_macos_cleanup")?;
        let source = "/bin/sleep 30 & printf '%s\\n' \"$!\" > descendant.pid; printf ready; while [ ! -f finish ]; do /bin/sleep 0.01; done; exit 23";
        let (_, delivery) = registry
            .exec(&owner, id.clone(), spec(&scratch.0, source, tty)?)
            .await
            .map_err(failure)?;
        delivery.captured();
        let entry = registry.lookup(&owner, &id)?;
        let result = async {
            await_output(&entry, "ready").await?;
            let pid = std::fs::read_to_string(scratch.0.join("descendant.pid"))
                .map_err(|error| error.to_string())?
                .trim()
                .parse::<i32>()
                .map_err(|error| error.to_string())?;
            if pid <= 0 {
                return Err("fixture descendant PID is not positive".into());
            }
            let pid = nix::unistd::Pid::from_raw(pid);
            nix::sys::signal::kill(pid, None)
                .map_err(|error| format!("fixture descendant was not alive: {error}"))?;
            std::fs::write(scratch.0.join("finish"), "finish")
                .map_err(|error| error.to_string())?;
            let report = settled_report(&entry).await?;
            if report["exit_code"] != 23
                || report["cleanup"] != "complete"
                || report.get("cleanup_error").is_some()
            {
                return Err(format!(
                    "native Mac terminal did not preserve exit and cleanup: {report}"
                ));
            }
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    match nix::sys::signal::kill(pid, None) {
                        Err(nix::errno::Errno::ESRCH) => return Ok(()),
                        Ok(()) => (),
                        Err(error) => {
                            return Err(format!("descendant observation failed: {error}"))
                        }
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .map_err(|_| "native Mac terminal left its descendant alive".to_owned())?
        }
        .await;
        entry.cancel();
        result?;
    }
    Ok(())
}
