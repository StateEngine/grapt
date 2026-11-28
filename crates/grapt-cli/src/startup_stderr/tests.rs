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
    let forward = startup.into_log(&profile);
    writer.write_all(b"serving\n").await?;
    // The child exits; shutdown finishes the forward before this command does.
    drop(writer);
    forward.finish(Duration::from_secs(5)).await;
    let log = std::fs::read_to_string(crate::server_log::path(&profile))?;
    assert_eq!(log, "starting\nserving\n");
    Ok(())
}

#[tokio::test]
async fn a_blank_tail_still_reports_the_output_it_dropped() -> TestResult {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let startup = StartupStderr::capture(reader);
    writer.write_all(b"the cause\n").await?;
    writer.write_all(&vec![b' '; TAIL_BYTES]).await?;
    drop(writer);
    let refusal = startup.refusal("refused".into()).await;
    assert_eq!(
        refusal,
        format!(
            "refused\nservice stderr: 10 earlier bytes were not kept, and the last {TAIL_BYTES} are blank"
        )
    );
    Ok(())
}

#[test]
fn a_stalled_log_writer_never_holds_the_reader() -> TestResult {
    let (chunks, mut pending) = channel(1);
    let lost = Arc::new(AtomicU64::new(0));
    let mut sink = Sink::Log {
        chunks,
        lost: Arc::clone(&lost),
    };
    sink.accept(b"first");
    // The writer takes nothing: the next chunk is counted, not waited on.
    sink.accept(b"second");
    assert_eq!(lost.load(Ordering::Relaxed), 6);
    assert_eq!(pending.try_recv()?, (None, b"first".to_vec()));
    sink.accept(b"third");
    assert_eq!(
        pending.try_recv()?,
        (Some(not_written(6)), b"third".to_vec())
    );
    Ok(())
}

#[test]
fn output_dropped_just_before_the_pipe_closes_is_noted_in_the_log() -> TestResult {
    let profile = crate::testfs::ScratchDir::new("startup-stderr-eof")?;
    let (chunks, mut pending) = channel(1);
    let lost = Arc::new(AtomicU64::new(0));
    let mut sink = Sink::Log {
        chunks,
        lost: Arc::clone(&lost),
    };
    sink.accept(b"first\n");
    sink.accept(b"lost at the end");
    // The pipe closes with that overflow still unwritten.
    drop(sink);
    write_log(crate::server_log::open(&profile)?, &mut pending, &lost);
    assert_eq!(
        std::fs::read_to_string(crate::server_log::path(&profile))?,
        format!("first\n{}", not_written(15))
    );
    Ok(())
}
