//! A spawned child's command line can be transiently empty during exec.
use super::*;
use std::cell::Cell;
use tokio::time::Instant;

pub(super) fn observe_ready(
    proc: &dyn ProcSource,
    parent: u32,
    child: u32,
    deadline: Instant,
) -> Result<ChildrenObservation, String> {
    loop {
        let observed = observe_children_in(proc, parent);
        if observed.unavailable.is_some() {
            return Err(format!("child listing unavailable: {observed:?}"));
        }
        match observed.children.iter().find(|found| found.pid == child) {
            Some(found) if found.cmdline.starts_with("unavailable:") => {
                return Err(format!("child command line unreadable: {found:?}"));
            }
            Some(found) if !found.cmdline.is_empty() => return Ok(observed),
            _ => {}
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "CMDLINE_READINESS_DEADLINE: child {child} not yet observable; {observed:?}"
            ));
        }
        // Yield to exec progress; readiness is the nonempty command line,
        // never an arbitrary sleep or a retry of the evidence assertion.
        thread::yield_now();
    }
}

/// Script the observed exec window without depending on host scheduling.
struct ExecWindow {
    proc: FakeProc,
    empty_reads: usize,
    reads: Cell<usize>,
}

impl ExecWindow {
    fn new(empty_reads: usize) -> Self {
        Self {
            proc: FakeProc::product(vec![Ok("/proc/100/task/100".into())]),
            empty_reads,
            reads: Cell::new(0),
        }
    }
}

impl ProcSource for ExecWindow {
    fn entries(&self, path: &Path, limit: usize) -> Result<Vec<Result<PathBuf, String>>, String> {
        self.proc.entries(path, limit)
    }

    fn read(&self, path: &Path, limit: u64) -> Result<String, String> {
        if path == Path::new("/proc/4242/cmdline") {
            let read = self.reads.get();
            self.reads.set(read + 1);
            if read < self.empty_reads {
                return Ok(String::new());
            }
        }
        self.proc.read(path, limit)
    }
}

#[test]
fn transient_empty_cmdline_is_never_a_final_stress_sample() {
    const SAMPLES: usize = 256;
    let mut empty = 0;
    for sample in 0..SAMPLES {
        let proc = ExecWindow::new(1 + sample % 8);
        let observed = observe_ready(
            &proc,
            100,
            4242,
            Instant::now() + Duration::from_secs(CHILD_LIFETIME_SECS),
        )
        .unwrap_or_else(|error| panic!("exec readiness failed: {error}"));
        assert_eq!(observed.children.len(), 1, "{observed:?}");
        let found = &observed.children[0];
        if found.cmdline.is_empty() {
            empty += 1;
        } else {
            assert_eq!(found.cmdline, "bash -c echo preset-ok");
        }
    }
    eprintln!("exec-window stress: {empty}/{SAMPLES} empty final samples");
    assert_eq!(empty, 0, "EMPTY_CMDLINE_FINAL_SAMPLE: {empty}/{SAMPLES}");
}

#[test]
fn an_unobservable_cmdline_fails_at_its_sourced_deadline() {
    let proc = ExecWindow::new(usize::MAX);
    let result = observe_ready(&proc, 100, 4242, Instant::now());
    let error = result
        .err()
        .unwrap_or_else(|| panic!("empty cmdline became ready"));
    assert!(error.contains("CMDLINE_READINESS_DEADLINE"), "{error}");
    assert!(error.contains("cmdline: \"\""), "{error}");
    assert_eq!(
        proc.reads.get(),
        1,
        "expired deadline must not sample again"
    );
}

#[test]
fn an_unreadable_cmdline_is_not_retried_as_empty() {
    let mut proc = ExecWindow::new(0);
    proc.proc
        .files
        .insert("/proc/4242/cmdline".into(), Err("Permission denied".into()));
    let result = observe_ready(
        &proc,
        100,
        4242,
        Instant::now() + Duration::from_secs(CHILD_LIFETIME_SECS),
    );
    let error = result
        .err()
        .unwrap_or_else(|| panic!("unreadable cmdline became ready"));
    assert!(error.contains("Permission denied"), "{error}");
    assert_eq!(proc.reads.get(), 1, "unreadable is an immediate failure");
}

#[test]
fn an_observable_child_survives_an_unrelated_task_listing_race() {
    let mut proc = ExecWindow::new(0);
    proc.proc.dirs.insert(
        "/proc/100/task".into(),
        Ok(vec![
            Ok("/proc/100/task/100".into()),
            Err("No such file or directory".into()),
        ]),
    );
    let observed = observe_ready(&proc, 100, 4242, Instant::now())
        .unwrap_or_else(|error| panic!("owned child was observable: {error}"));
    assert_eq!(observed.children[0].cmdline, "bash -c echo preset-ok");
    assert_eq!(
        observed.skipped,
        vec!["No such file or directory".to_owned()]
    );
    assert_eq!(proc.reads.get(), 1);
}
