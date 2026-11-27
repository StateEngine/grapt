//! Oracle-parity `compaction` config (`core/src/config/compaction.ts`):
//! `{ auto?: boolean, buffer?: non-negative int, keep?: { tokens?: int } }`,
//! folded over the ordered config documents, a later document winning per
//! key (`session/compaction.ts` `settings`).
//!
//! - `auto: false` disables the proactive trigger: the 90% ceiling on the
//!   reported total (else the local estimate). A provider overflow, a
//!   structural bound and `/compact` still compact.
//! - `buffer` is accepted and IGNORED: SE compacts at 90% of the window, so a
//!   buffer has no threshold to move. A Session whose configuration sets it is
//!   told so once ([`IgnoredKey`]).
//! - `server_side` (SE's own key) lets an OpenAI Responses or ChatGPT/Codex
//!   route compact on the provider: the same decision sends a
//!   `compaction_trigger` and commits the provider's opaque compaction item,
//!   falling back to SE's own summary when that fails.
//!   Other providers always use SE's own summary. The old key still parses,
//!   but cannot override the provider-first decision of 2026-10-30.
//! - `keep` is accepted and IGNORED (named divergence,
//!   docs/coding-quality/COMPACTION-CONFIG.md): in SE it would only size the
//!   published `recent` text, which history replay recomputes, so honouring
//!   it needs a durable per-compaction record first. A Session whose
//!   configuration sets `keep.tokens` is told so once ([`IgnoredKey`]).
use std::path::Path;

use grapt_config::ConfigValue;

use super::catalog_config::{CatalogEnvironment, CatalogSources};

/// A config key SE accepts, so an oracle configuration loads, but does not
/// apply, and what SE does instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct IgnoredKey {
    pub key: &'static str,
    pub reason: &'static str,
}

pub(super) const SERVER_SIDE: IgnoredKey = IgnoredKey {
    key: "compaction.server_side",
    reason: "OpenAI uses provider compaction; other providers use SE summary",
};

pub(super) const BUFFER: IgnoredKey = IgnoredKey {
    key: "compaction.buffer",
    reason: "SE compacts at 90% of the context window",
};

pub(super) const KEEP_TOKENS: IgnoredKey = IgnoredKey {
    key: "compaction.keep.tokens",
    reason: "SE uses its own recent-context retention",
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct CompactionSettings {
    pub auto: bool,
    pub buffer: Option<u64>,
    /// Optional earlier ceiling on provider-reported context tokens.
    pub model_auto_compact_token_limit: Option<u64>,
    /// Legacy value, parsed for compatibility and reported as ignored.
    pub server_side: Option<bool>,
    /// Whether any document set `keep.tokens`, which has no effect.
    pub keep_tokens: bool,
}

impl Default for CompactionSettings {
    fn default() -> Self {
        Self {
            auto: true,
            buffer: None,
            model_auto_compact_token_limit: None,
            server_side: None,
            keep_tokens: false,
        }
    }
}

impl CompactionSettings {
    /// Provider-first for OpenAI, SE summary for every other protocol. The
    /// legacy server_side key still parses but cannot override this policy.
    pub(super) fn server_side_on(&self, family: grapt_provider::ProtocolFamily) -> bool {
        matches!(
            family,
            grapt_provider::ProtocolFamily::OpenAiResponses
                | grapt_provider::ProtocolFamily::ChatGptCodex
        )
    }

    /// The keys these settings were given that SE does not apply.
    pub(super) fn ignored(&self) -> impl Iterator<Item = IgnoredKey> {
        self.keep_tokens
            .then_some(KEEP_TOKENS)
            .into_iter()
            .chain(self.server_side.is_some().then_some(SERVER_SIDE))
            .chain(self.buffer.is_some().then_some(BUFFER))
    }

    /// The settings that apply to a Session placed at `directory`, under the
    /// service whose root configuration is `service_root/grapt.jsonc`.
    /// That root document is the base, as the product's own configuration
    /// (provider, catalog) is; the Session's catalog sources (user config,
    /// project ancestors, GRAPT_CONFIG) fold over it, a later document
    /// winning per key. Only an ABSENT setting takes the default: a source
    /// that cannot be read is refused, as the catalogs refuse it; a
    /// `compaction` value of the wrong shape is refused by name.
    pub(super) fn read(service_root: &Path, directory: &Path) -> Result<Self, String> {
        let sources = CatalogSources::read(directory, CatalogEnvironment::read(service_root)?)?;
        Self::fold(sources.documents.iter().map(|document| &document.value))
    }

    pub(super) fn fold<'a>(
        documents: impl IntoIterator<Item = &'a ConfigValue>,
    ) -> Result<Self, String> {
        let mut settings = Self::default();
        for document in documents {
            if let Some(limit) = document.get("model_auto_compact_token_limit") {
                let limit = non_negative(limit.value(), "model_auto_compact_token_limit")?;
                if limit == 0 {
                    return Err("config model_auto_compact_token_limit must be positive".into());
                }
                settings.model_auto_compact_token_limit = Some(limit);
            }
            let Some(compaction) = document.get("compaction") else {
                continue;
            };
            let ConfigValue::Object(_) = compaction.value() else {
                return Err(format!(
                    "config compaction must be an object, found {}",
                    compaction.value().kind()
                ));
            };
            if let Some(auto) = compaction.value().get("auto") {
                settings.auto = auto.value().as_bool().ok_or_else(|| {
                    format!(
                        "config compaction.auto must be a boolean, found {}",
                        auto.value().kind()
                    )
                })?;
            }
            if let Some(server_side) = compaction.value().get("server_side") {
                settings.server_side = Some(server_side.value().as_bool().ok_or_else(|| {
                    format!(
                        "config compaction.server_side must be a boolean, found {}",
                        server_side.value().kind()
                    )
                })?);
            }
            if let Some(buffer) = compaction.value().get("buffer") {
                settings.buffer = Some(non_negative(buffer.value(), "compaction.buffer")?);
            }
            if let Some(keep) = compaction.value().get("keep") {
                // Accepted for oracle configs, deliberately not applied.
                let ConfigValue::Object(_) = keep.value() else {
                    return Err(format!(
                        "config compaction.keep must be an object, found {}",
                        keep.value().kind()
                    ));
                };
                if let Some(tokens) = keep.value().get("tokens") {
                    non_negative(tokens.value(), KEEP_TOKENS.key)?;
                    settings.keep_tokens = true;
                }
            }
        }
        Ok(settings)
    }
}

fn non_negative(value: &ConfigValue, key: &str) -> Result<u64, String> {
    let ConfigValue::Number(number) = value else {
        return Err(format!(
            "config {key} must be a non-negative integer, found {}",
            value.kind()
        ));
    };
    number
        .as_i64()
        .and_then(|number| u64::try_from(number).ok())
        .ok_or_else(|| format!("config {key} must be a non-negative integer"))
}

#[cfg(test)]
#[path = "compaction_config_tests.rs"]
mod tests;
