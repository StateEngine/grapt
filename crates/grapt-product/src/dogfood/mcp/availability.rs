//! Continuity distinguishes an unavailable door from a healthy wrong contract.
use super::{McpRegistry, ServerEntry, StartPhase};
use grapt_mcp::session::Unusable;
use grapt_session::execution::StateEngineServerFailure as Failure;

impl McpRegistry {
    /// A turn owns its registry location through provider work and local tools.
    /// Weak retention needs no async cleanup and cannot outlive the turn drain.
    pub(crate) async fn hold_location(&self, location: &str) -> Option<std::sync::Arc<()>> {
        let mut locations = self.entries.lock().await;
        if !super::release_over_bound(
            &mut locations,
            location,
            self.retained_locations,
            self.clock.now().as_millis(),
        ) {
            return None;
        }
        let held = locations.entry(location.to_owned()).or_default();
        held.last_read = super::next_read_ordinal();
        held.last_read_ms = self.clock.now().as_millis();
        let token = held
            .active
            .upgrade()
            .unwrap_or_else(|| std::sync::Arc::new(()));
        held.active = std::sync::Arc::downgrade(&token);
        Some(token)
    }

    /// Required continuity may recover a timed-out initial handshake once,
    /// before its catalog snapshot is frozen for provider preparation.
    #[cfg(test)]
    pub(crate) async fn with_continuity_status<T>(
        &self,
        location: &str,
        declared: &std::collections::BTreeMap<String, super::Declaration>,
        producer: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
        read: impl FnOnce(&std::collections::BTreeMap<String, ServerEntry>) -> T,
    ) -> T {
        self.with_continuity_status_at(location, location, declared, producer, read)
            .await
    }

    pub(crate) async fn with_continuity_status_at<T>(
        &self,
        key: &str,
        directory: &str,
        declared: &std::collections::BTreeMap<String, super::Declaration>,
        producer: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
        read: impl FnOnce(&std::collections::BTreeMap<String, ServerEntry>) -> T,
    ) -> T {
        super::admission::with_status(
            self,
            key,
            directory,
            declared,
            true,
            producer.map(std::sync::Arc::downgrade),
            read,
        )
        .await
    }

    /// Re-admit an unsent call from a fresh configuration read, never its old
    /// approval snapshot. Return excluded human time for the eventual dispatch.
    /// Runtime declarations retain their usual precedence.
    pub(crate) async fn readmit_missing(
        &self,
        location: &str,
        directory: &str,
        server: &str,
        approved: &super::LaunchIdentity,
        declared: &std::collections::BTreeMap<String, super::Declaration>,
        budget: std::time::Duration,
    ) -> Result<std::time::Duration, super::McpCallRefusal> {
        use super::McpCallRefusal as E;
        let started = tokio::time::Instant::now();
        let human_wait = grapt_mcp_client::HumanWait::default();
        let deadline = human_wait.deadline(budget).map_err(|_| E::TimedOut)?;
        let inspect =
            |entries: &std::collections::BTreeMap<String, ServerEntry>| match entries.get(server) {
                Some(ServerEntry::Live(live)) if live.launch().matches_approval(approved) => Ok(()),
                Some(ServerEntry::Live(live)) => Err(E::ConnectionReplaced {
                    approved: super::describe_launch(approved),
                    current: super::describe_launch(live.launch()),
                }),
                Some(ServerEntry::Disabled) => Err(E::NotConnected(Unusable::Disabled)),
                Some(ServerEntry::Unusable(reason, _, _)) => Err(E::NotConnected(reason.clone())),
                None => Err(E::Busy {
                    waiting: self.retained_locations,
                }),
            };
        let recover = async {
            let runtime = self.runtime.lock().await;
            let current = runtime
                .get(location)
                .and_then(|servers| servers.get(server))
                .or_else(|| declared.get(server));
            if current != Some(approved.declaration.as_ref()) {
                return Err(E::ConnectionReplaced {
                    approved: super::describe_launch(approved),
                    current: "server configuration changed before re-admission".into(),
                });
            }
            drop(runtime);
            super::admission::with_status_with_wait(
                self,
                location,
                declared,
                false,
                None,
                super::admission::StartupSelection {
                    directory,
                    server: Some(server),
                    human_wait: &human_wait,
                },
                inspect,
            )
            .await
        };
        let result = deadline.timeout(recover).await.map_err(|_| E::TimedOut)?;
        let mut human_paused = human_wait.paused_elapsed();
        if matches!(result, Err(E::Busy { .. })) {
            // A catalog may have reserved this server after the route was
            // evicted. Join that exact reservation, not its catalog-wide clock.
            let joining = deadline
                .timeout(async {
                    self.entries.lock().await.get(location).is_some_and(|held| {
                        held.starting
                            .get(server)
                            .is_some_and(|token| token.strong_count() != 0)
                    })
                })
                .await
                .map_err(|_| E::TimedOut)?;
            if joining {
                human_paused = human_paused.saturating_add(
                    super::admission::recover_queued(
                        self,
                        location,
                        server,
                        approved,
                        budget.saturating_sub(started.elapsed().saturating_sub(human_paused)),
                    )
                    .await?,
                );
                let locations = tokio::time::timeout(
                    budget.saturating_sub(started.elapsed().saturating_sub(human_paused)),
                    self.entries.lock(),
                )
                .await
                .map_err(|_| E::TimedOut)?;
                inspect(
                    &locations
                        .get(location)
                        .ok_or_else(|| E::NoSuchLocation(location.into()))?
                        .entries,
                )?;
                return Ok(human_paused);
            }
        }
        result?;
        Ok(human_paused)
    }

    pub(crate) async fn server_unavailable(
        &self,
        location: &str,
        server: &str,
    ) -> Option<(Failure, String)> {
        let locations = self.entries.lock().await;
        let Some(held) = locations.get(location) else {
            return Some((
                Failure::NotConfigured,
                Failure::NotConfigured.reason().into(),
            ));
        };
        let Some(entry) = held.entries.get(server) else {
            let reason = if held.declarations.contains_key(server) {
                Failure::Starting
            } else {
                Failure::NotConfigured
            };
            return Some((reason, reason.reason().into()));
        };
        let reason = match entry {
            ServerEntry::Disabled => Failure::Disabled,
            ServerEntry::Live(live) => {
                if live.observed_unusable().is_none() && !live.io.is_retired() {
                    return None;
                }
                Failure::Disconnected
            }
            ServerEntry::Unusable(reason, phase, _) => match (reason, phase) {
                (Unusable::Disabled, _) => Failure::Disabled,
                (Unusable::Disconnected, _) => Failure::Disconnected,
                (Unusable::TimedOut, Some(StartPhase::Handshake)) => Failure::HandshakeTimeout,
                (Unusable::TimedOut, Some(StartPhase::Discovery)) => Failure::DiscoveryTimeout,
                (Unusable::TimedOut, _) => Failure::TimedOut,
                (Unusable::Failed(_), _) => Failure::Failed,
            },
        };
        let detail = if matches!(
            held.declarations.get(server),
            Some(super::Declaration::Local { .. })
        ) {
            format!("{}; {}", reason.reason(), entry.detail())
        } else {
            reason.reason().to_owned()
        };
        Some((reason, detail))
    }
}
