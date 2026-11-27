//! Reserve startup outside the registry lock, then fence its installation.
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};
use std::time::Duration;

use super::retry::{Backoff, ROUND_BUDGET};
use super::{next_read_ordinal, release_over_bound, retire, Declaration, McpRegistry, ServerEntry};

pub(crate) struct StartupSelection<'a> {
    pub directory: &'a str,
    pub server: Option<&'a str>,
    pub human_wait: &'a grapt_mcp_client::HumanWait,
}

/// Refresh healthy connections and retry failed local starts. Dropping this
/// future also drops its reservation tokens: the next refresh can proceed
/// without an async destructor or a detached cleanup task.
pub(crate) async fn with_status<T>(
    registry: &McpRegistry,
    location: &str,
    directory: &str,
    declared: &BTreeMap<String, Declaration>,
    required_continuity: bool,
    producer: Option<Weak<std::sync::atomic::AtomicBool>>,
    read: impl FnOnce(&BTreeMap<String, ServerEntry>) -> T,
) -> T {
    let human_wait = grapt_mcp_client::HumanWait::default();
    with_status_with_wait(
        registry,
        location,
        declared,
        required_continuity,
        producer,
        StartupSelection {
            directory,
            server: None,
            human_wait: &human_wait,
        },
        read,
    )
    .await
}

/// A re-admitted call supplies its active clock and starts only its own server.
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) async fn with_status_with_wait<T>(
    registry: &McpRegistry,
    location: &str,
    declared: &BTreeMap<String, Declaration>,
    required_continuity: bool,
    producer: Option<Weak<std::sync::atomic::AtomicBool>>,
    startup: StartupSelection<'_>,
    read: impl FnOnce(&BTreeMap<String, ServerEntry>) -> T,
) -> T {
    let StartupSelection {
        directory,
        server: only_server,
        human_wait,
    } = startup;
    let _notify = NotifyOnDrop(&registry.changed);
    // All attempts reserved by this catalog read share one finite human
    // allowance and one pause signal observed by enclosing active deadlines.
    let _joined = only_server.map(|_| super::nested_startup::join(human_wait));
    let runtime = registry
        .runtime
        .lock()
        .await
        .get(location)
        .cloned()
        .unwrap_or_default();
    let (incarnation, attempts) = {
        let mut locations = registry.entries.lock().await;
        if !release_over_bound(
            &mut locations,
            location,
            registry.retained_locations,
            registry.clock.now().as_millis(),
        ) {
            let unavailable = declared
                .keys()
                .chain(runtime.keys())
                .map(|name| {
                    (
                        name.clone(),
                        ServerEntry::unusable(grapt_mcp::session::Unusable::Failed(
                            "MCP location capacity reached".into(),
                        )),
                    )
                })
                .collect();
            return read(&unavailable);
        }
        let held = locations.entry(location.to_owned()).or_default();
        let same_placement = held.directory.is_empty() || held.directory == directory;
        held.directory = directory.to_owned();
        held.last_read = next_read_ordinal();
        held.last_read_ms = registry.clock.now().as_millis();
        if held.incarnation == 0 {
            held.incarnation = next_read_ordinal();
        }
        let mut desired = declared.clone();
        desired.extend(runtime);
        let declarations = &held.declarations;
        let unchanged = |name: &String| {
            same_placement
                && declarations
                    .get(name)
                    .is_some_and(|old| desired.get(name) == Some(old))
        };
        held.entries.retain(|name, entry| {
            let keep = unchanged(name);
            if !keep {
                retire(entry);
            }
            keep
        });
        held.starting.retain(|name, token| {
            let keep = unchanged(name) && token.strong_count() != 0;
            if !keep {
                if let Some(token) = token.upgrade() {
                    token.retire();
                }
            }
            keep
        });
        held.backoff.retain(|name, _| unchanged(name));
        // This is the latest declaration snapshot, including starts still in
        // flight. A later refresh can withdraw or replace a reserved name.
        if desired
            .get("stateengine")
            .is_none_or(|declaration| matches!(declaration, Declaration::Disabled))
        {
            held.bound_producer = None;
        } else if required_continuity
            && producer
                .as_ref()
                .and_then(Weak::upgrade)
                .is_some_and(|token| token.load(std::sync::atomic::Ordering::Acquire))
        {
            // An older refresh may arrive after its same-key successor. An
            // invalidated token cannot replace current producer ownership.
            held.bound_producer = producer;
        }
        held.declarations = desired.clone();
        let now = registry.clock.now().as_millis();
        let mut attempts = Vec::new();
        for (name, declaration) in desired {
            if only_server.is_some_and(|server| server != name.as_str()) {
                continue;
            }
            if held.starting.contains_key(&name) {
                continue;
            }
            let retry = matches!(declaration, Declaration::Local { .. })
                && held.entries.get(&name).is_some_and(retryable);
            if held.entries.contains_key(&name) && !retry {
                continue;
            }
            if retry && !held.backoff.contains_key(&name) {
                held.backoff
                    .insert(name.clone(), Backoff::failed(None, now));
            }
            let token = Arc::new(super::startup_lease::StartupLease::with_human_wait(
                human_wait.clone(),
            ));
            held.starting.insert(name.clone(), Arc::downgrade(&token));
            attempts.push(Attempt {
                name,
                declaration,
                token,
                retry,
                directory: directory.to_owned(),
            });
        }
        (held.incarnation, attempts)
    };
    let (retries, initial): (Vec<_>, Vec<_>) =
        attempts.into_iter().partition(|attempt| attempt.retry);
    // New declarations retain their separate startup and catalog budgets.
    // They are never delayed by recovery of previously failed connections.
    for attempt in initial {
        let retry_handshake = required_continuity
            && attempt.name == "stateengine"
            && matches!(attempt.declaration, Declaration::Local { .. });
        connect_attempt(registry, location, incarnation, attempt, retry_handshake).await;
    }
    // Bound active recovery work, including backoff and installation, while
    // honoring the channel's separately bounded human allowance.
    let _ = async {
        human_wait
            .deadline(ROUND_BUDGET)?
            .timeout(async {
                for attempt in retries {
                    connect_attempt(registry, location, incarnation, attempt, false).await;
                }
            })
            .await
    }
    .await;
    let locations = registry.entries.lock().await;
    match locations.get(location) {
        Some(held) => read(&held.entries),
        None => read(&BTreeMap::new()),
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn connect_attempt(
    registry: &McpRegistry,
    location: &str,
    incarnation: u64,
    attempt: Attempt,
    retry_handshake: bool,
) {
    let delay = {
        let locations = registry.entries.lock().await;
        locations
            .get(location)
            .filter(|held| current(held, incarnation, &attempt))
            .map(|held| {
                held.backoff
                    .get(&attempt.name)
                    .map_or(Duration::ZERO, |backoff| {
                        backoff.remaining(registry.clock.now().as_millis())
                    })
            })
    };
    let Some(delay) = delay else {
        return;
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    {
        let mut locations = registry.entries.lock().await;
        let Some(held) = locations
            .get_mut(location)
            .filter(|held| current(held, incarnation, &attempt))
        else {
            return;
        };
        // Retire a dead child before trying to admit its replacement. Keep
        // its failure visible, without retaining its connection slot.
        if let Some(ServerEntry::Live(live)) = held.entries.get(&attempt.name) {
            let reason = live
                .observed_unusable()
                .unwrap_or(grapt_mcp::session::Unusable::Disconnected);
            let previous = held
                .entries
                .insert(attempt.name.clone(), ServerEntry::unusable(reason));
            if let Some(previous) = previous {
                retire(&previous);
            }
        }
    }
    let Some(mut entry) = connect_reserved(registry, location, &attempt).await else {
        return;
    };
    if retry_handshake
        && matches!(
            entry,
            ServerEntry::Unusable(
                grapt_mcp::session::Unusable::TimedOut,
                Some(super::StartPhase::Handshake),
                _
            )
        )
    {
        // Initialization has no admitted tool effect to replay. Keep this
        // exact reservation across one bounded retry; a concurrent refresh
        // cannot start another child, and a replacement invalidates our token.
        let retried = async {
            attempt
                .token
                .human_wait()
                .deadline(ROUND_BUDGET)?
                .timeout(async {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    {
                        let locations = registry.entries.lock().await;
                        if !locations
                            .get(location)
                            .is_some_and(|held| current(held, incarnation, &attempt))
                        {
                            return None;
                        }
                    }
                    connect_reserved(registry, location, &attempt).await
                })
                .await
        }
        .await;
        match retried {
            Ok(Some(replacement)) => entry = replacement,
            Ok(None) => {
                retire(&entry);
                return;
            }
            // The first failure remains accurate when the bounded retry ends
            // without a new observation. Dropping connect reaps its child.
            Err(_) => {}
        }
    }
    let mut locations = registry.entries.lock().await;
    let Some(held) = locations
        .get_mut(location)
        .filter(|held| current(held, incarnation, &attempt))
    else {
        retire(&entry);
        return;
    };
    held.starting.remove(&attempt.name);
    if retryable(&entry) && matches!(attempt.declaration, Declaration::Local { .. }) {
        let previous = held.backoff.get(&attempt.name).copied();
        held.backoff.insert(
            attempt.name.clone(),
            Backoff::failed(previous, registry.clock.now().as_millis()),
        );
    } else {
        held.backoff.remove(&attempt.name);
    }
    if let Some(previous) = held.entries.insert(attempt.name, entry) {
        retire(&previous);
    }
}

struct Attempt {
    name: String,
    declaration: Declaration,
    token: Arc<super::startup_lease::StartupLease>,
    retry: bool,
    directory: String,
}

async fn connect_reserved(
    registry: &McpRegistry,
    location: &str,
    attempt: &Attempt,
) -> Option<ServerEntry> {
    tokio::select! {
        biased;
        _ = attempt.token.retired() => None,
        entry = registry.connect(
            location, &attempt.directory, &attempt.name, &attempt.declaration,
            Arc::clone(&attempt.token),
        ) => Some(entry),
    }
}

fn current(held: &super::LocationConnections, incarnation: u64, attempt: &Attempt) -> bool {
    held.incarnation == incarnation
        && held.declarations.get(&attempt.name) == Some(&attempt.declaration)
        && held
            .starting
            .get(&attempt.name)
            .and_then(Weak::upgrade)
            .is_some_and(|token| Arc::ptr_eq(&token, &attempt.token))
}

fn retryable(entry: &ServerEntry) -> bool {
    use grapt_mcp::session::Unusable;
    match entry {
        ServerEntry::Unusable(Unusable::Disabled, _, _) | ServerEntry::Disabled => false,
        ServerEntry::Unusable(_, _, _) => true,
        ServerEntry::Live(live) => live.observed_unusable().is_some() || live.io.is_retired(),
    }
}

// Wake queued calls on installation or canceled startup; the weak reservation
// still decides which caller may connect. No detached waiter owns the registry.
struct NotifyOnDrop<'a>(&'a tokio::sync::Notify);
impl Drop for NotifyOnDrop<'_> {
    fn drop(&mut self) {
        self.0.notify_waiters();
    }
}

/// Only an unsent call may enter recovery. Reserve the currently approved
/// declaration through ordinary admission, within the original active call
/// deadline. Return excluded human time so the caller does not charge it again.
pub(crate) async fn recover_queued(
    registry: &McpRegistry,
    location: &str,
    server: &str,
    approved: &super::LaunchIdentity,
    budget: Duration,
) -> Result<Duration, super::McpCallRefusal> {
    use super::{McpCallRefusal as E, ServerEntry};
    // This guard outlives the recovery future and its reservation token, so
    // cancellation also wakes callers that were joining our startup.
    let _notify = NotifyOnDrop(&registry.changed);
    let started = tokio::time::Instant::now();
    let mut human_paused = Duration::ZERO;
    let mut attempted = false;
    loop {
        let notified = registry.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let (attempt, human_wait) = {
            let remaining = budget.saturating_sub(started.elapsed().saturating_sub(human_paused));
            let mut locations = tokio::time::timeout(remaining, registry.entries.lock())
                .await
                .map_err(|_| E::TimedOut)?;
            let held = locations
                .get_mut(location)
                .ok_or_else(|| E::NoSuchLocation(location.to_owned()))?;
            if held.declarations.get(server) != Some(approved.declaration.as_ref()) {
                return Err(E::ConnectionReplaced {
                    approved: super::describe_launch(approved),
                    current: "server declaration changed or retired while queued".to_owned(),
                });
            }
            if let Some(ServerEntry::Live(live)) = held.entries.get(server) {
                if !live.io.is_retired() && live.observed_unusable().is_none() {
                    return Ok(human_paused);
                }
            }
            if let Some(token) = held.starting.get(server).and_then(Weak::upgrade) {
                // Observing a pause must not keep a weak startup reservation
                // alive after its actual connection owner has been dropped.
                (None, token.server_human_wait().clone())
            } else {
                // Never republish a cloned configuration snapshot: another
                // refresh may have withdrawn it while this call waited.
                if attempted || !matches!(approved.declaration.as_ref(), Declaration::Local { .. })
                {
                    return Err(E::NotConnected(grapt_mcp::session::Unusable::Disconnected));
                }
                let token = Arc::new(super::startup_lease::StartupLease::new());
                let human_wait = token.server_human_wait().clone();
                held.starting
                    .insert(server.to_owned(), Arc::downgrade(&token));
                (
                    Some((
                        held.incarnation,
                        Attempt {
                            name: server.to_owned(),
                            declaration: approved.declaration.as_ref().clone(),
                            token,
                            retry: true,
                            directory: held.directory.clone(),
                        },
                    )),
                    human_wait,
                )
            }
        };
        let remaining = budget.saturating_sub(started.elapsed().saturating_sub(human_paused));
        let paused_before = human_wait.paused_elapsed();
        let _joined = super::nested_startup::join(&human_wait);
        human_wait
            .deadline(remaining)
            .map_err(|_| E::TimedOut)?
            .timeout(async {
                if let Some((incarnation, attempt)) = attempt {
                    attempted = true;
                    connect_attempt(registry, location, incarnation, attempt, false).await;
                } else {
                    notified.await;
                }
            })
            .await
            .map_err(|_| E::TimedOut)?;
        human_paused =
            human_paused.saturating_add(human_wait.paused_elapsed().saturating_sub(paused_before));
    }
}
