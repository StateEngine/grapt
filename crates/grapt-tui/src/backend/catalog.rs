//! Keep catalog-gated activity queued through quiet streams and failed reads.
use crate::bridge::Bridge;
use grapt_client::{Client, ClientError, PeriodicRead};
use std::{sync::atomic::Ordering, sync::mpsc::SyncSender, time::Duration};
use tokio::time::Instant;

const RETRY_DELAY: Duration = Duration::from_secs(1);

#[derive(Debug, Default)]
pub(crate) struct CatalogRefresh {
    retry: Option<Instant>,
}

impl CatalogRefresh {
    pub fn wait(&self, now: Instant) -> Option<Duration> {
        self.retry.map(|due| due.saturating_duration_since(now))
    }

    pub(super) fn refresh(
        &mut self,
        client: &mut Client<Bridge>,
        reads: &mut super::reads::Reads,
        now: Instant,
    ) -> Result<bool, ClientError> {
        // A pending resume, or a read already running, leaves the flag raised.
        if client.resume_pending()
            || reads.busy(PeriodicRead::Sessions)
            || self.retry.is_some_and(|due| now < due)
            || !client
                .peer_mut()
                .sessions_dirty
                .swap(false, Ordering::AcqRel)
        {
            return Ok(false);
        }
        // A read in flight owns the refresh: no past-due retry stays armed.
        self.retry = None;
        match reads.begin(client, PeriodicRead::Sessions) {
            Ok(_) => Ok(false),
            Err(error) => {
                client.peer_mut().catalog_refreshed(false);
                self.failed(client, error, now)
            }
        }
    }

    /// The catalog read's answer arrived and was applied, or failed.
    pub(super) fn settled(
        &mut self,
        client: &mut Client<Bridge>,
        result: Result<bool, ClientError>,
        now: Instant,
        wake: &SyncSender<()>,
    ) -> Result<bool, ClientError> {
        client
            .peer_mut()
            .catalog_refreshed(matches!(result, Ok(true)));
        match result {
            // Discarded for its scope: raise the flag to ask again.
            Ok(false) => {
                client
                    .peer_mut()
                    .sessions_dirty
                    .store(true, Ordering::Release);
                Ok(false)
            }
            Ok(true) => {
                self.retry = None;
                // The only event may already be retained inside Bridge. Its
                // original reader wake was consumed before this catalog read.
                let _ = wake.try_send(());
                Ok(true)
            }
            Err(error) => self.failed(client, error, now),
        }
    }

    /// The catalog read run inline through the loop's own prepare, accept and
    /// settle steps, so a test drives this retry owner without the worker.
    #[cfg(test)]
    pub(crate) fn refresh_inline(
        &mut self,
        client: &mut Client<Bridge>,
        now: Instant,
        wake: &SyncSender<()>,
    ) -> Result<bool, ClientError> {
        use grapt_client::Transport;
        if client.resume_pending()
            || self.retry.is_some_and(|due| now < due)
            || !client
                .peer_mut()
                .sessions_dirty
                .swap(false, Ordering::AcqRel)
        {
            return Ok(false);
        }
        let applied =
            client
                .prepare_read(PeriodicRead::Sessions)
                .and_then(|prepared| match prepared {
                    Some(prepared) => {
                        let response = client.peer_mut().call(&prepared.request);
                        client.accept_read(prepared, response)
                    }
                    None => Ok(true),
                });
        self.settled(client, applied, now, wake)
    }

    fn failed(
        &mut self,
        client: &mut Client<Bridge>,
        error: ClientError,
        now: Instant,
    ) -> Result<bool, ClientError> {
        client
            .peer_mut()
            .sessions_dirty
            .store(true, Ordering::Release);
        self.retry = Some(now + RETRY_DELAY);
        Err(error)
    }
}
