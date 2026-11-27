//! Offline cleanup command dispatch, separate from provider/service startup.
use crate::session_cleanup::{CleanupCommand, CleanupDisposition};
use grapt_store::{OsRandomIds, SystemClock};

pub(crate) fn run(arguments: &[String]) -> Result<String, String> {
    let command = CleanupCommand::parse(arguments).map_err(|error| error.to_string())?;
    let config = grapt_platform::PublishedConfig::load(command.service())
        .map_err(|error| error.to_string())?;
    let ids = OsRandomIds::seed().map_err(|error| error.to_string())?;
    let report = command
        .run(config.build().as_str(), &SystemClock, &ids)
        .map_err(|error| error.to_string())?;
    let rows = report
        .sessions
        .iter()
        .map(|row| {
            let (disposition, reason) = match &row.disposition {
                CleanupDisposition::Eligible => ("eligible", None),
                CleanupDisposition::Tombstoned => ("tombstoned", None),
                CleanupDisposition::Retained(reason) => ("retained", Some(format!("{reason:?}"))),
            };
            serde_json::json!({
                "session":row.session.as_str(), "last_event_ms":row.last_event_ms,
                "disposition":disposition, "reason":reason,
            })
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&serde_json::json!({
        "schema":"grapt.session-retention.v1", "applied":report.applied,
        "physical_erasure":false, "sessions":rows,
    }))
    .map_err(|error| error.to_string())
}

pub(crate) const HELP: &str = "usage: grapt-server cleanup-sessions --service <absolute-profile> --cutoff <unix-ms> --current-session <ses_id> [--apply]\n\nOffline session-retention cleanup. Stop the service first; a live owner refuses.\nDry-run is the default; --apply appends session tombstones. Both modes take the\nregistration lock. The cutoff must be in the past. Current, recent, active,\npending-effect, descendant-bearing or uncertain sessions are retained.\nThis is not physical erasure: events, artifacts and operational evidence remain.\nSee docs/session-cleanup.md for exclusions and coordination limits.";
