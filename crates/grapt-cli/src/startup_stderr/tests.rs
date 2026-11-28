//! A new profile's startup stderr reaches a failed launch's refusal, and a
//! ready service's server log.
use super::*;
use tokio::io::AsyncWriteExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[tokio::test]
async fn a_failed_child_names_its_own_stderr() -> TestResult {
    #[cfg(unix)]
    let mut command = {
        let mut command = tokio::process::Command::new("sh");
        command.args(["-c", "echo profile setup failed: disk full >&2; exit 3"]);
        command
    };
    #[cfg(windows)]
    let mut command = {
        let mut command = tokio::process::Command::new("cmd");
        command.args([
            "/C",
            "echo profile setup failed: disk full 1>&2 & exit /b 3",
        ]);
        command
    };
    let mut child = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()?;
    let startup = StartupStderr::capture(child.stderr.take().ok_or("no stderr pipe")?);
    child.wait().await?;
    let refusal = startup
        .refusal("embedded child exited before readiness".into())
        .await;
    assert!(
        refusal.starts_with("embedded child exited before readiness\nservice stderr:\n"),
        "{refusal}"
    );
    assert!(
        refusal.contains("profile setup failed: disk full"),
        "{refusal}"
    );
    Ok(())
}

#[tokio::test]
async fn a_long_startup_keeps_its_latest_output_and_says_what_it_dropped() -> TestResult {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let startup = StartupStderr::capture(reader);
    writer.write_all(&vec![b'x'; TAIL_BYTES]).await?;
    writer.write_all(b"\nthe last line").await?;
    drop(writer);
    let refusal = startup.refusal("refused".into()).await;
    assert!(refusal.contains("after 14 earlier bytes"), "{refusal}");
    assert!(refusal.ends_with("the last line"), "{refusal}");
    Ok(())
}

#[tokio::test]
async fn a_ready_service_keeps_startup_and_later_output_in_its_log() -> TestResult {
    let profile = crate::testfs::ScratchDir::new("startup-stderr")?;
    let (mut writer, reader) = tokio::io::duplex(1024);
    let startup = StartupStderr::capture(reader);
    writer.write_all(b"starting\n").await?;
    // The reader takes the startup line before the hand-over.
    while matches!(&*startup.sink.lock().unwrap_or_else(PoisonError::into_inner), Sink::Tail { kept, .. } if kept.is_empty())
    {
        tokio::task::yield_now().await;
    }
    startup.into_log(&profile);
    writer.write_all(b"serving\n").await?;
    drop(writer);
    let log = crate::server_log::path(&profile);
    for _ in 0..1000 {
        if std::fs::read_to_string(&log)? == "starting\nserving\n" {
            return Ok(());
        }
        tokio::task::yield_now().await;
    }
    Err(format!("server log holds {:?}", std::fs::read_to_string(&log)?).into())
}
