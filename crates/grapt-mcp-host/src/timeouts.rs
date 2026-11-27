//! Per-server MCP timeout phases, and the bounds a configured value obeys.
//!
//! Three phases because they measure different work and a user configures them
//! for different reasons: `startup` covers the handshake, `catalog` covers
//! `tools/list`, and `execution` covers a tool call. One shared deadline
//! cannot serve all three -- an execution budget generous enough for a long
//! tool also makes a dead server look merely slow.
//!
//! Values are milliseconds. Precedence is global then per-server: a global
//! `mcp.timeout` supplies defaults and a server's own `timeout` replaces only
//! the phases it names.

use std::time::Duration;

use serde_json::Value;

/// The largest any configured phase may be: 24 hours.
///
/// It has to clear the longest phase anyone legitimately configures. The
/// frozen contract's own execution default is twelve hours -- a tool call left
/// running for a working day -- so any bound BELOW that would refuse a value
/// the contract treats as ordinary. (A bound exactly equal to it would admit
/// it, since the check is `>`; the point is that there must be room above.)
/// Twenty-four hours leaves that room while keeping every phase finite, so no
/// configuration can produce a wait that never ends.
const MAX_TIMEOUT_MS: u64 = 24 * 60 * 60 * 1_000;

/// What each phase costs when nothing is configured.
///
/// 10s / 10s / 120s: this product's accepted defaults, retained deliberately.
/// The frozen contract's are 30s / 30s / 12h. The execution difference is the
/// material one -- adopting twelve hours would make every hung tool call a
/// half-day wait for users who configured nothing -- so the divergence is a
/// ruled decision recorded here, not an oversight. Configured values are
/// honoured either way, which is what this module exists to make true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpTimeouts {
    pub startup: Duration,
    pub catalog: Duration,
    pub execution: Duration,
}

impl Default for McpTimeouts {
    fn default() -> Self {
        Self {
            startup: Duration::from_secs(10),
            catalog: Duration::from_secs(10),
            execution: Duration::from_secs(120),
        }
    }
}

impl McpTimeouts {
    /// Read a `timeout` object, layering it over `base`.
    ///
    /// A phase present here replaces the one in `base`; a phase absent leaves
    /// it alone, which is what makes a global a default rather than an
    /// override.
    ///
    /// A malformed value is REFUSED, never dropped: silently returning `base`
    /// is the defect this module exists to fix.
    pub fn layer(base: Self, fields: &Value, where_: &str) -> Result<Self, String> {
        let Some(object) = fields.as_object() else {
            return Err(format!("{where_} timeout must be an object"));
        };
        let mut layered = base;
        for (key, value) in object {
            let millis = phase_millis(value, where_, key)?;
            let phase = Duration::from_millis(millis);
            match key.as_str() {
                "startup" => layered.startup = phase,
                "catalog" => layered.catalog = phase,
                "execution" => layered.execution = phase,
                other => {
                    return Err(format!(
                        "{where_} timeout has no {other} phase; \
                         it carries startup, catalog and execution"
                    ))
                }
            }
        }
        Ok(layered)
    }
}

/// A phase value in milliseconds, bounded and positive.
fn phase_millis(value: &Value, where_: &str, phase: &str) -> Result<u64, String> {
    let Some(number) = value.as_u64() else {
        // Named precisely. `-1`, `1.5` and `"30000"` are three different
        // mistakes and an operator fixing one should not have to guess which.
        return Err(format!(
            "{where_} timeout {phase} must be a whole number of milliseconds, not {value}"
        ));
    };
    if number == 0 {
        return Err(format!(
            "{where_} timeout {phase} must be positive; zero would refuse every attempt \
             before it began"
        ));
    }
    if number > MAX_TIMEOUT_MS {
        return Err(format!(
            "{where_} timeout {phase} is {number}ms, over the {MAX_TIMEOUT_MS}ms bound"
        ));
    }
    Ok(number)
}
