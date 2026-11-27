//! Test-owned lifecycle seams at existing boundaries, compiled only under the
//! `handover-crash-fixture` feature beside `handover::crash_boundary`.
//!
//! A hold blocks at a serve boundary until EOF on the process's own stdin, then
//! continues down the unchanged path. The pipe is created at spawn and belongs to
//! this process alone, so the owning test releases it by dropping its end, and
//! the kernel releases it if that test dies. An announce writes one line to this
//! process's stderr from the decoded registration. Neither is a shutdown
//! protocol: an ordinary build has no selector, no stdin read and no line.

#[derive(Clone, Copy)]
pub(crate) enum HoldPoint {
    /// Before readiness is published; used only by the harness budget witness.
    ServeBeforeReady,
    /// After the configured serve window has fully elapsed, before the drain.
    ServeWindowEnd,
    /// After the handover has read and announced the registration, before it
    /// waits for retirement; used only by the order witness.
    BeforeRetirement,
    #[cfg(feature = "handover-crash-fixture")]
    ExecutionInterrupted,
}

/// Block until the test releases this process, when selected. EOF releases.
/// Input or a read error is never a release: each exits with its own code.
pub(crate) async fn hold(point: HoldPoint) {
    #[cfg(feature = "handover-crash-fixture")]
    {
        let name = match point {
            HoldPoint::ServeBeforeReady => "serve-before-ready",
            HoldPoint::ServeWindowEnd => "serve-window-end",
            HoldPoint::BeforeRetirement => "before-retirement",
            HoldPoint::ExecutionInterrupted => "execution-interrupted",
        };
        if std::env::var_os("GRAPT_FIXTURE_HOLD_AT").as_deref() == Some(std::ffi::OsStr::new(name))
        {
            if matches!(point, HoldPoint::ExecutionInterrupted) {
                use std::io::Write;
                if std::io::stderr()
                    .write_all(b"secode-fixture boundary=execution-interrupted\n")
                    .is_err()
                {
                    std::process::exit(87);
                }
            }
            match tokio::task::spawn_blocking(await_stdin_eof).await {
                Ok(StdinEnd::Eof) => {}
                Ok(StdinEnd::Input) => std::process::exit(89),
                Ok(StdinEnd::ReadError) | Err(_) => std::process::exit(88),
            }
        }
    }
    #[cfg(not(feature = "handover-crash-fixture"))]
    let _ = point;
}

/// Announce the `before-retirement` boundary with the incumbent the handover
/// decoded from the registration. A failed write exits 87 rather than letting
/// the test guess whether the boundary was reached.
pub(crate) fn announce_before_retirement(incumbent_pid: Option<u32>) {
    #[cfg(feature = "handover-crash-fixture")]
    {
        use std::io::Write;
        if std::env::var_os("GRAPT_FIXTURE_ANNOUNCE_AT").as_deref()
            == Some(std::ffi::OsStr::new("before-retirement"))
        {
            let pid = incumbent_pid.map_or_else(|| "none".to_owned(), |pid| pid.to_string());
            let line = format!("secode-fixture boundary=before-retirement incumbent_pid={pid}\n");
            let mut stderr = std::io::stderr().lock();
            if stderr
                .write_all(line.as_bytes())
                .and_then(|()| stderr.flush())
                .is_err()
            {
                std::process::exit(87);
            }
        }
    }
    #[cfg(not(feature = "handover-crash-fixture"))]
    let _ = incumbent_pid;
}

#[cfg(feature = "handover-crash-fixture")]
enum StdinEnd {
    Eof,
    Input,
    ReadError,
}

/// One byte is enough to tell EOF from input; nothing is buffered or retained.
#[cfg(feature = "handover-crash-fixture")]
fn await_stdin_eof() -> StdinEnd {
    use std::io::Read;
    let mut byte = [0_u8; 1];
    let mut stdin = std::io::stdin().lock();
    loop {
        match stdin.read(&mut byte) {
            Ok(0) => return StdinEnd::Eof,
            Ok(_) => return StdinEnd::Input,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return StdinEnd::ReadError,
        }
    }
}
