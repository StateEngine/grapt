use super::*;

#[test]
fn older_cost_snapshots_never_add_again_or_erase_missing_coverage() -> Result<(), String> {
    let older = CostEstimate {
        usd: Some(0.000123),
        priced: 1,
        missing_usage: 1,
        missing_pricing: 1,
        errors: 0,
        overflow: false,
    };
    let newer = CostEstimate {
        usd: Some(0.000456),
        priced: 2,
        ..older
    };
    let base = ObservedCalls {
        cost: CallCost::Reported(older),
        scope: [b'a'; 64],
        attempts: 3,
        unreported: 0,
        input: 100,
        read: 90,
        write: Some(0),
        loss: 0,
        overflow: false,
    };
    for snapshots in [
        [
            base,
            ObservedCalls {
                cost: CallCost::Reported(newer),
                attempts: 4,
                ..base
            },
        ],
        [
            ObservedCalls {
                cost: CallCost::Reported(newer),
                attempts: 4,
                ..base
            },
            base,
        ],
    ] {
        let mut cache = CacheSummary::default();
        for snapshot in snapshots {
            cache.observe(CacheUsage {
                observed_calls: Some(CallObservation::Reported(snapshot)),
                ..CacheUsage::default()
            });
        }
        let status = cache.calls_status().ok_or("cost absent")?;
        assert!(
            status.contains("USD 0.000456") && status.contains("2/4 priced"),
            "{status}"
        );
        assert!(status.contains("missing usage 1, pricing 1"), "{status}");
    }
    Ok(())
}
