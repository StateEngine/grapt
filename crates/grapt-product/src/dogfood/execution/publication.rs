//! What a Session's configured MCP servers publish to a model.
//!
//! Split out of `execution.rs` under the workspace god-file ceiling. It sits
//! beside `registry_location` deliberately: publication and dispatch must key
//! the registry identically, and keeping the derivation next to its only
//! non-dispatch caller is what makes a divergence visible.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::{Arc, Mutex};

use grapt_session::notice::Unavailable;
use grapt_types::SessionId;

use super::super::{connections, mcp, session_config, tools};
use super::{registry_location, DeterministicAuthorities, LiveExecutor, SessionPorts};

/// The notice prefixes before a source refusal; each fits
/// catalog_config::NOTICE_PREFIX_BYTES, so the refusal is never cut.
const PLUGINS_REFUSED: &str = "they could not be refreshed: ";
const CONFIG_UNREADABLE: &str = "it could not be read: ";
const CONFIG_REFUSED: &str = "it was refused: ";

#[derive(Debug)]
pub(in crate::dogfood) struct PublicationLedger {
    refusals: Mutex<HashMap<SessionId, String>>,
}

impl PublicationLedger {
    pub(in crate::dogfood) fn new() -> Self {
        Self {
            refusals: Mutex::new(HashMap::new()),
        }
    }
    pub(in crate::dogfood) fn refuse(&self, session: SessionId, reason: String) {
        let mut refusals = match self.refusals.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        if refusals.contains_key(&session) || refusals.len() < super::MAX_PUBLICATION_REFUSALS {
            refusals.entry(session).or_insert(reason);
        }
    }
    pub(in crate::dogfood) fn take(&self, session: &SessionId) -> Option<String> {
        match self.refusals.lock() {
            Ok(mut guard) => guard.remove(session),
            Err(poisoned) => poisoned.into_inner().remove(session),
        }
    }
}

impl LiveExecutor {
    pub(in crate::dogfood) fn invalidate_config(&self, session: &SessionId) {
        self.published.clear(session);
    }
    /// Take one bounded snapshot of what the session's configured MCP servers
    /// publish to a model.
    ///
    /// A configuration this cannot read, or a server that will not start,
    /// yields NO tools rather than a refusal. A broken MCP declaration must not
    /// stop a user chatting, and the connection endpoint already reports the
    /// real state of every declared server with its reason — so the fact is
    /// available where an operator looks for it, rather than surfacing as a
    /// failed prompt with an unrelated message.
    ///
    /// Routes describe this refresh only. SC-CACHE-02 keeps the advertised
    /// definitions in the Session's instruction journal; failed routes cannot
    /// replace that prefix or grant a stale launch any authority.
    ///
    /// Returns the MCP sources this turn cannot use, each with a
    /// product-authored reason, or `None` when that could not be read at all.
    pub(super) async fn refresh_published_tools(
        &self,
        session: &SessionId,
    ) -> Option<Vec<Unavailable>> {
        if self.location.ephemeral {
            // Never ensure MCP or refresh a plugin generation in this mode.
            // Configuration errors also refuse open_chain at its preflight.
            self.published.clear(session);
            return self
                .location
                .preflight(session)
                .err()
                .map(|reason| vec![Unavailable::new("ephemeral integrations", &reason)]);
        }
        let revision = self
            .inbox
            .lock()
            .ok()
            .and_then(|inbox| session_config::revision(&inbox, session).ok());
        let Some(revision) = revision else {
            return Some(vec![Unavailable::new(
                "MCP configuration",
                "Session config cannot be read",
            )]);
        };
        let (published, unavailable) = self.published_tools_for(session, revision).await;
        let accepted = session_config::publish_current(&self.inbox, session, revision, || {
            self.published.set(session, Arc::new(published))
        });
        if let Err(reason) = accepted {
            return Some(vec![Unavailable::new("MCP configuration", &reason)]);
        }
        unavailable
    }

    #[expect(clippy::too_many_lines, reason = "legacy")]
    async fn published_tools_for(
        &self,
        session: &SessionId,
        revision: Option<u64>,
    ) -> (mcp::PublishedTools, Option<Vec<Unavailable>>) {
        // Plugins refresh first, over the service's own sources: one they
        // cannot read is named here, as the MCP read below never sees it.
        if let Err(refusal) = self.plugins.refresh().await {
            let refused = Unavailable::new("plugins", &format!("{PLUGINS_REFUSED}{refusal}"));
            return (
                mcp::PublishedTools {
                    refresh_failed: true,
                    ..Default::default()
                },
                Some(vec![refused]),
            );
        }
        // Everything that reads configuration is done and DROPPED before the
        // await. `with_status` may start a child process, so this future
        // crosses threads, and holding a session record or a config source
        // across that point would make it unable to.
        let roots_revision = match self.mcp.roots_revision() {
            Ok(revision) => revision,
            Err(reason) => {
                return (
                    mcp::PublishedTools {
                        refresh_failed: true,
                        ..Default::default()
                    },
                    Some(vec![Unavailable::new("MCP roots", &reason)]),
                )
            }
        };
        let (
            location,
            directory,
            declared,
            roots,
            codemode_servers,
            blocked,
            required_continuity,
            producer,
        ) = {
            let authorities = DeterministicAuthorities::new(&*self.clock, &*self.ids);
            let Ok(record) = SessionPorts::new(Arc::clone(&self.inbox), &authorities).get(session)
            else {
                return (
                    mcp::PublishedTools {
                        refresh_failed: true,
                        ..Default::default()
                    },
                    None,
                );
            };
            let required_continuity = self.inbox.lock().ok().and_then(|inbox| {
                if session_config::revision(&inbox, session).ok() != Some(revision) {
                    return None;
                }
                let required =
                    super::super::stateengine_resume::is_required(&inbox, session).ok()?;
                let custody = self.location.session_capabilities.lock().ok()?;
                Some((required, custody.producer_bindings.get(session).cloned()))
            });
            let Some((required_continuity, producer)) = required_continuity else {
                return (
                    mcp::PublishedTools {
                        refresh_failed: true,
                        ..Default::default()
                    },
                    None,
                );
            };
            let directory = registry_location(&record.placement);
            // A source that cannot be read refuses, as every other reader
            // does: publishing nothing silently looked like no MCP at all.
            let sources = session_config::sources(
                &self.inbox,
                session,
                &self.location.service_root,
                Path::new(&directory),
            );
            let sources = match sources {
                Ok(sources) => sources,
                Err(refusal) => {
                    let refused = Unavailable::new(
                        "MCP configuration",
                        &format!("{CONFIG_UNREADABLE}{refusal}"),
                    );
                    return (
                        mcp::PublishedTools {
                            refresh_failed: true,
                            ..Default::default()
                        },
                        Some(vec![refused]),
                    );
                }
            };
            // The refusal is this product's own wording, which names config
            // keys and server names but never a configured value.
            let snapshot = match connections::McpSnapshot::read(&sources, &directory) {
                Ok(snapshot) => snapshot,
                Err(refusal) => {
                    let refused = Unavailable::new(
                        "MCP configuration",
                        &format!("{CONFIG_REFUSED}{refusal}"),
                    );
                    return (
                        mcp::PublishedTools {
                            refresh_failed: true,
                            ..Default::default()
                        },
                        Some(vec![refused]),
                    );
                }
            };
            (
                session_config::key_for_revision(session, &directory, revision),
                directory,
                snapshot.declared,
                snapshot.roots,
                snapshot.codemode_servers,
                snapshot.blocked,
                required_continuity,
                producer,
            )
        };
        if let Err(reason) = self
            .mcp
            .configure_roots(&location, &roots, &declared, roots_revision)
            .await
        {
            return (
                mcp::PublishedTools {
                    refresh_failed: true,
                    ..Default::default()
                },
                Some(vec![Unavailable::new("MCP roots", &reason)]),
            );
        }
        let project = |entries: &BTreeMap<String, mcp::ServerEntry>| {
            let mut published = mcp::tool_definitions_for_session(
                entries,
                tools::BUILTIN_TOOL_NAMES,
                required_continuity,
            );
            published.codemode_servers = codemode_servers;
            let unavailable = entries
                .iter()
                .filter_map(|(name, entry)| {
                    entry
                        .unavailable_reason()
                        .map(|reason| Unavailable::new(name, &reason))
                })
                .collect::<Vec<_>>();
            (published, unavailable)
        };
        let (mut published, mut unavailable) = if required_continuity {
            self.mcp
                .with_continuity_status_at(
                    &location,
                    &directory,
                    &declared,
                    producer.as_ref(),
                    project,
                )
                .await
        } else {
            self.mcp
                .with_status_at(&location, &directory, &declared, project)
                .await
        };
        unavailable.extend(
            blocked
                .iter()
                .map(|name| Unavailable::new(name, connections::PROJECT_LOCAL_BLOCKED)),
        );
        published.blocked_servers = blocked;
        if self.plugins.contribute_tools(&mut published).is_err() {
            return (
                mcp::PublishedTools {
                    refresh_failed: true,
                    ..Default::default()
                },
                Some(unavailable),
            );
        }
        published.refresh_failed = !unavailable.is_empty();
        (published, Some(unavailable))
    }
}

#[cfg(test)]
mod tests {
    //! WHAT THESE CANNOT WITNESS, said plainly rather than left implied.
    //!
    //! The original defect was at the DISPATCH CALL SITE, which derived its own
    //! key instead of calling this helper. A mutation there -- restoring
    //! `fs::canonicalize` around `record.placement.directory()` in
    //! `execution.rs` -- reintroduces the exact failure and passes everything
    //! below, because these call the helper directly.
    //!
    //! Closing that seam needs publication and dispatch observed together
    //! against one running product, which is the live journey. This note exists
    //! so the helper test is not mistaken for the whole regression witness.

    use super::registry_location;

    /// THE REGISTRY KEY IS THE PLACEMENT DIRECTORY, UNMODIFIED.
    ///
    /// Dispatch used to canonicalize it while publication did not, so on
    /// Windows the two keys were `E:/project` and `\?\E:\project` and an
    /// approved MCP call could never find its connection. This pins the
    /// property that was violated: the key is carried through, not resolved.
    ///
    /// A test that handed one identical key to both sides could not have
    /// caught that, which is why this asserts against the SPELLING rather than
    /// comparing two callers.
    #[test]
    fn the_registry_key_is_the_placement_directory_unmodified() {
        // A spelling GUARANTEED to differ from its canonical form on every
        // platform: a trailing `/.` names the same directory and is not what
        // `canonicalize` returns anywhere. The first version of this witness
        // only compared against the canonical form WHEN it happened to differ,
        // so on a host where the temp path is already canonical it asserted
        // nothing about the property it exists to pin.
        let temp = std::env::temp_dir();
        let given = format!("{}/.", temp.to_string_lossy().replace('\\', "/"));
        let placement = match grapt_types::LocationRef::new(&given, None) {
            Ok(placement) => placement,
            Err(error) => panic!("the witness needs a usable location: {error}"),
        };

        assert_eq!(
            registry_location(&placement),
            given,
            "the registry key must be the directory as given"
        );

        let canonical = match std::fs::canonicalize(&given) {
            Ok(canonical) => canonical.to_string_lossy().into_owned(),
            Err(error) => panic!("the temp directory must resolve: {error}"),
        };
        assert_ne!(
            canonical, given,
            "this witness needs a spelling that differs from its canonical form"
        );
        assert_ne!(
            registry_location(&placement),
            canonical,
            "the key must not be the canonical form: that is the defect"
        );
    }
}

/// The MCP catalog each running Session planned against, keyed BY SESSION.
///
/// The planning seam is synchronous and belongs to `grapt-runtime`, while
/// reading the registry may START a server and is not. So the snapshot is
/// taken in the async run that precedes planning and read at plan time.
///
/// It is keyed by Session because executions run CONCURRENTLY — the prompt
/// driver spawns jobs and up to `MAX_ACTIVE_EXECUTIONS` may be live at once.
/// A single latest-wins slot would let one Session publish its catalog, a
/// second overwrite it, and the first then plan against the second's tools:
/// a model offered a server the user never configured for that project, and a
/// call bound to the wrong authority. Cloning the Arc afterwards would only
/// hold the wrong catalog steady.
///
/// One snapshot per Session for the life of its execution is also what keeps
/// a model's attempts consistent: a catalog that changed between a first and
/// second attempt would let it call a tool it was never shown.
#[derive(Debug, Clone, Default)]
pub(super) struct PublishedSnapshots(Arc<Mutex<BTreeMap<SessionId, Arc<mcp::PublishedTools>>>>);

impl PublishedSnapshots {
    pub(super) fn set(&self, session: &SessionId, published: Arc<mcp::PublishedTools>) {
        let mut slots = match self.0.lock() {
            Ok(slots) => slots,
            Err(poisoned) => poisoned.into_inner(),
        };
        slots.insert(session.clone(), published);
    }

    /// The catalog this Session planned against. A Session with no snapshot
    /// publishes NO tools rather than borrowing another's.
    pub(super) fn get(&self, session: &SessionId) -> Arc<mcp::PublishedTools> {
        let slots = match self.0.lock() {
            Ok(slots) => slots,
            Err(poisoned) => poisoned.into_inner(),
        };
        slots
            .get(session)
            .map(Arc::clone)
            .unwrap_or_else(|| Arc::new(mcp::PublishedTools::default()))
    }

    pub(super) fn clear(&self, session: &SessionId) {
        let mut slots = match self.0.lock() {
            Ok(slots) => slots,
            Err(poisoned) => poisoned.into_inner(),
        };
        slots.remove(session);
    }
}

/// Removes a Session's snapshot however its execution ends, including a panic
/// or an early return. A snapshot outliving its execution is a catalog a later
/// run could inherit without having taken it.
#[derive(Debug)]
pub(super) struct PublishedSnapshotGuard {
    pub(super) snapshots: PublishedSnapshots,
    pub(super) session: SessionId,
}

impl Drop for PublishedSnapshotGuard {
    fn drop(&mut self) {
        self.snapshots.clear(&self.session);
    }
}

#[cfg(test)]
mod prefix_tests {
    /// Every notice prefix before a source refusal fits the budget the
    /// shared reader leaves it, so a notice never cuts the refusal.
    #[test]
    fn every_refusal_prefix_fits_the_reader_budget() {
        for prefix in [
            super::PLUGINS_REFUSED,
            super::CONFIG_UNREADABLE,
            super::CONFIG_REFUSED,
        ] {
            assert!(
                prefix.len() <= super::super::super::catalog_config::NOTICE_PREFIX_BYTES,
                "{prefix:?}"
            );
        }
    }
}
