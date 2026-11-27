use super::*;
use crate::wire;
use serde_json::json;

fn page(observations: &[Option<serde_json::Value>]) -> Result<wire::MessagePage, String> {
    page_with_legacy(observations, Some((100, 1)))
}

fn page_with_legacy(
    observations: &[Option<serde_json::Value>],
    legacy: Option<(u64, u64)>,
) -> Result<wire::MessagePage, String> {
    let mut rows = Vec::new();
    for (index, observation) in observations.iter().enumerate() {
        let message = wire::assistant_message(
            &format!("msg_{:012}", index + 1),
            &[],
            Some(wire::DurableFinish::Stop),
            legacy,
            None,
        )
        .map_err(|error| error.to_string())?;
        let mut value = serde_json::to_value(message).map_err(|error| error.to_string())?;
        if let Some(observation) = observation {
            value["observedUsage"] = observation.clone();
        }
        rows.push(grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
            serde_json::from_value(value).map_err(|error| error.to_string())?,
        )));
    }
    let bytes = wire::encode_messages(rows, None).map_err(|error| error.to_string())?;
    wire::decode_messages(&bytes).map_err(|error| error.to_string())
}

#[test]
fn a_later_hit_or_unknown_usage_keeps_the_last_miss_diagnostic() -> Result<(), String> {
    let mut cache = CacheSummary::default();
    assert_eq!(cache.miss_status(), None);
    cache.observe(CacheUsage {
        input: Some(100),
        read: Some(40),
        request_change: Some(RequestChange::Tools),
        ..CacheUsage::default()
    });
    let miss = cache.miss_status().ok_or("miss absent")?;
    assert!(miss.contains("cause unreported") && miss.contains("tools changed"));
    cache.observe(CacheUsage {
        input: Some(100),
        read: Some(100),
        request_change: Some(RequestChange::Identical),
        ..CacheUsage::default()
    });
    cache.observe(CacheUsage {
        usage_warning: Some(grapt_types::failure_diagnostic::UsageDiagnostic {
            field: None,
            issue: grapt_types::failure_diagnostic::UsageIssue::Null,
        }),
        ..CacheUsage::default()
    });
    assert!(cache.status().contains("warning: provider usage"));
    assert!(cache.status().contains("answer preserved"));
    assert_eq!(cache.miss_status(), Some(miss));
    cache.observe(CacheUsage {
        input: Some(100),
        read: Some(80),
        request_change: Some(RequestChange::AppendOnly),
        ..CacheUsage::default()
    });
    assert!(cache
        .miss_status()
        .is_some_and(|miss| miss.contains("input appended")));
    Ok(())
}

#[test]
fn inconsistent_reported_partitions_refuse_before_display() {
    for observation in [
        json!({"inputTotal":100,"cacheRead":101}),
        json!({"inputTotal":100,"inputFresh":20,"cacheRead":80,"cacheWrite":10}),
        json!({"inputFresh":grapt_api::u3a::SafeTokenCount::MAX,"cacheRead":1,"cacheWrite":0}),
    ] {
        assert!(page(&[Some(observation)]).is_err());
    }
}

#[test]
fn session_overflow_stays_unavailable_instead_of_saturating_to_a_ratio() {
    let mut cache = CacheSummary::default();
    cache.history_complete(true);
    for _ in 0..2049 {
        cache.observe(CacheUsage {
            input: Some(grapt_api::u3a::SafeTokenCount::MAX),
            read: Some(grapt_api::u3a::SafeTokenCount::MAX),
            ..CacheUsage::default()
        });
    }
    assert_eq!(cache.history_share(), None);
    assert!(cache.status().contains("accounting overflow"));
}

#[test]
fn a_truncated_history_cannot_claim_the_session_cache_share() {
    let mut cache = CacheSummary::default();
    cache.observe(CacheUsage {
        input: Some(100),
        read: Some(99),
        ..CacheUsage::default()
    });
    assert_eq!(cache.latest_share(), Some(0.99));
    assert_eq!(cache.history_share(), None);
    assert!(cache.status().contains("history incomplete"));
}

#[test]
fn all_dispatch_totals_are_folded_once_and_remain_honest_under_unknowns() -> Result<(), String> {
    let observed = ObservedCalls {
        cost: CallCost::Unknown,
        scope: [b'a'; 64],
        attempts: 3,
        unreported: 0,
        input: 1100,
        read: 1000,
        write: Some(0),
        loss: 0,
        overflow: false,
    };
    let mut cache = CacheSummary::default();
    for _ in 0..2 {
        cache.observe(CacheUsage {
            input: Some(100),
            read: Some(90),
            observed_calls: Some(CallObservation::Reported(observed)),
            ..CacheUsage::default()
        });
    }
    let status = cache.calls_status().ok_or("all-call status absent")?;
    assert!(
        status.contains("90.9%")
            && status.contains("uncached 100")
            && status.contains("3 observed"),
        "{status}"
    );
    for changed in [
        ObservedCalls {
            unreported: 1,
            ..observed
        },
        ObservedCalls {
            loss: 1,
            ..observed
        },
        ObservedCalls {
            overflow: true,
            ..observed
        },
    ] {
        let mut cache = CacheSummary::default();
        cache.observe(CacheUsage {
            observed_calls: Some(CallObservation::Reported(changed)),
            ..CacheUsage::default()
        });
        let status = cache
            .calls_status()
            .ok_or("incomplete call status absent")?;
        assert!(status.starts_with("calls - · uncached -"), "{status}");
    }
    cache.observe(CacheUsage {
        observed_calls: Some(CallObservation::Reported(ObservedCalls {
            scope: [b'b'; 64],
            ..observed
        })),
        ..CacheUsage::default()
    });
    assert!(cache
        .calls_status()
        .ok_or("scope status absent")?
        .contains("capture changed"));
    Ok(())
}

#[test]
fn unreadable_all_call_evidence_cannot_be_shown_as_absent_or_zero() {
    let mut cache = CacheSummary::default();
    assert_eq!(cache.calls_status(), None);
    cache.observe(CacheUsage {
        observed_calls: Some(CallObservation::Unreadable),
        ..CacheUsage::default()
    });
    assert_eq!(
        cache.calls_status(),
        Some("calls - · accounting data unreadable".to_owned())
    );
}

#[test]
fn stale_call_snapshots_cannot_restore_a_percentage_after_evidence_was_lost() -> Result<(), String>
{
    let complete = ObservedCalls {
        cost: CallCost::Unknown,
        scope: [b'a'; 64],
        attempts: 3,
        unreported: 0,
        input: 100,
        read: 90,
        write: Some(0),
        loss: 0,
        overflow: false,
    };
    for incomplete in [
        ObservedCalls {
            loss: 1,
            ..complete
        },
        ObservedCalls {
            overflow: true,
            ..complete
        },
        ObservedCalls {
            unreported: 1,
            ..complete
        },
    ] {
        for stale_attempts in [2, 3] {
            let stale = ObservedCalls {
                attempts: stale_attempts,
                ..complete
            };
            for snapshots in [[incomplete, stale], [stale, incomplete]] {
                let mut cache = CacheSummary::default();
                for snapshot in snapshots {
                    cache.observe(CacheUsage {
                        observed_calls: Some(CallObservation::Reported(snapshot)),
                        ..CacheUsage::default()
                    });
                }
                let status = cache.calls_status().ok_or("call totals missing")?;
                assert!(status.starts_with("calls - · uncached -"), "{status}");
                assert!(status.contains("3 observed"), "{status}");
            }
        }
    }
    let unknown_write = ObservedCalls {
        write: None,
        ..complete
    };
    for snapshots in [[unknown_write, complete], [complete, unknown_write]] {
        let mut cache = CacheSummary::default();
        for snapshot in snapshots {
            cache.observe(CacheUsage {
                observed_calls: Some(CallObservation::Reported(snapshot)),
                ..CacheUsage::default()
            });
        }
        let status = cache.calls_status().ok_or("call totals missing")?;
        assert!(
            status.contains("calls 90.0%") && status.contains("write -"),
            "{status}"
        );
    }
    Ok(())
}

#[test]
fn cost_tracks_paged_assistant_subtotals_without_claiming_a_full_seat_day() {
    let row = |estimated_usd| CacheUsage {
        estimated_usd,
        input: Some(100),
        read: Some(90),
        ..CacheUsage::default()
    };
    let mut older = CacheSummary::default();
    older.observe(row(Some(0.25)));
    let mut newer = CacheSummary::default();
    newer.observe(row(Some(0.5)));
    newer.prepend(older);
    assert!(newer.cost_status().contains("0.750000"));
    assert!(newer.cost_status().contains("history incomplete"));
    newer.history_complete(true);
    assert!(newer.cost_status().contains("assistant history"));
    let settled = newer;
    newer.refresh(older);
    assert_eq!(
        newer, settled,
        "an older tool refresh cannot drop settled costs"
    );
    newer.observe(row(None));
    let status = newer.cost_status();
    assert!(status.contains("subtotal") && status.contains("1 unpriced"));
    let mut absent = CacheSummary::default();
    absent.observe(row(None));
    assert!(absent.cost_status().starts_with("unavailable"));
    let mut overflow = CacheSummary::default();
    overflow.observe(row(Some(f64::MAX)));
    overflow.observe(row(Some(f64::MAX)));
    assert!(overflow.cost_status().contains("overflow"));
}

mod calls_cost;
