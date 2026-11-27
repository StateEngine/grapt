//! One admitted startup's lifetime, independent of the registry mutex.
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Debug)]
pub struct StartupLease {
    retired: AtomicBool,
    changed: tokio::sync::Notify,
    human_wait: grapt_mcp_client::HumanWait,
    server_human_wait: grapt_mcp_client::HumanWait,
}

impl StartupLease {
    pub fn new() -> Self {
        Self::with_human_wait(grapt_mcp_client::HumanWait::default())
    }

    pub fn with_human_wait(human_wait: grapt_mcp_client::HumanWait) -> Self {
        Self {
            retired: AtomicBool::new(false),
            changed: tokio::sync::Notify::new(),
            human_wait,
            server_human_wait: grapt_mcp_client::HumanWait::default(),
        }
    }

    pub fn human_wait(&self) -> &grapt_mcp_client::HumanWait {
        &self.human_wait
    }

    /// Recovery of this server cannot inherit another reservation's pause.
    /// The catalog's shared clock still bounds the whole admission round.
    pub fn server_human_wait(&self) -> &grapt_mcp_client::HumanWait {
        &self.server_human_wait
    }

    pub fn retire(&self) {
        self.retired.store(true, Ordering::Release);
        // There is exactly one connection future per reservation. A retained
        // permit also closes retirement-before-first-poll without another task.
        self.changed.notify_one();
    }

    pub fn is_retired(&self) -> bool {
        self.retired.load(Ordering::Acquire)
    }

    pub async fn retired(&self) {
        if !self.is_retired() {
            self.changed.notified().await;
        }
    }
}

impl Default for StartupLease {
    fn default() -> Self {
        Self::new()
    }
}
