//! Positive, best-effort observations of MSYS images in this command's job.
//! A missed or unreadable process never establishes that no MSYS child ran.
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Weak};
use std::thread::JoinHandle;
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use tokio::sync::Semaphore;

// A slow image query cannot accumulate detached workers across completed
// commands. Exhaustion disables this optional diagnostic, never command launch.
static CAPACITY: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(super::GLOBAL_RESOURCES)));

pub(super) struct Observer {
    stopped: Arc<AtomicBool>,
    observed: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Observer {
    pub fn start(job: Weak<win32job::Job>) -> Self {
        Self::start_with(job, |worker| {
            std::thread::Builder::new()
                .name("grapt-msys-observer".into())
                .spawn(worker)
        })
    }

    pub fn start_with(
        job: Weak<win32job::Job>,
        spawn: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>>,
    ) -> Self {
        let stopped = Arc::new(AtomicBool::new(false));
        let observed = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopped);
        let found = Arc::clone(&observed);
        let worker = Arc::clone(&CAPACITY)
            .try_acquire_owned()
            .ok()
            .and_then(|permit| {
                spawn(Box::new(move || {
                    let _permit = permit;
                    let mut previous = Vec::new();
                    while !stop.load(Ordering::Acquire) {
                        // Each census is bounded to 1025 entries. Its temporary
                        // strong job handle is released BEFORE image/filesystem I/O.
                        let Some(mut current) = members(&job) else {
                            break;
                        };
                        current.sort_unstable();
                        current.dedup();
                        if current != previous {
                            let pids: Vec<_> = current
                                .iter()
                                .filter_map(|pid| u32::try_from(*pid).ok().map(Pid::from_u32))
                                .collect();
                            let mut system = System::new();
                            system.refresh_processes_specifics(
                                ProcessesToUpdate::Some(&pids),
                                true,
                                ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
                            );
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                            let Some(after) = members(&job) else { break };
                            if pids.iter().any(|pid| {
                                after.contains(&(pid.as_u32() as usize))
                                    && system.process(*pid).and_then(|p| p.exe()).is_some_and(msys)
                            }) {
                                found.store(true, Ordering::Release);
                                break;
                            }
                            previous = current;
                        }
                        std::thread::sleep(Duration::from_millis(10));
                    }
                }))
                .ok()
            });
        Self {
            stopped,
            observed,
            worker,
        }
    }

    pub fn observed(&self) -> bool {
        self.observed.load(Ordering::Acquire)
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
    }

    #[cfg(test)]
    pub fn finished(&self) -> bool {
        self.worker.as_ref().is_none_or(JoinHandle::is_finished)
    }
}

impl Drop for Observer {
    fn drop(&mut self) {
        self.stop();
        // Never join an optional metadata reader on command cleanup. Its permit
        // bounds outstanding workers; its Weak job reference cannot keep a
        // kill-on-close job alive while an image query is slow.
        drop(self.worker.take());
    }
}

fn members(job: &Weak<win32job::Job>) -> Option<Vec<usize>> {
    job.upgrade()?.query_process_id_list().ok()
}

fn msys(path: &Path) -> bool {
    let Some(parent) = path.parent() else {
        return false;
    };
    path.file_name().is_some_and(|name| {
        ["sh.exe", "bash.exe", "ssh.exe"]
            .iter()
            .any(|candidate| name.eq_ignore_ascii_case(candidate))
    }) && parent
        .file_name()
        .is_some_and(|name| name.eq_ignore_ascii_case("bin"))
        && parent
            .parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name.eq_ignore_ascii_case("usr"))
        && parent.join("msys-2.0.dll").is_file()
}
