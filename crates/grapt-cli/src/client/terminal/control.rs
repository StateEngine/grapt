//! Holding the private pipe separates observing exit from requesting exit.

use tokio::process::{Child, ChildStdin};

pub(super) fn retain(child: &mut Child) -> Result<ChildStdin, &'static str> {
    child
        .stdin
        .take()
        .ok_or("owned product control pipe is absent")
}

/// Cancellation drops this future and releases the private pipe. While the
/// observer is pending, waiting for exit must retain it instead of sending EOF.
pub(super) async fn observe(
    child: &mut Child,
    control: ChildStdin,
) -> std::io::Result<std::process::ExitStatus> {
    let status = child.wait().await;
    drop(control);
    status
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::time::Duration;
    use tokio::io::AsyncReadExt;
    use tokio::process::Command;

    #[test]
    #[ignore = "private child process fixture; only the custody witness invokes it"]
    fn owned_product_control_child_fixture() -> Result<(), String> {
        if std::env::var("GRAPT_FRC_CONTROL_CHILD").as_deref() != Ok("read-eof") {
            return Err("private control fixture authority is absent".into());
        }
        println!("FRC_CONTROL_READY");
        std::io::stdout()
            .flush()
            .map_err(|error| error.to_string())?;
        let count = std::io::stdin()
            .lock()
            .read(&mut [0; 1])
            .map_err(|error| error.to_string())?;
        assert_eq!(count, 0, "private control fixture expected only EOF");
        println!("FRC_CONTROL_EOF_SETTLED");
        Ok(())
    }

    #[tokio::test]
    async fn frc_product_control_survives_exit_observation_until_coding_ends() -> Result<(), String>
    {
        let executable = std::env::current_exe().map_err(|error| error.to_string())?;
        let mut child = Command::new(executable)
            .args([
                "--exact",
                "client::terminal::control::tests::owned_product_control_child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("GRAPT_FRC_CONTROL_CHILD", "read-eof")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| error.to_string())?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or("control child output is absent")?;
        let mut readiness = Vec::new();
        tokio::time::timeout(Duration::from_secs(5), async {
            for _ in 0..2_048 {
                let mut chunk = [0; 128];
                let count = stdout
                    .read(&mut chunk)
                    .await
                    .map_err(|error| error.to_string())?;
                if readiness.len() + count > 2_048 {
                    return Err("private control readiness exceeded its byte bound".into());
                }
                readiness.extend_from_slice(&chunk[..count]);
                if readiness
                    .windows(17)
                    .any(|bytes| bytes == b"FRC_CONTROL_READY")
                {
                    return Ok::<(), String>(());
                }
                if count == 0 {
                    break;
                }
            }
            Err("private control readiness is absent or exceeds its bound".into())
        })
        .await
        .map_err(|_| "private control readiness timed out")??;
        let control = retain(&mut child)?;
        let early =
            tokio::time::timeout(Duration::from_secs(2), observe(&mut child, control)).await;
        let status = tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .map_err(|_| "control child did not settle after EOF")?
            .map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        tokio::time::timeout(
            Duration::from_secs(1),
            stdout.take(2_049).read_to_end(&mut bytes),
        )
        .await
        .map_err(|_| "control child evidence did not settle")?
        .map_err(|error| error.to_string())?;
        assert!(
            early.is_err(),
            "exit observation closed the owned product control pipe"
        );
        assert!(status.success(), "private control child failed");
        assert!(
            bytes.len() <= 2_048
                && bytes
                    .windows(23)
                    .any(|window| window == b"FRC_CONTROL_EOF_SETTLED"),
            "private control EOF evidence is absent"
        );
        Ok(())
    }
}
