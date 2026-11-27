//! Session cost is a bounded read projection; no estimated bill is persisted.
use super::{DogfoodApplication, SessionId};
use crate::{cache_accounting::CostTruth, usage_accounting::pricing};
use grapt_session::history::{accounting::AccountingSnapshot, SessionHistoryStore};
use grapt_types::MoneyUsd;
use serde_json::{json, Map, Value};

fn wire(amount: Option<MoneyUsd>, status: &str) -> Map<String, Value> {
    let mut fields = Map::from_iter([("costStatus".into(), json!(status))]);
    if let Some(amount) = amount {
        fields.insert("cost".into(), json!(amount.amount()));
    }
    fields
}

pub(crate) fn project(
    application: &DogfoodApplication,
    snapshot: Option<&AccountingSnapshot>,
    metrics_complete: bool,
) -> Map<String, Value> {
    if !metrics_complete {
        return wire(None, "unavailable");
    }
    // This uses the existing accounting snapshot and its read-only capability,
    // including retries, summaries and inherited Physical Attempt deduplication.
    let Some(snapshot) = snapshot else {
        return wire(None, "unreadable");
    };
    match estimate(application, snapshot) {
        Ok(fields) => fields,
        // An unavailable cost must not hide an otherwise readable Session.
        // It also must not become a zero or a complete estimate.
        Err(_) => wire(None, "unreadable"),
    }
}

pub(crate) fn accounting(
    application: &DogfoodApplication,
    session: &SessionId,
) -> Result<AccountingSnapshot, String> {
    if let Some(pool) = &application.readers {
        let mut read = pool.checkout().map_err(|error| error.to_string())?;
        let result = read
            .store_mut()
            .pooled_read_snapshot()
            .and_then(|source| SessionHistoryStore::accounting_snapshot(&source, session));
        // A read that produced no value closes its reader instead of idling a
        // connection that may no longer reach the database; only a completed
        // read recycles. Same rule as the other read paths.
        if result.is_err() {
            read.discard();
            return result;
        }
        read.finish().map_err(|error| error.to_string())?;
        return result;
    }
    let mut inbox = application.lock_inbox();
    let result = inbox
        .store_mut()
        .pooled_read_snapshot()
        .and_then(|source| SessionHistoryStore::accounting_snapshot(&source, session));
    result
}

fn estimate(
    application: &DogfoodApplication,
    snapshot: &AccountingSnapshot,
) -> Result<Map<String, Value>, String> {
    let Some(inventory) = &application.catalog_snapshot().inventory else {
        return Ok(wire(None, "unavailable"));
    };
    let mut total = MoneyUsd::ZERO;
    let mut priced = 0;
    let mut complete = snapshot.enumeration_complete && snapshot.operational_coverage_complete;
    for attempt in &snapshot.attempts {
        let Some(selection) = &attempt.selection else {
            complete = false;
            continue;
        };
        // Catalog API rates cannot become subscription charges. Match the
        // actual configured account, including non-default selected providers.
        if application
            .catalog_snapshot()
            .protocols
            .get(selection.provider())
            .is_none_or(|family| *family == grapt_provider::ProtocolFamily::ChatGptCodex)
            || !attempt.terminal_observed
            || !pricing::final_usage(&attempt.usage)
        {
            complete = false;
            continue;
        }
        let Some(model) = inventory.available().iter().find(|model| {
            model.provider() == selection.provider() && model.id() == selection.model()
        }) else {
            complete = false;
            continue;
        };
        match pricing::catalog_cost(
            selection.provider().as_str(),
            selection.model().as_str(),
            model.metadata(),
            &attempt.usage,
        )? {
            (CostTruth::Estimated { amount, .. }, _) => {
                total = total
                    .checked_add(amount)
                    .map_err(|error| error.to_string())?;
                priced += 1;
            }
            _ => complete = false,
        }
    }
    Ok(if priced == 0 {
        wire(None, "unavailable")
    } else {
        wire(
            Some(total),
            if complete {
                "catalog-api-estimate"
            } else {
                "catalog-api-subtotal"
            },
        )
    })
}
