//! Format public server timestamps; never read an ambient wall clock.
// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Duration formatting.
// Modified by Grapt: public outcome, typed timestamps, explicit UTC, and unknown-start handling.
pub(super) fn summary(
    started_ms: Option<u64>,
    completed_ms: Option<u64>,
    outcome: Option<&grapt_client::Outcome>,
) -> Option<String> {
    let completed = completed_ms?;
    let label = match outcome? {
        grapt_client::Outcome::Succeeded => "done",
        grapt_client::Outcome::Failed => "failed",
        grapt_client::Outcome::Interrupted => "interrupted",
    };
    let minute = completed / 60_000 % 1_440;
    let terminal = format!("{label} {:02}:{:02} UTC", minute / 60, minute % 60);
    let Some(elapsed) = started_ms.and_then(|started| completed.checked_sub(started)) else {
        return Some(terminal);
    };
    let seconds = elapsed / 1_000;
    let hours = seconds / 3_600;
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    let duration = if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else if seconds == 0 {
        "<1s".to_owned()
    } else {
        format!("{seconds}s")
    };
    Some(format!("Worked for {duration} · {terminal}"))
}

#[cfg(test)]
mod tests {
    fn summary(started: Option<u64>, completed: Option<u64>) -> Option<String> {
        super::summary(started, completed, Some(&grapt_client::Outcome::Succeeded))
    }
    #[test]
    fn unknown_start_omits_duration_without_hiding_known_completion() {
        assert_eq!(
            summary(None, Some(60_000)).as_deref(),
            Some("done 00:01 UTC")
        );
        assert_eq!(summary(Some(0), None), None);
        assert_eq!(
            summary(Some(60_001), Some(60_000)).as_deref(),
            Some("done 00:01 UTC")
        );
        assert_eq!(
            summary(Some(0), Some(0)).as_deref(),
            Some("Worked for <1s · done 00:00 UTC")
        );
    }
    #[test]
    fn durations_and_utc_day_rollover_follow_server_time() {
        assert_eq!(
            summary(Some(0), Some(1_390_000)).as_deref(),
            Some("Worked for 23m 10s · done 00:23 UTC")
        );
        assert_eq!(
            summary(Some(86_399_999), Some(86_400_001)).as_deref(),
            Some("Worked for <1s · done 00:00 UTC")
        );
        assert_eq!(
            summary(Some(0), Some(3_661_000)).as_deref(),
            Some("Worked for 1h 1m · done 01:01 UTC")
        );
    }
}
