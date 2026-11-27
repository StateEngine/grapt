//! How many locations keep connections, and which one gives way.
//!
//! Split out of `mcp.rs` under the workspace god-file ceiling.

use std::collections::BTreeMap;

use super::{LocationConnections, ServerEntry};

/// How many MCP connections this product may hold at once, LIVE AND RETIRED
/// together.
///
/// Each is a child process and a pipe. The location bound below counts map
/// entries, and a retired connection a caller still holds is neither in the
/// map nor gone — so the two bounds measure different things and the product
/// needs both.
pub(crate) const MAX_LIVE_CONNECTIONS: usize = 32;

/// How many callers may wait on ONE connection at once.
///
/// The channel serves one at a time, so a queue is expected; an unbounded one
/// is not. Each waiter holds a reference that keeps the child alive past any
/// eviction, so this bounds retained processes as much as it bounds patience.
pub(crate) const MAX_QUEUED_CALLERS: usize = 8;

/// How many locations retain connections at once.
// Session config revisions and subagents each have distinct registry keys.
// Match the aggregate connection ceiling instead of evicting a parent's key
// after only eight catalog reads; process admission remains capped separately.
pub(crate) const MAX_RETAINED_LOCATIONS: usize = MAX_LIVE_CONNECTIONS;

/// Mark a connection dead if it is one.
pub(crate) fn retire(entry: &ServerEntry) {
    if let ServerEntry::Live(live) = entry {
        live.roots.retire();
        live.io.retire();
    }
}

/// A process-wide monotonic read ordinal.
pub(crate) fn next_read_ordinal() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Drop least-recently-read locations until the bound admits one more.
///
/// The location being read is never evicted: releasing the connections a
/// caller is about to use would make the bound produce the restart storm it
/// exists to prevent.
pub(crate) fn release_over_bound(
    locations: &mut BTreeMap<String, LocationConnections>,
    keep: &str,
    bound: usize,
    now_ms: i64,
) -> bool {
    while locations.len() >= bound && !locations.contains_key(keep) {
        let Some(oldest) = locations
            .iter()
            .filter(|(name, held)| name.as_str() != keep && !protected(held))
            .min_by_key(|(_, held)| held.last_read)
            .map(|(name, _)| name.clone())
        else {
            return false;
        };
        // Retire BEFORE dropping, and note what dropping does NOT do. Removing
        // the entry drops THIS owner's reference; a caller queued on the
        // connection holds its own, so the channel and its child outlive the
        // removal for as long as that caller does. Retiring is what stops such
        // a caller dispatching; it is not what ends the process.
        // The separate aggregate connection permit still accounts for retired
        // I/O until its final holder releases it.
        if let Some(released) = locations.remove(&oldest) {
            crate::diagnostics::report(format!(
                "MCP_LOCATION_EVICTED at_ms={} location={:?} last_read_age_ms={} bound_producer={}",
                now_ms,
                oldest.chars().take(4096).collect::<String>(),
                now_ms.saturating_sub(released.last_read_ms).max(0),
                bound_producer(&released),
            ));
            for token in released
                .starting
                .values()
                .filter_map(std::sync::Weak::upgrade)
            {
                token.retire();
            }
            for entry in released.entries.values() {
                retire(entry);
            }
        }
    }
    true
}

/// A retained bound producer and any exchange or startup own their location.
/// Refuse a new location when every slot is protected; never grow the map.
fn bound_producer(held: &LocationConnections) -> bool {
    held.bound_producer
        .as_ref()
        .and_then(std::sync::Weak::upgrade)
        .is_some_and(|token| token.load(std::sync::atomic::Ordering::Acquire))
        && matches!(held.entries.get("stateengine"), Some(ServerEntry::Live(_)))
}

fn protected(held: &LocationConnections) -> bool {
    held.active.strong_count() != 0
        || bound_producer(held)
        || held
            .starting
            .values()
            .any(|token| token.strong_count() != 0)
        || held.entries.values().any(|entry| match entry {
            ServerEntry::Live(live) => std::sync::Arc::strong_count(&live.io) > 1,
            _ => false,
        })
}
