// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// bb64b894fd67158be0f874872e6d7bfe717e7368. Modified for bounded coalescing,
// the held channel's sole reader, and location-scoped live refresh hints.
use super::{channel::HeldIo, Declaration, LiveIo, McpRegistry, ServerEntry};
use grapt_api::{
    u3a::{BoundedString, DogfoodEvent, FiniteF64, LiveEvent, McpCatalogChangedData, WireId},
    DogfoodEventHub,
};
use grapt_types::{ClockAuthority, EventId, IdAuthority};
use std::{
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc, Weak,
    },
    task::Poll,
    time::Duration,
};

pub(crate) struct Emitter {
    ids: Arc<grapt_store::OsRandomIds>,
    clock: Arc<grapt_store::SystemClock>,
    hub: Arc<DogfoodEventHub>,
}
impl std::fmt::Debug for Emitter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("McpCatalogEmitter")
    }
}
impl Emitter {
    pub(crate) fn new(
        ids: Arc<grapt_store::OsRandomIds>,
        clock: Arc<grapt_store::SystemClock>,
        hub: Arc<DogfoodEventHub>,
    ) -> Arc<Self> {
        Arc::new(Self { ids, clock, hub })
    }
    pub(crate) fn emit(&self, location: &str) {
        self.emit_kind(location, false);
    }
    pub(crate) fn resource_changed(&self, location: &str) {
        self.emit_kind(location, true);
    }
    fn emit_kind(&self, location: &str, resource: bool) {
        let Some(event) = (|| {
            let scope = BoundedString::try_new(super::super::catalog_mcp::scope(location)).ok()?;
            let id = WireId::<EventId>::parse(format!("evt_{}", self.ids.mint().as_str()))?;
            let created = FiniteF64::try_new(self.clock.now().as_millis() as f64).ok()?;
            let event = LiveEvent::new(id, created, McpCatalogChangedData { scope });
            Some(if resource {
                DogfoodEvent::McpResourceChanged(event)
            } else {
                DogfoodEvent::McpCatalogChanged(event)
            })
        })() else {
            return;
        };
        if let Err(error) = self.hub.publish(&event) {
            crate::diagnostics::report(format!("MCP catalog refresh hint unavailable: {error}"));
        }
    }
}
#[derive(Debug)]
pub(crate) struct Signal {
    bits: AtomicU8,
    resources: Arc<super::subscriptions::State>,
    emitter: Arc<Emitter>,
    location: String,
}
impl Signal {
    pub(crate) fn changed(&self) {
        self.emitter.resource_changed(&self.location);
    }
    pub(crate) fn accept(&self, notification: &serde_json::Value) {
        if notification["method"] == "notifications/resources/updated" {
            if self.resources.notify(notification) {
                self.changed();
            }
            return;
        }
        let bit = match notification["method"].as_str() {
            Some("notifications/prompts/list_changed") => 1,
            Some("notifications/resources/list_changed") => 2,
            _ => return,
        };
        if self.bits.fetch_or(bit, Ordering::AcqRel) & bit == 0 {
            self.emitter.emit(&self.location);
        }
    }
    pub(crate) fn clear(&self, kind: grapt_mcp::content::CatalogKind) {
        let bit = if kind == grapt_mcp::content::CatalogKind::Prompts {
            1
        } else {
            2
        };
        self.bits.fetch_and(!bit, Ordering::AcqRel);
    }
}
#[derive(Debug)]
pub(crate) struct Intake(tokio::task::JoinHandle<()>);
impl Drop for Intake {
    fn drop(&mut self) {
        self.0.abort();
    }
}
impl McpRegistry {
    pub(crate) async fn connect(
        &self,
        key: &str,
        directory: &str,
        server: &str,
        declaration: &Declaration,
        startup: Arc<super::startup_lease::StartupLease>,
    ) -> ServerEntry {
        let roots = self.roots_for(key, server, declaration).await;
        let mut entry = self
            .connect_raw(directory, server, declaration, startup, roots)
            .await;
        if let ServerEntry::Live(live) = &entry {
            self.track_resources(&live.io.resources);
        }
        if let (Some(emitter), ServerEntry::Live(live)) = (&self.catalog_emitter, &mut entry) {
            live.io.resources.hint(emitter, key);
            if let Ok(mut io) = live.io.io().try_lock() {
                io.channel.set_catalog_signal(Arc::new(Signal {
                    bits: AtomicU8::new(0),
                    resources: live.io.resources.clone(),
                    emitter: emitter.clone(),
                    location: key.into(),
                }));
            }
            let weak = Arc::downgrade(&live.io);
            live.notifications = Some(Intake(tokio::spawn(intake(weak))));
            emitter.emit(key);
        }
        entry
    }
}
async fn intake(weak: Weak<HeldIo<LiveIo>>) {
    loop {
        // Passive reads release their strong reference before sleeping. Cleanup
        // exchanges hold the same I/O lock, with a two-second ceiling and an
        // immediate retirement fence; there is still exactly one wire reader.
        let Some(held) = weak.upgrade() else {
            return;
        };
        if held.is_retired() || held.observed_unusable().is_some() {
            held.resources.close();
            return;
        }
        if let Ok(mut io) = held.io().try_lock() {
            if let Err(error) = super::subscriptions::drain_one(&held, &mut io).await {
                held.observe(error.into_unusable());
                held.retire();
                return;
            }
            let result = std::future::poll_fn(|cx| Poll::Ready(io.channel.poll_idle(cx))).await;
            if let Err(error) = result {
                held.observe(error.into_unusable());
                held.retire();
                return;
            }
        }
        drop(held);
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

impl grapt_mcp_host::CatalogNotifications for Signal {
    fn accept(&self, notification: &serde_json::Value) {
        Signal::accept(self, notification);
    }
    fn clear(&self, kind: grapt_mcp::content::CatalogKind) {
        Signal::clear(self, kind);
    }
    fn changed(&self) {
        Signal::changed(self);
    }
}
