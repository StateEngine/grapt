//! A dead supervisor's leftover containment is reclaimed at the next start;
//! nothing a live owner holds, and no unit whose bridge still runs, is touched.
use super::*;
use std::error::Error;

/// Every systemd call here is bounded by the same allowance as production.
/// A call that exhausts it answered nothing, so an idempotent call is asked
/// again until the witness cap: only a manager that never answers fails the
/// witness (gate 5014aeb0 exhausted one allowance under IO saturation).
fn bounded(command: impl Fn() -> Command) -> Result<std::process::Output, Box<dyn Error>> {
    let started = tokio::time::Instant::now();
    loop {
        match bounded_command_output_within(command(), UNIT_MAIN_STATE_ALLOWANCE) {
            Err(error)
                if error.kind() == std::io::ErrorKind::TimedOut
                    && started.elapsed() < super::tests::WITNESS_CAP => {}
            output => return Ok(output?),
        }
    }
}

/// `show` and `set-property --runtime` are idempotent, so they may be asked
/// again after an unanswered call.
fn systemctl_user(arguments: &[&str]) -> Result<std::process::Output, Box<dyn Error>> {
    bounded(|| {
        let mut command = Command::new("/usr/bin/systemctl");
        command
            .arg("--user")
            .arg("--no-pager")
            .args(arguments)
            .env_clear();
        command
    })
}

/// One `--property=` value of a unit, as systemd reports it.
fn property(unit: &str, name: &str) -> Result<String, Box<dyn Error>> {
    let output = systemctl_user(&["show", unit, &format!("--property={name}"), "--value"])?;
    Ok(String::from_utf8(output.stdout)?.trim().to_owned())
}

fn unit_command(unit: &str, slice: &str, remain: bool, wait: bool, program: &[&str]) -> Command {
    let mut command = Command::new("/usr/bin/systemd-run");
    command.args(["--user", "--quiet", "--service-type=exec"]);
    if wait {
        command.args(["--wait", "--pipe"]);
    }
    command
        .arg(format!("--unit={unit}"))
        .arg(format!("--slice={slice}"))
        .arg(format!(
            "--property=RemainAfterExit={}",
            if remain { "yes" } else { "no" }
        ))
        .args(program)
        .env_clear();
    command
}

/// Start a transient unit in `slice` running `program`, without waiting.
/// Starting is not idempotent, so an unanswered start is judged by whether
/// the unit was loaded rather than asked again.
fn start_unit(
    unit: &str,
    slice: &str,
    remain: bool,
    program: &[&str],
) -> Result<(), Box<dyn Error>> {
    let started = match bounded_command_output_within(
        unit_command(unit, slice, remain, false, program),
        UNIT_MAIN_STATE_ALLOWANCE,
    ) {
        Ok(output) => output.status.success(),
        Err(error) if error.kind() == std::io::ErrorKind::TimedOut => {
            property(&format!("{unit}.service"), "LoadState")? == "loaded"
        }
        Err(error) => return Err(error.into()),
    };
    if !started {
        return Err(format!("{unit} did not start").into());
    }
    Ok(())
}

/// A pid that existed and is gone: a reaped child.
fn dead_pid() -> Result<u32, Box<dyn Error>> {
    let mut child = Command::new("/usr/bin/true")
        .env_clear()
        .spawn_with_required_environment()?;
    let pid = child.id();
    child.wait()?;
    Ok(pid)
}

/// Poll `check` until it holds, for at most the witness cap.
fn settles(
    mut check: impl FnMut() -> Result<bool, Box<dyn Error>>,
) -> Result<bool, Box<dyn Error>> {
    let deadline = tokio::time::Instant::now() + super::tests::WITNESS_CAP;
    loop {
        if check()? {
            return Ok(true);
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Everything the witness created, removed when it ends however it ends.
#[derive(Default)]
struct Created {
    units: Vec<String>,
    slices: Vec<String>,
    children: Vec<std::process::Child>,
}

impl Drop for Created {
    fn drop(&mut self) {
        for unit in &self.units {
            let _ = stop_unit(unit);
        }
        for slice in &self.slices {
            let _ = bounded_systemctl(&["stop", slice]);
            let _ = bounded_systemctl(&["revert", slice]);
        }
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[test]
fn a_dead_supervisors_containment_is_reclaimed_and_nothing_live_is_touched(
) -> Result<(), Box<dyn Error>> {
    let mut created = Created::default();
    let (dead, dead_running) = (dead_pid()?, dead_pid()?);
    // The live owner and the running bridge must outlive every wait the
    // witness may spend; `Created` ends both however the witness ends.
    created.children.push(
        Command::new("/usr/bin/sleep")
            .arg("infinity")
            .env_clear()
            .spawn_with_required_environment()?,
    );
    let live_pid = created.children[0].id();
    let slice = |owner: u32| format!("secode_plugins_{owner}_1.slice");
    let unit = |owner: u32, n: u32| format!("secode-plugin-{owner}-1-{n}");
    let service = |owner: u32, n: u32| format!("{}.service", unit(owner, n));
    for owner in [dead, dead_running, live_pid] {
        created.slices.push(slice(owner));
        created.units.push(unit(owner, 1));
    }
    created.units.push(unit(dead, 2));

    // The dead owner's leftovers: an exited unit with its waiting launcher,
    // a failed unit, and its slice with runtime properties.
    let properties = systemctl_user(&["set-property", "--runtime", &slice(dead), "TasksMax=64"])?;
    assert!(properties.status.success(), "slice properties were refused");
    created.children.push(
        unit_command(&unit(dead, 1), &slice(dead), true, true, &["/usr/bin/true"])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn_with_required_environment()?,
    );
    let launcher = created.children.len() - 1;
    start_unit(&unit(dead, 2), &slice(dead), false, &["/usr/bin/false"])?;
    // Held: a dead owner's unit whose main process still runs, and a live
    // owner's exited unit.
    start_unit(
        &unit(dead_running, 1),
        &slice(dead_running),
        true,
        &["/usr/bin/sleep", "infinity"],
    )?;
    start_unit(
        &unit(live_pid, 1),
        &slice(live_pid),
        true,
        &["/usr/bin/true"],
    )?;
    let settled = settles(|| {
        Ok(property(&service(dead, 1), "SubState")? == "exited"
            && property(&service(dead, 2), "ActiveState")? == "failed"
            && property(&service(live_pid, 1), "SubState")? == "exited")
    })?;
    assert!(settled, "the leftover shapes never formed");
    assert!(
        created.children[launcher].try_wait()?.is_none(),
        "the launcher must still be waiting"
    );

    // A listing the manager did not answer judged nothing; ask again.
    let judged = settles(|| Ok(reclaim_orphaned_containment()))?;
    assert!(
        judged,
        "the user manager never answered the reclaim listing"
    );

    let reclaimed = settles(|| {
        Ok(property(&service(dead, 1), "ActiveState")? == "inactive"
            && property(&service(dead, 2), "ActiveState")? == "inactive"
            && property(&slice(dead), "ActiveState")? == "inactive")
    })?;
    assert!(
        reclaimed,
        "the dead supervisor's units and slice were not reclaimed"
    );
    let released = settles(|| Ok(created.children[launcher].try_wait()?.is_some()))?;
    assert!(released, "the systemd-run launcher was left waiting");
    assert_eq!(
        property(&service(dead_running, 1), "SubState")?,
        "running",
        "a running bridge's unit was touched"
    );
    assert_eq!(
        property(&slice(dead_running), "ActiveState")?,
        "active",
        "a slice holding a running unit was touched"
    );
    assert_eq!(
        property(&service(live_pid, 1), "SubState")?,
        "exited",
        "a live owner's unit was touched"
    );
    assert_eq!(
        property(&slice(live_pid), "ActiveState")?,
        "active",
        "a live owner's slice was touched"
    );
    Ok(())
}

#[test]
fn only_this_crates_generated_names_are_listed() {
    let listing = "Id=secode-plugin-41-1-2.service\nMainPID=0\n\n\
                   Id=secode-plugin-plr111-systemctl-7.service\nMainPID=0\n\n\
                   Id=secode-plugin-42-1-1.service\nMainPID=99\n\n\
                   Id=secode-plugin-43-1.service\nMainPID=0\n\n\
                   Id=secode-plugin-44-1-2-3.service\nMainPID=0\n\n\
                   Id=secode-plugin-45-x-2.service\nMainPID=0\n\n\
                   Id=secode-plugin-46--2.service\nMainPID=0\n\n\
                   Id=secode_plugins_41_3.slice\n\n\
                   Id=secode_plugins_47.slice\n\n\
                   Id=secode_plugins_48_1_2.slice\n\n\
                   Id=secode_plugins_x_3.slice\n";
    let (units, slices) = orphaned_containment(listing);
    assert_eq!(
        units,
        vec![
            ("secode-plugin-41-1-2".to_owned(), 41, Some(0)),
            ("secode-plugin-42-1-1".to_owned(), 42, Some(99)),
        ]
    );
    assert_eq!(slices, vec![("secode_plugins_41_3.slice".to_owned(), 41)]);
}
