use super::*;
use std::io::Read;
use std::time::Duration;

fn bounded(child: &mut std::process::Child) -> io::Result<std::process::ExitStatus> {
    for _ in 0..100 {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill()?;
    child.wait()?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "unrelated child did not exit",
    ))
}

#[test]
fn stderr_inheritance_is_child_local_and_parent_stays_cloexec(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut reader, original) = io::pipe()?;
    let original = OwnedFd::from(original);
    let destination = original.try_clone()?;
    let descriptor = destination.as_raw_fd();
    // A competing spawn must see neither output capability. The positive child
    // below proves this shell can observe /dev/fd when inheritance is intended.
    let source = format!(
        "test ! -e /dev/fd/{} && test ! -e /dev/fd/{descriptor}",
        original.as_raw_fd()
    );
    let unrelated = std::thread::spawn(move || -> io::Result<()> {
        for _ in 0..16 {
            let mut child = std::process::Command::new("/bin/sh")
                .args(["-c", &source])
                .spawn()?;
            if !bounded(&mut child)?.success() {
                return Err(io::Error::other(
                    "unrelated child inherited Seatbelt stderr",
                ));
            }
        }
        Ok(())
    });
    let arguments = [
        OsString::from("/bin/sh"),
        "-c".into(),
        format!("printf admitted > /dev/fd/{descriptor}").into(),
    ];
    let (mut child, mut startup) = spawn(
        Path::new("/bin/sh"),
        &arguments,
        &[],
        &original,
        &destination,
    )?;
    for fd in [&original, &destination] {
        assert_ne!(fcntl(fd, FcntlArg::F_GETFD)? & FdFlag::FD_CLOEXEC.bits(), 0);
    }
    let mut status = None;
    for _ in 0..100 {
        status = child.try_wait()?;
        if status.is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        status,
        Some(0),
        "intended child did not receive its descriptor"
    );
    let mut diagnostics = String::new();
    startup.read_to_string(&mut diagnostics)?;
    assert!(diagnostics.is_empty(), "{diagnostics}");
    unrelated
        .join()
        .map_err(|_| "unrelated-child control panicked")??;
    drop(original);
    drop(destination);
    let mut received = String::new();
    reader.read_to_string(&mut received)?;
    assert_eq!(received, "admitted");
    assert_eq!(child.wait()?, 0);
    assert!(child.kill().is_err(), "reaped PID remained signalable");
    Ok(())
}
