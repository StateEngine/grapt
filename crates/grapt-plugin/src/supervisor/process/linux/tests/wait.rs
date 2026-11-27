//! Launcher completion and unit-main settlement are different observations.
use super::*;

#[test]
fn settled_unit_does_not_shorten_launcher_exit_wait() -> Result<(), Box<dyn Error>> {
    let (control, unit) = spawn_linux_test_process("exit 0", &[], linux_test_limits(100), false)?;
    let mut stopper = None;
    let outcome = (|| -> Result<(), Box<dyn Error>> {
        let started = tokio::time::Instant::now();
        // Require a loaded terminal unit: an absent unit before startup is not
        // evidence that the payload ran.
        while !attach_unit_main_process_exited(&unit) {
            if started.elapsed() >= WITNESS_CAP {
                return Err("unit did not reach a loaded terminal state".into());
            }
            thread::sleep(Duration::from_millis(50));
        }
        if control.has_exited()? {
            return Err("launcher exited before the delayed teardown window".into());
        }
        let delayed = control.clone();
        stopper = Some(
            thread::Builder::new()
                .name("delayed-plugin-stop".into())
                .spawn(move || {
                    // Fault injection: keep a real settled unit loaded past the old
                    // ten-second grace, then perform the existing containment stop.
                    thread::sleep(Duration::from_secs(14));
                    delayed
                        .stop_containment()
                        .map_err(|error| error.to_string())
                })?,
        );
        wait_for_exit(&control, &unit)
    })();
    // Teardown is proven before the regression result, including its old red.
    let cleanup = control.terminate();
    let stopped = finish_stopper(stopper);
    let absent = wait_for_unit_absent(&unit, WITNESS_CAP);
    cleanup?;
    stopped?;
    absent?;
    outcome
}

#[test]
fn live_launcher_still_fails_at_the_witness_cap() -> Result<(), Box<dyn Error>> {
    let limits = linux_test_limits(100);
    let (control, unit) = spawn_linux_test_process("exec /usr/bin/sleep 300", &[], limits, false)?;
    let outcome = (|| -> Result<(), Box<dyn Error>> {
        locate_cgroup(
            &control,
            &unit,
            Duration::from_millis(50),
            Duration::from_millis(limits.startup_millis),
        )
        .ok_or("live sentinel did not start")?;
        let failure = wait_for_exit_within(&control, &unit, Duration::from_millis(50))
            .err()
            .ok_or("a live launcher was accepted as completed")?
            .to_string();
        if !failure.contains(&unit) || !failure.contains("did not exit within") {
            return Err(format!("wrong cap failure: {failure}").into());
        }
        if control.has_exited()? {
            return Err("the live sentinel exited before the cap control was judged".into());
        }
        Ok(())
    })();
    let cleanup = control.terminate();
    let absent = wait_for_unit_absent(&unit, WITNESS_CAP);
    cleanup?;
    absent?;
    outcome
}

fn finish_stopper(
    stopper: Option<thread::JoinHandle<Result<(), String>>>,
) -> Result<(), Box<dyn Error>> {
    let Some(stopper) = stopper else {
        return Ok(());
    };
    let started = tokio::time::Instant::now();
    while !stopper.is_finished() {
        if started.elapsed() >= WITNESS_CAP {
            return Err("delayed containment stop did not settle within the witness cap".into());
        }
        thread::sleep(Duration::from_millis(50));
    }
    stopper
        .join()
        .map_err(|_| "delayed containment stop panicked")??;
    Ok(())
}
