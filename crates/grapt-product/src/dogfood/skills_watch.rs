//! Skills invalidation.
//! Copyright 2025 OpenAI. Apache-2.0; modified for bounded nonrecursive
//! registrations, the existing catalog scan and service-owned driver.
use grapt_types::{ClockAuthority, EventId, IdAuthority, LocationId, LocationRef};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Notify;

const MAX_LOCATIONS: usize = 32;
const MAX_SOURCES: usize = 512;
const MAX_WATCHES: usize = 4096;
const DEBOUNCE: Duration = Duration::from_millis(250);

type Sources = BTreeMap<LocationId, (LocationRef, BTreeSet<PathBuf>)>;
#[derive(Default)]
struct Signals {
    scopes: Mutex<BTreeMap<LocationId, (LocationRef, Vec<PathBuf>)>>,
    dirty: Mutex<BTreeSet<LocationId>>,
    changed: Notify,
    rewatch: AtomicBool,
    warned: AtomicBool,
}
#[derive(Default)]
struct State {
    backend: Option<RecommendedWatcher>,
    sources: Sources,
    registered: BTreeSet<PathBuf>,
}
pub(super) struct Watch {
    state: Mutex<State>,
    signals: Arc<Signals>,
}
impl std::fmt::Debug for Watch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SkillWatcher").finish_non_exhaustive()
    }
}
fn lock<T>(value: &Mutex<T>) -> MutexGuard<'_, T> {
    value
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}
impl Watch {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            signals: Arc::default(),
        })
    }

    /// Register before reading skill files, so a change during that read also
    /// invalidates the completed response. Session-local configured roots at
    /// the same location are retained together, within the catalog's bound.
    pub fn track(&self, directory: &str, roots: Vec<PathBuf>) -> Result<(), String> {
        let location = LocationRef::new(directory, None).map_err(|e| e.to_string())?;
        let id = location.canonical_id();
        let mut state = lock(&self.state);
        if !state.sources.contains_key(&id) && state.sources.len() >= MAX_LOCATIONS {
            return Err("automatic skill refresh exceeds 32 locations".into());
        }
        let mut roots: BTreeSet<_> = roots.into_iter().collect();
        if let Some((_, previous)) = state.sources.get(&id) {
            roots.extend(previous.iter().cloned());
        }
        if roots.len() > MAX_SOURCES {
            return Err("automatic skill refresh exceeds 512 sources at this location".into());
        }
        let previous = state.sources.insert(id.clone(), (location, roots));
        match self.reconcile(&mut state, false) {
            Ok(()) => Ok(()),
            Err(error) => {
                self.signals.warn_once(&error);
                if let Some(previous) = previous {
                    state.sources.insert(id, previous);
                } else {
                    state.sources.remove(&id);
                }
                Err(error)
            }
        }
    }

    fn reconcile(&self, state: &mut State, rewatch: bool) -> Result<(), String> {
        self.reconcile_with(state, rewatch, &mut |_| Ok(()))
    }

    fn reconcile_with(
        &self,
        state: &mut State,
        rewatch: bool,
        after_watch: &mut impl FnMut(&Path) -> Result<(), String>,
    ) -> Result<(), String> {
        let registrations_changed = std::cell::Cell::new(false);
        let result = (|| {
            let mut scopes = BTreeMap::new();
            for (id, (location, roots)) in &state.sources {
                let mut matched = BTreeSet::new();
                for root in roots {
                    matched.insert(root.clone());
                    let ancestor = existing_directory(root)?;
                    let suffix = root
                        .strip_prefix(&ancestor)
                        .map_err(|_| "skill watch ancestry changed")?;
                    let canonical = std::fs::canonicalize(&ancestor)
                        .map_err(|e| format!("skill watch path is unreadable: {e}"))?;
                    matched.insert(canonical.join(suffix));
                }
                scopes.insert(
                    id.clone(),
                    (location.clone(), matched.into_iter().collect()),
                );
            }
            if state.backend.is_none() {
                let signals = Arc::clone(&self.signals);
                state.backend = Some(
                    notify::recommended_watcher(move |event| signals.observe(event))
                        .map_err(|e| format!("skill watcher unavailable: {e}"))?,
                );
            }
            // Publish before installing watches; callbacks never take the state
            // mutex, including when the SDK calls one synchronously during watch.
            *lock(&self.signals.scopes) = scopes;
            let backend = state.backend.as_mut().ok_or("skill watcher is absent")?;
            let registered = &mut state.registered;
            let mut plan = BTreeSet::new();
            let mut install = |path: &Path| -> Result<(), String> {
                if path.to_str().is_none() {
                    return Err("automatic skill refresh requires Unicode watch paths".into());
                }
                if !plan.insert(path.to_path_buf()) {
                    return Ok(());
                }
                if plan.len() > MAX_WATCHES {
                    return Err("automatic skill refresh exceeds 4096 directories".into());
                }
                // Re-arm one directory at a time, after its ancestors. The volume
                // root retains its registration. Stream-wide backend gaps are
                // covered by the invalidation after this batch.
                if rewatch && path.parent().is_some() && registered.remove(path) {
                    registrations_changed.set(true);
                    let _removed = backend.unwatch(path);
                }
                if !registered.contains(path) {
                    if registered.len() >= MAX_WATCHES {
                        return Err("automatic skill refresh exceeds 4096 registrations".into());
                    }
                    registrations_changed.set(true);
                    backend
                        .watch(path, RecursiveMode::NonRecursive)
                        .map_err(|e| format!("skill directory watch unavailable: {e}"))?;
                    registered.insert(path.to_path_buf());
                    after_watch(path)?;
                }
                Ok(())
            };
            for (id, (_, roots)) in &state.sources {
                for root in roots {
                    let parent = existing_directory(root.parent().unwrap_or(root))?;
                    let parent = std::fs::canonicalize(parent)
                        .map_err(|e| format!("skill watch parent is unreadable: {e}"))?;
                    let ancestors = parent.ancestors().take(129).collect::<Vec<_>>();
                    if ancestors.len() > 128 {
                        return Err("skill watch ancestry exceeds 128 directories".into());
                    }
                    // Keep ancestor coverage for incremental backends and observe
                    // creation of an initially missing skill root.
                    for ancestor in ancestors.into_iter().rev() {
                        install(ancestor)?;
                    }
                    // Reuse the bounded catalog walk, installing each watch before
                    // enumerating its children rather than registering a stale plan.
                    super::catalog_markdown::watch_directories(root, &mut |path| {
                        let mut scopes = lock(&self.signals.scopes);
                        if let Some((_, matched)) = scopes.get_mut(id) {
                            if !matched.iter().any(|root| path.starts_with(root)) {
                                matched.push(path.to_path_buf());
                            }
                        }
                        drop(scopes);
                        install(path)
                    })?;
                }
            }
            for path in registered.difference(&plan).cloned().collect::<Vec<_>>() {
                registrations_changed.set(true);
                let _removed = backend.unwatch(&path);
                registered.remove(&path);
            }
            Ok(())
        })();
        if registrations_changed.get() {
            // FSEvents restarts its entire stream with SinceNow on registration
            // changes. Invalidate every scope after the final restart, including
            // failure paths; the next bounded reconciliation/catalog read covers
            // edits and new directories whose native events fell in that gap.
            self.signals.invalidate_all();
        }
        result
    }

    async fn changed(&self) -> Vec<LocationRef> {
        loop {
            let notified = self.signals.changed.notified();
            if lock(&self.signals.dirty).is_empty() {
                notified.await;
            }
            tokio::time::sleep(DEBOUNCE).await;
            let dirty = std::mem::take(&mut *lock(&self.signals.dirty));
            if dirty.is_empty() {
                continue;
            }
            let mut state = lock(&self.state);
            let rewatch = self.signals.rewatch.swap(false, Ordering::AcqRel);
            if let Err(error) = self.reconcile(&mut state, rewatch) {
                self.signals.warn_once(&error);
            }
            return dirty
                .into_iter()
                .filter_map(|id| state.sources.get(&id).map(|(location, _)| location.clone()))
                .collect();
        }
    }
}

impl Signals {
    fn invalidate_all(&self) {
        let scopes = lock(&self.scopes);
        let mut dirty = lock(&self.dirty);
        dirty.extend(scopes.keys().cloned());
        if !dirty.is_empty() {
            self.changed.notify_one();
        }
    }

    fn warn_once(&self, error: &str) {
        if !self.warned.swap(true, Ordering::AcqRel) {
            crate::diagnostics::report(format!(
                "Automatic skill refresh unavailable; reopening / completion rescans: {}",
                error.chars().take(512).collect::<String>()
            ));
        }
    }
    fn observe(&self, event: notify::Result<Event>) {
        let event = match event {
            Ok(event) if matches!(event.kind, EventKind::Access(_)) => return,
            Ok(event) => Some(event),
            Err(error) => {
                self.warn_once(&error.to_string());
                None
            }
        };
        // A path can be removed and recreated before debounce ends. Its
        // spelling stays the same while the native watch still names the old
        // directory, so structural events must re-arm existing registrations.
        // Windows reports ordinary writes as Modify(Any), including metadata
        // changes on shared ancestors. They invalidate content inside a root,
        // but cannot replace a directory watch or invalidate sibling roots.
        let structural = event.as_ref().is_none_or(|event| {
            event.need_rescan()
                || !matches!(
                    event.kind,
                    EventKind::Modify(
                        notify::event::ModifyKind::Any | notify::event::ModifyKind::Data(_)
                    )
                )
        });
        let scopes = lock(&self.scopes);
        let mut dirty = lock(&self.dirty);
        for (id, (_, roots)) in &*scopes {
            let relevant = event.as_ref().is_none_or(|event| {
                event.need_rescan()
                    || event.paths.len() > 64
                    || event.paths.iter().any(|path| {
                        roots.iter().any(|root| {
                            path.starts_with(root) || (structural && root.starts_with(path))
                        })
                    })
            });
            if relevant {
                if structural {
                    self.rewatch.store(true, Ordering::Release);
                }
                dirty.insert(id.clone());
            }
        }
        if !dirty.is_empty() {
            self.changed.notify_one();
        }
    }
}

fn existing_directory(path: &Path) -> Result<PathBuf, String> {
    for ancestor in path.ancestors().take(128) {
        match std::fs::metadata(ancestor) {
            Ok(metadata) if metadata.is_dir() => return Ok(ancestor.to_path_buf()),
            Ok(_) => return Err("skill watch ancestor is not a directory".into()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("skill watch ancestor is unreadable: {error}")),
        }
    }
    Err("skill watch ancestry exceeds 128 directories".into())
}

pub(super) struct Drive {
    watch: Arc<Watch>,
    ids: Arc<super::OsRandomIds>,
    clock: Arc<super::SystemClock>,
    hub: Arc<grapt_api::DogfoodEventHub>,
}
impl Drive {
    pub async fn run(&self) {
        loop {
            for location in self.watch.changed().await {
                if let Err(error) = self.emit(&location) {
                    crate::diagnostics::report(format!("Skill refresh hint unavailable: {error}"));
                }
            }
        }
    }
    fn emit(&self, location: &LocationRef) -> Result<(), String> {
        use grapt_api::u3a::{DogfoodEvent, FiniteF64, LiveEvent, SkillsChangedData, WireId};
        let id = WireId::<EventId>::parse(format!("evt_{}", self.ids.mint().as_str()))
            .ok_or("skill refresh identity was refused")?;
        let created =
            FiniteF64::try_new(self.clock.now().as_millis() as f64).map_err(|e| e.to_string())?;
        let event = DogfoodEvent::SkillsChanged(LiveEvent::new(id, created, SkillsChangedData {}));
        self.hub
            .publish_at(&event, location)
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}
pub(super) fn attach(
    driver: &mut super::prompt_driver::PromptDriver,
    app: &super::ServerApplication,
) {
    driver.attach_skills(Arc::new(Drive {
        watch: app.skills_watch.clone(),
        ids: app.ids.clone(),
        clock: app.clock.clone(),
        hub: app.hub.clone(),
    }));
}

#[cfg(test)]
mod tests;
