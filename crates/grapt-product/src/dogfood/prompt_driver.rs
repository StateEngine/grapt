//! Service-owned, bounded execution custody, independent of HTTP lifetimes.
use grapt_api::u3a::STALL_TICK;
use grapt_types::ExecutionId;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, OwnedSemaphorePermit, Semaphore};
use tokio::task::{Id, JoinSet};

type Work = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;
type Cancel = Arc<dyn Fn() + Send + Sync>;
type Shutdown = Arc<dyn Fn(super::lifecycle::StopReason) + Send + Sync>;
type Admission = Arc<Mutex<Option<super::lifecycle::StopReason>>>;

struct Job {
    execution: Option<ExecutionId>,
    work: Work,
    cancel: Shutdown,
    failed: Cancel,
    capacity: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(super) struct PromptMailbox {
    sender: mpsc::Sender<Job>,
    capacity: Arc<Semaphore>,
    controls: Arc<Semaphore>,
    admission: Admission,
}

pub(super) struct Reservation {
    permit: mpsc::OwnedPermit<Job>,
    capacity: OwnedSemaphorePermit,
    admission: Admission,
}

pub(crate) struct PromptDriver {
    native_mail: Option<Arc<super::native_mail::custody::NativeMailOwners>>,
    shutdown_notice: Option<Shutdown>,
    skills: Option<Arc<super::skills_watch::Drive>>,
    session_ends: Option<Arc<super::hooks::session_end::SessionEnds>>,
    mcp: Option<Arc<super::mcp::McpRegistry>>,
    /// The writer that builds the mailbox scan indexes once, after readiness.
    scan_indexes: Option<Arc<Mutex<grapt_inbox::Inbox>>>,
    /// Post-readiness automatic resume for crash-interrupted turns.
    auto_resume_app: Option<Arc<super::DogfoodApplication>>,
    auto_resume_principal: Option<grapt_api::SealedPrincipal>,
    /// The hub whose window registry decides when a Session's last window is
    /// gone, and the stop applied when it is.
    close_grace: Option<CloseGraceDrive>,
    receiver: mpsc::Receiver<Job>,
    admission: Admission,
    capacity: Arc<Semaphore>,
    tasks: JoinSet<Option<ExecutionId>>,
    cancellations: Vec<TaskCustody>,
}

struct TaskCustody {
    task: Id,
    cancel: Shutdown,
    failed: Cancel,
}

/// Stops a turn whose last window is gone.
pub(crate) type CloseStop = Arc<dyn Fn(&grapt_types::SessionId) + Send + Sync>;

/// The product's end of the window registry: where the deadline comes from, and
/// what stopping an unwatched turn means here.
pub(crate) struct CloseGraceDrive {
    windows: Arc<grapt_api::WindowWatch>,
    stop: CloseStop,
}

/// Sleep until the next grace expires, then stop each Session it names.
///
/// Runs forever and is cancelled by the driver's own shutdown. It sleeps to the
/// earlier of the next deadline and [`STALL_TICK`], or until a window changes.
///
/// Every reading is monotonic. A wake is late only when it lands after the tick
/// it was due, which means the loop itself was blocked across that tick: that
/// time was the stall's, not the window's absence, so the running graces restart
/// rather than expire. An ordinary wake, including the one at a grace deadline,
/// is never late and decides nothing extra.
async fn close_grace_sleep(
    drive: &CloseGraceDrive,
    changes: &mut tokio::sync::watch::Receiver<()>,
) {
    let mut tick_was_due = tokio::time::Instant::now();
    loop {
        let now = tokio::time::Instant::now();
        if now.saturating_duration_since(tick_was_due) > STALL_TICK {
            drive.windows.rebase(now);
        }
        for session in drive.windows.expired(now) {
            (drive.stop)(&session);
        }
        tick_was_due = now + STALL_TICK;
        let stall = tokio::time::sleep_until(tick_was_due);
        tokio::pin!(stall);
        match drive.windows.next_deadline(now) {
            None => {
                // Nothing is unwatched, so there is nothing to time out. Sleep
                // until a window changes; a closed receiver means the hub is
                // gone, and there is nothing left to stop. The stall tick still
                // runs, so a loop that was blocked is noticed when it recovers.
                tokio::select! {
                    _ = &mut stall => {}
                    changed = changes.changed() => {
                        if changed.is_err() {
                            std::future::pending::<()>().await;
                        }
                    }
                }
            }
            Some(deadline) => {
                tokio::select! {
                    _ = tokio::time::sleep_until(deadline) => {}
                    _ = &mut stall => {}
                    changed = changes.changed() => {
                        if changed.is_err() {
                            std::future::pending::<()>().await;
                        }
                    }
                }
            }
        }
    }
}

fn build_scan_indexes(inbox: &Mutex<grapt_inbox::Inbox>) {
    let built = match inbox.lock() {
        Ok(mut inbox) => inbox
            .store_mut()
            .build_mailbox_scan_indexes()
            .map_err(|error| error.to_string()),
        Err(_) => Err("the inbox lock is poisoned".to_owned()),
    };
    if let Err(error) = built {
        crate::diagnostics::report(format!("mailbox scan indexes were not built: {error}"));
    }
}

impl PromptMailbox {
    pub(super) fn new() -> (Self, PromptDriver) {
        let (sender, receiver) = mpsc::channel(2 * super::MAX_ACTIVE_EXECUTIONS);
        let capacity = Arc::new(Semaphore::new(super::MAX_ACTIVE_EXECUTIONS));
        let admission = Arc::new(Mutex::new(None));
        (
            Self {
                sender,
                capacity: Arc::clone(&capacity),
                controls: Arc::new(Semaphore::new(super::MAX_ACTIVE_EXECUTIONS)),
                admission: Arc::clone(&admission),
            },
            PromptDriver {
                native_mail: None,
                shutdown_notice: None,
                skills: None,
                session_ends: None,
                mcp: None,
                scan_indexes: None,
                auto_resume_app: None,
                auto_resume_principal: None,
                close_grace: None,
                receiver,
                admission,
                capacity,
                tasks: JoinSet::new(),
                cancellations: Vec::new(),
            },
        )
    }

    /// Reserve before durable admission. The permit covers queued AND running
    /// executions and remains held until the owned future has ended.
    pub(super) fn reserve(&self) -> Result<Reservation, String> {
        let capacity = Arc::clone(&self.capacity)
            .try_acquire_owned()
            .map_err(|_| "prompt execution capacity is unavailable")?;
        let permit = self
            .sender
            .clone()
            .try_reserve_owned()
            .map_err(|_| "prompt execution owner is unavailable")?;
        Ok(Reservation {
            permit,
            capacity,
            admission: Arc::clone(&self.admission),
        })
    }

    /// Control work remains available even when all prompt slots are occupied.
    pub(super) fn reserve_control(&self) -> Result<Reservation, String> {
        let capacity = Arc::clone(&self.controls)
            .try_acquire_owned()
            .map_err(|_| {
                "Session stop capacity is unavailable; retry after another stop completes"
            })?;
        let permit = self
            .sender
            .clone()
            .try_reserve_owned()
            .map_err(|_| "Session stop owner is unavailable; restart the service")?;
        Ok(Reservation {
            permit,
            capacity,
            admission: Arc::clone(&self.admission),
        })
    }
}

impl Reservation {
    #[cfg(test)]
    pub(super) fn submit(self, execution: ExecutionId, work: Work, cancel: Cancel, failed: Cancel) {
        self.submit_shutdown(execution, work, Arc::new(move |_| cancel()), failed);
    }
    pub(super) fn submit_shutdown(
        self,
        execution: ExecutionId,
        work: Work,
        cancel: Shutdown,
        failed: Cancel,
    ) {
        Self::submit_job(
            self.permit,
            &self.admission,
            Job {
                execution: Some(execution),
                work,
                cancel,
                failed,
                capacity: self.capacity,
            },
        );
    }

    pub(super) fn submit_control(self, work: Work) {
        Self::submit_job(
            self.permit,
            &self.admission,
            Job {
                execution: None,
                work,
                cancel: Arc::new(|_| {}),
                failed: Arc::new(|| {}),
                capacity: self.capacity,
            },
        );
    }

    fn submit_job(permit: mpsc::OwnedPermit<Job>, admission: &Admission, job: Job) {
        let held = match admission.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(reason) = *held {
            drop(held);
            (job.cancel)(reason);
            // Dropping control work closes its reply instead of enqueuing
            // through a permit that outlived the driver's final drain.
            drop(job);
        } else {
            permit.send(job);
        }
    }
}

impl PromptDriver {
    pub(super) fn attach_mcp(&mut self, mcp: Arc<super::mcp::McpRegistry>) {
        self.mcp = Some(mcp);
    }
    pub(super) fn attach_scan_indexes(&mut self, inbox: Arc<Mutex<grapt_inbox::Inbox>>) {
        self.scan_indexes = Some(inbox);
    }
    pub(super) fn attach_auto_resume_app(&mut self, application: Arc<super::DogfoodApplication>) {
        self.auto_resume_app = Some(application);
    }
    pub(crate) fn attach_auto_resume_principal(&mut self, principal: grapt_api::SealedPrincipal) {
        self.auto_resume_principal = Some(principal);
    }
    /// Drive the window registry: stop a turn when its last window is gone.
    pub(super) fn attach_close_grace(
        &mut self,
        windows: Arc<grapt_api::WindowWatch>,
        stop: CloseStop,
    ) {
        self.close_grace = Some(CloseGraceDrive { windows, stop });
    }
    pub(super) fn attach_skills(&mut self, drive: Arc<super::skills_watch::Drive>) {
        self.skills = Some(drive);
    }
    pub(super) fn attach_shutdown_notice(&mut self, notice: Shutdown) {
        self.shutdown_notice = Some(notice);
    }
    pub(super) fn attach_session_ends(
        &mut self,
        ends: Arc<super::hooks::session_end::SessionEnds>,
    ) {
        self.session_ends = Some(ends);
    }
    pub(super) fn attach_native_mail(
        &mut self,
        owners: Arc<super::native_mail::custody::NativeMailOwners>,
    ) {
        self.native_mail = Some(owners);
    }

    #[cfg(test)]
    pub(crate) async fn run_until(&mut self, stop: impl Future<Output = ()>) -> Result<(), String> {
        self.run_until_shutdown(async {
            stop.await;
            super::lifecycle::StopReason::ServiceStopped
        })
        .await
    }
    #[expect(clippy::cognitive_complexity, reason = "legacy")]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub(crate) async fn run_until_shutdown(
        &mut self,
        stop: impl Future<Output = super::lifecycle::StopReason>,
    ) -> Result<(), String> {
        tokio::pin!(stop);
        if let Some(inbox) = self.scan_indexes.take() {
            // The driver starts after readiness, so this build is outside the
            // launch window. Detached: the reads work without the indexes.
            drop(tokio::task::spawn_blocking(move || {
                build_scan_indexes(&inbox)
            }));
        }
        let auto_resume_task = match (
            self.auto_resume_app.take(),
            self.auto_resume_principal.take(),
        ) {
            (Some(application), Some(principal)) => Some(tokio::task::spawn(async move {
                super::prompt_submission::auto_resume::drive(application, principal).await;
            })),
            _ => None,
        };
        let mut stopping = super::lifecycle::StopReason::ServiceStopped;
        let native = self.native_mail.clone();
        let mut mail_drive = native
            .as_deref()
            .map(super::native_mail::custody::MailDrive::new);
        let skills = self.skills.clone();
        let skills_work = async {
            if let Some(skills) = skills {
                skills.run().await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::pin!(skills_work);
        let close = self.close_grace.take();
        let mut close_changes = close.as_ref().map(|drive| drive.windows.changes());
        let mut result = loop {
            if let Some(native) = &native {
                // An actual close is a finite joined service action, not a
                // cancellable poll. A busy prompt can defer its own close;
                // unrelated held context leases are never awaited here.
                native.close_pending().await;
                native.enqueue_ready();
            }
            tokio::select! {
                biased;
                reason = &mut stop => { stopping = reason; break Ok(()); },
                _ = async {
                    match (&close, &mut close_changes) {
                        (Some(drive), Some(changes)) => {
                            close_grace_sleep(drive, changes).await;
                        }
                        _ => std::future::pending().await,
                    }
                } => {},
                _ = &mut skills_work => break Err("skill watcher driver ended unexpectedly".into()),
                result = self.tasks.join_next_with_id(), if !self.tasks.is_empty() => {
                    match result {
                        Some(Ok((task, _))) => self.cancellations.retain(|held| held.task != task),
                        Some(Err(error)) => {
                            // A failed task cannot stop unrelated Sessions. Task IDs
                            // identify failures even when the future never returned.
                            if let Some(index) = self.cancellations.iter().position(|held| held.task == error.id()) {
                                let held = self.cancellations.swap_remove(index);
                                (held.failed)();
                            } else {
                                break Err("failed prompt task lost its custody record".into());
                            }
                        }
                        None => break Err("owned prompt task disappeared".into()),
                    }
                }
                job = self.receiver.recv() => {
                    let Some(job) = job else { break Ok(()); };
                    if self.cancellations.len() >= 2 * super::MAX_ACTIVE_EXECUTIONS {
                        (job.failed)();
                        continue;
                    }
                    let Job { execution, work, cancel, failed, capacity } = job;
                    let task = self.tasks.spawn(async move {
                        let _capacity = capacity;
                        work.await;
                        execution
                    });
                    self.cancellations.push(TaskCustody { task: task.id(), cancel, failed });
                }
                _ = async { if let Some(native) = &native { native.capacity_available(&self.capacity).await; } },
                    if native.as_ref().is_some_and(|native| native.capacity_waiting()) => {},
                result = async { match &mut mail_drive {
                    Some(drive) => drive.advance().await,
                    None => std::future::pending().await,
                } }, if native.is_some() => {
                    if let Err(reason) = result { break Err(reason); }
                },
            }
        };
        // Stop the startup auto-resume driver before closing admission.
        if let Some(task) = auto_resume_task {
            task.abort();
            let _ = task.await;
        }
        // Drop each session coordinator before joining native transport cleanup.
        drop(mail_drive);
        self.capacity.close();
        {
            let mut admission = match self.admission.lock() {
                Ok(held) => held,
                Err(poisoned) => poisoned.into_inner(),
            };
            *admission = Some(stopping);
        }
        self.receiver.close();
        // Offer the reason before terminal events ask clients to hydrate
        // history; that refresh can outlive the listener being stopped.
        if let Some(notice) = &self.shutdown_notice {
            notice(stopping);
        }
        // Pending inputs remain durable and unpromoted for an explicit resume.
        while let Ok(job) = self.receiver.try_recv() {
            (job.cancel)(stopping);
        }
        for held in &self.cancellations {
            (held.cancel)(stopping);
        }
        let drained = tokio::time::timeout(Duration::from_secs(10), async {
            while self.tasks.join_next().await.is_some() {}
        })
        .await;
        if drained.is_err() {
            self.tasks.abort_all();
            // Join forced abandonment before releasing service-owned native
            // context custody. A task drop is not an asynchronous close.
            while self.tasks.join_next().await.is_some() {}
            result = Err("prompt execution shutdown exceeded its drain allowance".into());
        }
        self.cancellations.clear();
        if let Some(mcp) = &self.mcp {
            if let Err(reason) = mcp.close_resource_subscriptions().await {
                result = Err(reason);
            }
        }
        if let Some(ends) = &self.session_ends {
            if let Err(reason) = ends.close().await {
                result = Err(reason);
            }
        }
        if let Some(native) = &native {
            if !matches!(
                tokio::time::timeout(Duration::from_secs(6), native.close()).await,
                Ok(Ok(()))
            ) {
                // Local executions were joined above. Keep the missing remote
                // receipt visible without abandoning listener/registration cleanup.
                crate::diagnostics::report(format!("service shutdown warning reason={} kind=native-inbox-close-unconfirmed local-executions-joined=true", stopping.as_str()));
            }
        }
        result
    }
}

/// One startup acknowledgement: the admitted input has an execution owner.
/// Every synchronously refusable custody dependency is resolved first, but
/// MCP startup and chain planning come after it, before provider I/O; their
/// failures are recorded as the execution's own.
#[derive(Debug, Clone)]
pub(super) struct Started(Arc<Mutex<Option<StartSender>>>);
/// The acknowledgement: `Ok(None)` an owned execution, `Ok(Some(reason))` an
/// admitted prompt whose turn was blocked and stays queued, `Err` a refusal.
type StartSender = oneshot::Sender<Result<Option<String>, String>>;

impl Started {
    pub(super) fn channel() -> (Self, oneshot::Receiver<Result<Option<String>, String>>) {
        let (sender, receiver) = oneshot::channel();
        (Self(Arc::new(Mutex::new(Some(sender)))), receiver)
    }
    pub(super) fn settle(&self, result: Result<(), String>) {
        self.send(result.map(|()| None));
    }
    /// Accept the prompt as queued: its turn was blocked before it began.
    pub(super) fn settle_queued(&self, reason: String) {
        self.send(Ok(Some(reason)));
    }
    fn send(&self, result: Result<Option<String>, String>) {
        let sender = match self.0.lock() {
            Ok(mut held) => held.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        if let Some(sender) = sender {
            let _dropped_http_receiver = sender.send(result);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execution() -> Result<ExecutionId, String> {
        ExecutionId::new("exe_driver").ok_or_else(|| "execution fixture identity is invalid".into())
    }

    #[test]
    fn reserved_capacity_covers_unstarted_jobs_and_recovers_after_release() -> Result<(), String> {
        let (mailbox, _driver) = PromptMailbox::new();
        let mut held = Vec::new();
        for _ in 0..64 {
            held.push(mailbox.reserve()?);
        }
        assert!(
            mailbox.reserve().is_err(),
            "65th reserved prompt exceeded the execution bound"
        );
        drop(held.pop());
        let _recovered = mailbox.reserve()?;
        assert!(
            mailbox.reserve().is_err(),
            "recovered capacity duplicated a permit"
        );
        Ok(())
    }

    #[tokio::test]
    async fn dropped_http_receiver_does_not_cancel_owned_execution() -> Result<(), String> {
        let (mailbox, mut driver) = PromptMailbox::new();
        let (started, http) = Started::channel();
        let (entered_tx, entered) = oneshot::channel();
        let (continue_tx, proceed) = oneshot::channel();
        let (finished_tx, finished) = oneshot::channel();
        let (stop_tx, stop) = oneshot::channel();
        mailbox.reserve()?.submit(
            execution()?,
            Box::pin(async move {
                started.settle(Ok(()));
                let _closed = entered_tx.send(());
                assert!(
                    proceed.await.is_ok(),
                    "execution was abandoned before continuation"
                );
                let _closed = finished_tx.send(());
            }),
            Arc::new(|| {}),
            Arc::new(|| {}),
        );
        drop(http);
        let (run, witness) = tokio::join!(
            driver.run_until(async {
                let _closed = stop.await;
            }),
            async {
                entered.await.map_err(|_| "execution never entered")?;
                continue_tx
                    .send(())
                    .map_err(|_| "HTTP disconnect cancelled execution")?;
                finished
                    .await
                    .map_err(|_| "owned execution never settled")?;
                stop_tx.send(()).map_err(|_| "driver ended early")?;
                Ok::<(), String>(())
            }
        );
        witness?;
        run?;
        assert!(
            mailbox.reserve().is_err(),
            "stopped owner still admits work"
        );
        Ok(())
    }

    #[tokio::test]
    async fn failed_jobs_release_custody_without_cancelling_a_healthy_neighbor(
    ) -> Result<(), String> {
        let (mailbox, mut driver) = PromptMailbox::new();
        let (entered_tx, entered) = oneshot::channel();
        let (release_tx, release) = oneshot::channel();
        let (finished_tx, finished) = oneshot::channel();
        let (stop_tx, stop) = oneshot::channel();
        let cancelled = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let cancelled_job = Arc::clone(&cancelled);
        mailbox.reserve()?.submit(
            execution()?,
            Box::pin(async move {
                let _closed = entered_tx.send(());
                assert!(release.await.is_ok(), "neighbor lost its continuation");
                let _closed = finished_tx.send(());
            }),
            Arc::new(move || {
                cancelled_job.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
            Arc::new(|| panic!("healthy neighbor was classified as failed")),
        );
        let (failed_tx, mut failures) = mpsc::channel(1);
        let failed_jobs_entered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let (run, witness) = tokio::join!(
            driver.run_until(async {
                let _closed = stop.await;
            }),
            async {
                entered.await.map_err(|_| "neighbor never entered")?;
                // More failures than the total custody bound expose retained
                // cancellation entries even when capacity permits are released.
                for _ in 0..=crate::dogfood::MAX_ACTIVE_EXECUTIONS {
                    let failed_tx = failed_tx.clone();
                    let entered_count = Arc::clone(&failed_jobs_entered);
                    mailbox.reserve()?.submit(
                        execution()?,
                        Box::pin(async move {
                            entered_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            panic!("named owned prompt failure mutation")
                        }),
                        Arc::new(|| {}),
                        Arc::new(move || {
                            assert!(failed_tx.try_send(()).is_ok(), "failure report was lost");
                        }),
                    );
                    tokio::time::timeout(Duration::from_secs(5), failures.recv())
                        .await
                        .map_err(|_| "failed task report timed out")?
                        .ok_or("failed task was not reported")?;
                    assert!(
                        !cancelled.load(std::sync::atomic::Ordering::SeqCst),
                        "failed task cancelled its healthy neighbor"
                    );
                }
                assert_eq!(
                    failed_jobs_entered.load(std::sync::atomic::Ordering::SeqCst),
                    crate::dogfood::MAX_ACTIVE_EXECUTIONS + 1,
                    "leaked cancellation records rejected work before it ran"
                );
                let mut held = Vec::new();
                for _ in 1..crate::dogfood::MAX_ACTIVE_EXECUTIONS {
                    held.push(mailbox.reserve()?);
                }
                assert!(
                    mailbox.reserve().is_err(),
                    "running neighbor lost its capacity permit"
                );
                drop(held);
                release_tx.send(()).map_err(|_| "neighbor was abandoned")?;
                finished.await.map_err(|_| "neighbor never completed")?;
                stop_tx
                    .send(())
                    .map_err(|_| "driver stopped after a failed task")?;
                Ok::<(), String>(())
            }
        );
        witness?;
        run?;
        assert!(
            driver.cancellations.is_empty(),
            "shutdown retained cancellation custody"
        );
        Ok(())
    }

    #[tokio::test]
    async fn shutdown_interrupts_and_joins_started_work() -> Result<(), String> {
        let (mailbox, mut driver) = PromptMailbox::new();
        let (cancel, mut cancelled) = tokio::sync::watch::channel(false);
        let (entered_tx, entered) = oneshot::channel();
        let (ended_tx, ended) = oneshot::channel();
        let (stop_tx, stop) = oneshot::channel();
        mailbox.reserve()?.submit(
            execution()?,
            Box::pin(async move {
                let _closed = entered_tx.send(());
                assert!(
                    cancelled.changed().await.is_ok(),
                    "shutdown dropped cancellation custody"
                );
                assert!(*cancelled.borrow());
                let _closed = ended_tx.send(());
            }),
            Arc::new(move || {
                let _closed = cancel.send(true);
            }),
            Arc::new(|| {}),
        );
        let (run, witness) = tokio::join!(
            driver.run_until(async {
                let _closed = stop.await;
            }),
            async {
                entered.await.map_err(|_| "job never started")?;
                stop_tx.send(()).map_err(|_| "driver ended early")?;
                ended
                    .await
                    .map_err(|_| "shutdown did not join cancelled work")?;
                Ok::<(), String>(())
            }
        );
        witness?;
        run
    }
}
