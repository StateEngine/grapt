//! Drain both bounded pipes while the copied CLI is still running.
use super::*;

pub(super) fn finish(
    process: &mut RestartProcess,
    input: Option<&[u8]>,
    polls: u16,
) -> TestResult<(std::process::ExitStatus, String, String)> {
    let stdout = process.0.stdout.take().ok_or("CLI stdout absent")?;
    let stderr = process.0.stderr.take().ok_or("CLI stderr absent")?;
    thread::scope(|scope| {
        let stdout = scope.spawn(move || read(stdout));
        let stderr = scope.spawn(move || read(stderr));
        let status = (|| -> TestResult<std::process::ExitStatus> {
            if let Some(input) = input {
                process
                    .0
                    .stdin
                    .as_mut()
                    .ok_or("CLI stdin absent")?
                    .write_all(input)?;
            }
            drop(process.0.stdin.take());
            for _ in 0..polls {
                if let Some(status) = process.0.try_wait()? {
                    return Ok(status);
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err("connection CLI did not finish within its fixture bound".into())
        })();
        if status.is_err() {
            drop(process.0.stdin.take());
            let _ = process.0.kill();
            let _ = process.0.wait();
        }
        let out = stdout.join().map_err(|_| "CLI stdout reader stopped")?;
        let error = stderr.join().map_err(|_| "CLI stderr reader stopped")?;
        Ok((status?, out?, error?))
    })
}

fn read(pipe: impl Read) -> std::io::Result<String> {
    let mut text = String::new();
    pipe.take(65537).read_to_string(&mut text)?;
    Ok(text)
}

#[test]
fn drains_both_pipes_before_waiting_for_exit() -> TestResult {
    const CHILD: &str = "GRAPT_TEST_CLI_PIPE_CHILD";
    const COMPLETE: usize = 49152;
    const OVERSIZED: usize = 131072;
    if let Some(mode) = std::env::var_os(CHILD) {
        let bytes = if mode == "complete" {
            COMPLETE
        } else {
            OVERSIZED
        };
        child_write(std::io::stdout(), b'o', bytes)?;
        child_write(std::io::stderr(), b'e', bytes)?;
        // The bounded readers deliberately close oversized pipes. Avoid the
        // test harness writing its footer into those closed child streams.
        std::process::exit(0);
    }
    for mode in ["complete", "oversized"] {
        let mut process = RestartProcess(
            std::process::Command::new(std::env::current_exe()?)
                .args([
                    "--exact",
                    "connections::cli::output::drains_both_pipes_before_waiting_for_exit",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
        );
        let (status, out, error) = finish(&mut process, None, 1000)?;
        assert!(status.success(), "pipe fixture exited {status}");
        if mode == "complete" {
            assert!(out.len() <= 65536 && error.len() <= 65536);
            assert!(out.contains(&"o".repeat(COMPLETE)), "stdout was truncated");
            assert_eq!(error, "e".repeat(COMPLETE), "stderr was truncated");
        } else {
            assert_eq!(out.len(), 65537, "stdout retention was not bounded");
            assert_eq!(error, "e".repeat(65537), "stderr retention was not bounded");
        }
    }
    Ok(())
}

fn child_write(mut pipe: impl Write, byte: u8, bytes: usize) -> std::io::Result<()> {
    match pipe.write_all(&vec![byte; bytes]) {
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}
