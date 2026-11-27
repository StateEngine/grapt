//! One bounded source for input-wait reasons and the runtime watchdog count.
use std::sync::{Mutex, MutexGuard};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy)]
pub(in crate::dogfood) enum Kind {
    Permission,
    Question,
}

#[derive(Debug, Default)]
struct Counts {
    permissions: u32,
    questions: u32,
}

#[derive(Debug)]
pub(in crate::dogfood) struct Waits {
    counts: Mutex<Counts>,
    total: watch::Sender<u32>,
}

impl Default for Waits {
    fn default() -> Self {
        Self {
            counts: Mutex::default(),
            total: watch::channel(0).0,
        }
    }
}

impl Waits {
    fn lock(&self) -> MutexGuard<'_, Counts> {
        match self.counts.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(in crate::dogfood) fn subscribe(&self) -> watch::Receiver<u32> {
        self.total.subscribe()
    }

    pub(in crate::dogfood) fn pending(&self) -> Option<Kind> {
        let counts = self.lock();
        if counts.questions > 0 {
            Some(Kind::Question)
        } else if counts.permissions > 0 {
            Some(Kind::Permission)
        } else {
            None
        }
    }

    pub(in crate::dogfood) fn begin(&self, kind: Kind) -> Result<(), String> {
        let mut counts = self.lock();
        let total = counts
            .permissions
            .checked_add(counts.questions)
            .ok_or("human input wait count overflow")?;
        if total >= grapt_permission::MAX_PENDING_TOOL_PERMISSIONS as u32 {
            return Err("human input wait bound exhausted".into());
        }
        match kind {
            Kind::Permission => counts.permissions += 1,
            Kind::Question => counts.questions += 1,
        }
        self.total.send_replace(total + 1);
        Ok(())
    }

    pub(in crate::dogfood) fn end(&self, kind: Kind) -> Result<(), String> {
        let mut counts = self.lock();
        let count = match kind {
            Kind::Permission => &mut counts.permissions,
            Kind::Question => &mut counts.questions,
        };
        *count = count
            .checked_sub(1)
            .ok_or("human input wait was released twice")?;
        let total = counts
            .permissions
            .checked_add(counts.questions)
            .ok_or("human input wait count overflow")?;
        self.total.send_replace(total);
        Ok(())
    }
}
