//! Root Session hook attachments end with the existing service-owned driver.
//! This bounded registry retains hook snapshots, not execution or Session state.
use super::{Event, HookRuntime};
use grapt_types::SessionId;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const MAX_ATTACHMENTS: usize = 256;
const CLOSE_ALLOWANCE: Duration = Duration::from_secs(5);

#[derive(Debug, Default)]
pub(in crate::dogfood) struct SessionEnds {
    attached: Mutex<BTreeMap<SessionId, Arc<HookRuntime>>>,
}

impl SessionEnds {
    pub(in crate::dogfood) fn remember(
        &self,
        session: &SessionId,
        runtime: &Arc<HookRuntime>,
    ) -> Result<(), String> {
        let mut held = self
            .attached
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        // The latest attached configuration also removes a revoked hook.
        if runtime.settings.commands(Event::SessionEnd).is_empty() {
            held.remove(session);
            return Ok(());
        }
        if held.len() >= MAX_ATTACHMENTS && !held.contains_key(session) {
            return Err("SessionEnd attachment limit of 256 reached; restart the service".into());
        }
        held.insert(session.clone(), Arc::clone(runtime));
        Ok(())
    }

    pub(in crate::dogfood) fn forget(&self, session: &SessionId) {
        self.attached
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(session);
    }

    pub(in crate::dogfood) async fn close(&self) -> Result<(), String> {
        let attached = {
            let mut held = self
                .attached
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            std::mem::take(&mut *held)
        };
        tokio::time::timeout(CLOSE_ALLOWANCE, async {
            let mut failure = None;
            for (session, runtime) in attached {
                if let Err(reason) = runtime.fire(Event::SessionEnd, &session, None).await {
                    failure.get_or_insert(reason);
                }
            }
            failure.map_or(Ok(()), Err)
        })
        .await
        .map_err(|_| "SessionEnd hooks exceeded the service's five-second allowance")?
    }
}
