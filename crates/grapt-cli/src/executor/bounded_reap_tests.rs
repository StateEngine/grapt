use super::{bound_reap, residue_message};
use std::future::Future;
use std::io;
use std::process::ExitStatus;
use std::time::Duration;

const BOUND_TEXT: &str = "it was not reaped within its 10-second cleanup bound";

/// The kernel's own EPERM value, as a refused kill delivers it.
fn refused_kill() -> io::Error {
    io::Error::from_raw_os_error(1)
}

fn pending_wait() -> impl Future<Output = io::Result<ExitStatus>> {
    std::future::pending()
}

/// A wait that would complete long after the bound, so a missing timeout
/// fails finitely instead of hanging.
async fn late_reap_wait() -> io::Result<ExitStatus> {
    tokio::time::sleep(Duration::from_secs(3600)).await;
    Err(io::Error::other("the late reap completed past the bound"))
}

/// A real `ExitStatus` (exit 3) from a short-lived child, waited within a bound.
async fn real_exit_status() -> ExitStatus {
    let (program, args) = if cfg!(windows) {
        ("cmd", ["/c", "exit 3"])
    } else {
        ("sh", ["-c", "exit 3"])
    };
    let mut child = match tokio::process::Command::new(program)
        .args(args)
        .kill_on_drop(true)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => panic!("the status child would not spawn: {error}"),
    };
    match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) if status.code() == Some(3) => status,
        Ok(Ok(status)) => panic!("the status child exited {status}, not 3"),
        Ok(Err(error)) => panic!("the status child's wait failed: {error}"),
        Err(_) => panic!("the status child did not exit within 5 s"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_pending_reap_ends_at_exactly_the_bound() {
    let started = tokio::time::Instant::now();
    let Err((detail, kill_error)) = bound_reap(None, pending_wait()).await else {
        panic!("a pending reap cannot succeed");
    };
    assert!(kill_error.is_none());
    assert_eq!(detail, BOUND_TEXT);
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(10)
    );
}

#[tokio::test(start_paused = true)]
async fn a_late_reap_is_cut_at_the_bound() {
    let started = tokio::time::Instant::now();
    let Err((detail, _)) = bound_reap(None, late_reap_wait()).await else {
        panic!("a reap past the bound cannot succeed");
    };
    assert_eq!(detail, BOUND_TEXT);
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(10)
    );
}

#[tokio::test(start_paused = true)]
async fn a_refused_kill_is_named_alongside_the_bound() {
    let Err((detail, Some(kill_error))) = bound_reap(Some(refused_kill()), pending_wait()).await
    else {
        panic!("the refused kill's error was dropped");
    };
    assert_eq!(
        residue_message(Some(4242), &detail, Some(&kill_error)),
        format!(
            "the exact child (pid 4242) - {BOUND_TEXT}; the kill itself was refused: {kill_error}"
        )
    );
}

#[tokio::test]
async fn a_reaped_child_is_clean_even_when_the_kill_was_refused() {
    let status = real_exit_status().await;
    for kill_error in [None, Some(refused_kill())] {
        match bound_reap(kill_error, std::future::ready(Ok(status))).await {
            Ok(reaped) => assert_eq!(reaped, status),
            Err((detail, _)) => panic!("a reaped child cannot be residue: {detail}"),
        }
    }
}

#[tokio::test]
async fn a_failed_wait_is_named_with_the_kill_note() {
    let wait = std::future::ready(Err(io::Error::from_raw_os_error(3)));
    let Err((detail, Some(kill_error))) = bound_reap(Some(refused_kill()), wait).await else {
        panic!("a failed wait with a refused kill must carry both");
    };
    assert!(detail.starts_with("it was not reaped: "));
    assert!(detail.contains("os error 3"));
    assert!(
        residue_message(None, &detail, Some(&kill_error)).contains("the kill itself was refused: ")
    );
}
