use super::*;
use std::path::Path;

const ROLE: &str = "GRAPT_BREAKAWAY_TEST_ROLE";
const ROOT: &str = "GRAPT_BREAKAWAY_TEST_ROOT";
const FIXTURE: &str = "dogfood::tools::terminal::windows_job::tests::fixture";

fn command(root: &Path, role: &str) -> Result<Command, String> {
    let executable = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut command = Command::new(executable);
    command
        .args(["--exact", FIXTURE, "--nocapture"])
        .env(ROLE, role)
        .env(ROOT, root)
        .stdin(windows_spawn::Stdio::null())
        .stdout(windows_spawn::Stdio::null())
        .stderr(windows_spawn::Stdio::null());
    Ok(command)
}

fn publish(path: &Path, bytes: impl AsRef<[u8]>) -> Result<(), String> {
    let pending = path.with_extension("pending");
    std::fs::write(&pending, bytes).map_err(|error| error.to_string())?;
    std::fs::rename(pending, path).map_err(|error| error.to_string())
}

fn wait_file(path: &Path) -> Result<(), String> {
    for _ in 0..500 {
        match path.try_exists() {
            Ok(true) => return Ok(()),
            Ok(false) => std::thread::sleep(CONFIRM_POLL),
            Err(error) => return Err(error.to_string()),
        }
    }
    Err(format!("fixture did not publish {}", path.display()))
}

/// Re-executed only by the job witness; ordinary suite invocation has no role.
#[test]
fn fixture() -> Result<(), String> {
    let Some(role) = std::env::var_os(ROLE) else {
        return Ok(());
    };
    let role = role.into_string().map_err(|_| "non-UTF8 fixture role")?;
    if !matches!(role.as_str(), "root" | "ordinary" | "escaped" | "msys") {
        return Err("unknown fixture role".into());
    }
    let root = std::env::var_os(ROOT).ok_or("fixture root absent")?;
    let root = Path::new(&root);
    if role == "msys" {
        publish(&root.join("msys-ready"), b"ready")?;
        return wait_file(&root.join("stop-msys"));
    }
    if role == "root" {
        let mut ordinary = command(root, "ordinary")?
            .spawn_with(
                SpawnOptions::new()
                    .drop_policy(DropPolicy::Detach)
                    .creation_flags(CreationFlags::NO_WINDOW),
            )
            .map_err(|e| e.to_string())?;
        let mut escaped = command(root, "escaped")?
            .spawn_with(
                SpawnOptions::new()
                    .drop_policy(DropPolicy::Detach)
                    .creation_flags(CreationFlags::NO_WINDOW | CreationFlags::BREAKAWAY_FROM_JOB),
            )
            .map_err(|e| e.to_string())?;
        publish(&root.join("ordinary.pid"), ordinary.id().to_string())?;
        publish(&root.join("escaped.pid"), escaped.id().to_string())?;
        wait_file(&root.join("stop-root"))?;
        ordinary.kill().map_err(|e| e.to_string())?;
        escaped.kill().map_err(|e| e.to_string())?;
        return Ok(());
    }
    publish(&root.join(format!("{role}-ready")), b"ready")?;
    wait_file(&root.join("job-closed"))?;
    publish(&root.join(format!("{role}-survived")), b"alive")?;
    wait_file(&root.join("stop-escaped"))
}

#[test]
fn msys_observation_belongs_to_the_owned_command_tree() -> Result<(), String> {
    let scratch = crate::dogfood::tools::tests::Scratch::new()?;
    let bin = scratch.0.join("Git/usr/bin");
    std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
    let executable = bin.join("sh.exe");
    std::fs::copy(
        std::env::current_exe().map_err(|e| e.to_string())?,
        &executable,
    )
    .map_err(|e| e.to_string())?;
    std::fs::write(bin.join("msys-2.0.dll"), b"image classification fixture")
        .map_err(|e| e.to_string())?;
    let mut observed_command = Command::new(&executable);
    observed_command
        .args(["--exact", FIXTURE, "--nocapture"])
        .env(ROLE, "msys")
        .env(ROOT, &scratch.0)
        .stdin(windows_spawn::Stdio::null())
        .stdout(windows_spawn::Stdio::null())
        .stderr(windows_spawn::Stdio::null());
    let mut own = ConfirmedJob::create()?;
    own.observe_msys();
    let mut unrelated = ConfirmedJob::create()?;
    unrelated.observe_msys();
    let _ordinary = unrelated.spawn(
        &mut command(&scratch.0, "ordinary")?,
        CreationFlags::NO_WINDOW,
    )?;
    wait_file(&scratch.0.join("ordinary-ready"))?;
    let _child = own.spawn(&mut observed_command, CreationFlags::NO_WINDOW)?;
    wait_file(&scratch.0.join("msys-ready"))?;
    for _ in 0..150 {
        if own.observed_msys() {
            break;
        }
        std::thread::sleep(CONFIRM_POLL);
    }
    assert!(own.observed_msys(), "owned MSYS image was not observed");
    assert!(
        !unrelated.observed_msys(),
        "another command's MSYS image leaked across job ownership"
    );
    own.terminate_and_confirm(1)?;
    unrelated.terminate_and_confirm(1)?;
    assert!(own.observed_msys(), "exit discarded the observation");
    let finished = || {
        [&own, &unrelated].iter().all(|job| {
            job.msys
                .as_ref()
                .is_some_and(|observer| observer.finished())
        })
    };
    for _ in 0..150 {
        if finished() {
            break;
        }
        std::thread::sleep(CONFIRM_POLL);
    }
    assert!(finished(), "completed command retained its observer worker");

    std::fs::remove_file(scratch.0.join("ordinary-ready")).map_err(|e| e.to_string())?;
    let mut unavailable = ConfirmedJob::create()?;
    unavailable.msys = Some(super::super::windows_msys::Observer::start_with(
        Arc::downgrade(&unavailable.observer),
        |_worker| Err(std::io::Error::other("injected observer startup refusal")),
    ));
    let _native = unavailable.spawn(
        &mut command(&scratch.0, "ordinary")?,
        CreationFlags::NO_WINDOW,
    )?;
    wait_file(&scratch.0.join("ordinary-ready"))?;
    assert!(
        unavailable.active()? > 0,
        "optional observation blocked command launch"
    );
    unavailable.terminate_and_confirm(1)?;
    let note = crate::dogfood::tools::sandbox::msys::failure_note;
    assert!(note(128, own.observed_msys()).is_some());
    assert!(note(128, unrelated.observed_msys()).is_none());
    assert!(note(0, own.observed_msys()).is_none());
    assert!(note(-1_073_741_502, false).is_some());
    Ok(())
}

#[test]
fn explicit_daemon_survives_but_ordinary_children_die_on_job_close() -> Result<(), String> {
    let scratch = crate::dogfood::tools::tests::Scratch::new()?;
    let job = ConfirmedJob::create()?;
    let mut root = job.spawn(&mut command(&scratch.0, "root")?, CreationFlags::NO_WINDOW)?;
    wait_file(&scratch.0.join("ordinary-ready"))?;
    wait_file(&scratch.0.join("escaped-ready"))?;
    wait_file(&scratch.0.join("escaped.pid"))?;
    let read_pid = |name: &str| -> Result<u32, String> {
        std::fs::read_to_string(scratch.0.join(name))
            .map_err(|e| e.to_string())?
            .parse::<u32>()
            .map_err(|e| e.to_string())
    };
    let pid = read_pid("escaped.pid")?;
    // Pin each live child while its parent remains alive. The independent
    // witness jobs own cleanup even when an assertion fails afterward.
    let escaped = processkit::ProcessGroup::new().map_err(|e| e.to_string())?;
    escaped.adopt_external(pid).map_err(|e| e.to_string())?;
    let ordinary = processkit::ProcessGroup::new().map_err(|e| e.to_string())?;
    ordinary
        .adopt_external(read_pid("ordinary.pid")?)
        .map_err(|e| e.to_string())?;
    assert_eq!(
        job.active()?,
        2,
        "root and ordinary child must both stay contained"
    );
    drop(job);
    let mut observed = false;
    for _ in 0..150 {
        if root.try_wait().map_err(|e| e.to_string())?.is_some()
            && ordinary.members().map_err(|e| e.to_string())?.is_empty()
        {
            observed = true;
            break;
        }
        std::thread::sleep(CONFIRM_POLL);
    }
    assert!(
        observed,
        "closing the job did not kill its root and ordinary child"
    );
    assert_eq!(escaped.members().map_err(|e| e.to_string())?, vec![pid]);
    publish(&scratch.0.join("job-closed"), b"closed")?;
    wait_file(&scratch.0.join("escaped-survived"))?;
    assert!(!scratch
        .0
        .join("ordinary-survived")
        .try_exists()
        .map_err(|e| e.to_string())?);
    publish(&scratch.0.join("stop-escaped"), b"stop")?;
    for _ in 0..150 {
        if escaped.members().map_err(|e| e.to_string())?.is_empty() {
            return Ok(());
        }
        std::thread::sleep(CONFIRM_POLL);
    }
    Err("escaped fixture did not exit after its stop".into())
}
