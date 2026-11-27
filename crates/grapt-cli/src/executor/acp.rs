//! ACP connection shutdown owns and reaps only its embedded product.

use std::io::{self, IsTerminal};

use crate::command::ConnectionChoice;
use crate::{serve_acp, CliLimits, ExitClass, OutputMode};

use super::{bound_reap, connect, diagnostic_exit, output_sink, residue_message};

pub(super) async fn execute(connection: ConnectionChoice, limits: CliLimits) -> u8 {
    let mut sink = output_sink(OutputMode::Jsonl, limits);
    if io::stdin().is_terminal() {
        return diagnostic_exit(
            &mut sink,
            ExitClass::Usage,
            &"acp requires non-terminal JSONL stdin",
        );
    }
    let owner = match connect(connection, limits).await {
        Ok(client) => client,
        Err((class, message)) => return diagnostic_exit(&mut sink, class, &message),
    };
    let result = serve_acp(
        owner.without_child_ownership(),
        tokio::io::stdin(),
        tokio::io::stdout(),
        super::lossy_stderr::LossyStderr::stderr(),
        limits,
    )
    .await;
    let cleanup = match owner.take_embedded_child().await {
        Some(child) => close_child(child).await,
        None => Ok(()),
    };
    match (result, cleanup) {
        (Ok(()), Ok(())) => ExitClass::Success.code(),
        (Err(error), Ok(())) => diagnostic_exit(&mut sink, ExitClass::Protocol, &error),
        (Ok(()), Err(error)) => diagnostic_exit(&mut sink, ExitClass::Child, &error),
        (Err(error), Err(cleanup)) => diagnostic_exit(
            &mut sink,
            ExitClass::Protocol,
            &format!("{error}; embedded product cleanup failed: {cleanup}"),
        ),
    }
}

/// EOF asks the existing product lifetime owner to drain normally. If it
/// cannot finish within the cleanup bound, kill and reap this exact child.
pub(super) async fn close_child(mut child: tokio::process::Child) -> Result<(), String> {
    let pid = child.id();
    drop(child.stdin.take());
    match bound_reap(None, child.wait()).await {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => Err(format!("embedded product exited during cleanup: {status}")),
        Err((detail, _)) => {
            let kill_error = child.start_kill().err();
            match bound_reap(kill_error, child.wait()).await {
                Ok(_) => Err(format!(
                    "embedded product required forced cleanup: {}",
                    residue_message(pid, &detail, None)
                )),
                Err((reap_detail, kill_error)) => Err(format!(
                    "{}; {}",
                    residue_message(pid, &detail, None),
                    residue_message(pid, &reap_detail, kill_error.as_ref())
                )),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::close_child;
    use std::process::Stdio;
    #[cfg(unix)]
    use std::time::Duration;

    #[tokio::test]
    async fn graceful_cleanup_reaps_the_child_and_preserves_a_failed_exit() -> Result<(), String> {
        for code in [0, 7] {
            let (program, args) = if cfg!(windows) {
                ("cmd", vec!["/c".to_owned(), format!("exit {code}")])
            } else {
                ("sh", vec!["-c".to_owned(), format!("exit {code}")])
            };
            let child = tokio::process::Command::new(program)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| error.to_string())?;
            let result = close_child(child).await;
            if code == 0 {
                result?;
            } else {
                let error = result
                    .err()
                    .ok_or("a failed product exit was reported as clean")?;
                assert!(
                    error.contains("embedded product exited during cleanup"),
                    "{error}"
                );
                assert!(error.contains('7'), "failed exit status was lost: {error}");
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_child_ignoring_control_eof_is_killed_within_the_cleanup_bounds() -> Result<(), String>
    {
        let child = tokio::process::Command::new("sh")
            .args(["-c", "exec sleep 60"])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let result = tokio::time::timeout(Duration::from_secs(25), close_child(child))
            .await
            .map_err(|_| "a child ignoring EOF exceeded both cleanup bounds")?;
        let error = result
            .err()
            .ok_or("forced child cleanup was reported as graceful")?;
        assert!(error.contains("required forced cleanup"), "{error}");
        assert!(error.contains("10-second cleanup bound"), "{error}");
        Ok(())
    }
}
