use super::*;

// Cost page builders; only these projection cases use them.
fn cost_page(cost: serde_json::Value) -> Result<wire::MessagePage, String> {
    cost_page_for("msg_cost_capture", 3, cost)
}

fn cost_page_for(
    id: &str,
    attempts: u64,
    cost: serde_json::Value,
) -> Result<wire::MessagePage, String> {
    let message = wire::assistant_message(id, &[], Some(wire::DurableFinish::Stop), None, None)
        .map_err(|e| e.to_string())?;
    let mut value = serde_json::to_value(&message).map_err(|e| e.to_string())?;
    value["metadata"] = json!({"cacheObservedCalls":{
        "scope":"a".repeat(64),"attempts":attempts,"unreported":0,"pairedInput":100,"pairedRead":90,
        "knownWrite":0,"unreportedWrites":0,"lostAtLeast":0,"overflow":false,"cost":cost,
    }});
    let assistant: grapt_api::u3a::SessionMessageAssistantDto =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    let bytes = wire::encode_messages(
        vec![grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
            assistant,
        ))],
        None,
    )
    .map_err(|e| e.to_string())?;
    wire::decode_messages(&bytes).map_err(|e| e.to_string())
}

fn estimate() -> serde_json::Value {
    json!({"status":"subtotal","usd":0.000123,"priced":1,"missingUsage":1,"missingPricing":1,
        "errors":0,"overflow":false,"policy":"pinned-v1", "scope":"observed-dispatch-api-equivalent",
        "subscription":"unknown"})
}

#[test]
fn reopening_history_reuses_the_capture_cost_with_partial_coverage() -> Result<(), String> {
    let page = cost_page(estimate())?;
    let first = view(&page)?.cache;
    let reopened = view(&page)?.cache;
    assert_eq!(first, reopened);
    let status = reopened.calls_status().ok_or("cost missing")?;
    for part in [
        "subtotal API USD 0.000123",
        "1/3 priced",
        "missing usage 1, pricing 1",
        "subscription unknown",
    ] {
        assert!(status.contains(part), "{status}");
    }
    Ok(())
}

#[test]
fn legacy_unknown_and_malformed_cost_do_not_invent_zero_or_hide_cache_counts() -> Result<(), String>
{
    for (cost, expected) in [
        (json!("unavailable"), "API cost unknown"),
        (json!(null), "API cost unreadable"),
        (
            {
                let mut cost = estimate();
                cost["usd"] = json!(-1);
                cost
            },
            "API cost unreadable",
        ),
        (
            {
                let mut cost = estimate();
                cost["priced"] = json!(3);
                cost
            },
            "API cost unreadable",
        ),
        (
            {
                let mut cost = estimate();
                cost["status"] = json!("estimated");
                cost
            },
            "API cost unreadable",
        ),
    ] {
        let status = view(&cost_page(cost)?)?
            .cache
            .calls_status()
            .ok_or("call projection absent")?;
        assert!(
            status.contains(expected) && status.contains("calls 90.0%"),
            "{status}"
        );
        assert!(!status.contains("USD 0.000000"), "{status}");
    }
    Ok(())
}

#[test]
fn a_retained_coverage_gap_remains_a_subtotal_after_refresh_and_reopen() -> Result<(), String> {
    for gap in ["missingUsage", "missingPricing", "errors"] {
        let mut older = estimate();
        older["status"] = json!("unavailable");
        older["usd"] = json!(null);
        older["priced"] = json!(0);
        older["missingUsage"] = json!(0);
        older["missingPricing"] = json!(0);
        older[gap] = json!(1);
        let mut newer = estimate();
        newer["status"] = json!("estimated");
        newer["priced"] = json!(2);
        newer["missingUsage"] = json!(0);
        newer["missingPricing"] = json!(0);
        for stale_last in [false, true] {
            let mut page = cost_page_for("msg_cost_older", 1, older.clone())?;
            page.messages
                .extend(cost_page_for("msg_cost_newer", 2, newer.clone())?.messages);
            if stale_last {
                page.messages.reverse();
            }
            let first = view(&page)?.cache;
            let reopened = view(&page)?.cache;
            assert_eq!(first, reopened);
            let status = reopened.calls_status().ok_or("cost absent")?;
            assert!(
                status.contains("subtotal API USD") && !status.contains("estimated API USD"),
                "{gap}, stale last {stale_last}: {status}"
            );
            assert!(status.contains("2/2 priced"), "{status}");
            let label = match gap {
                "missingUsage" => "missing usage 1",
                "missingPricing" => "pricing 1",
                _ => "errors 1",
            };
            assert!(status.contains(label), "{status}");
        }
    }
    Ok(())
}

#[test]
fn positive_submicrodollar_estimates_remain_visible_in_both_cost_summaries() -> Result<(), String> {
    for (usd, amount) in [
        (0.0, "0.000000"),
        (0.0000001, "<0.000001"),
        (0.0000009, "<0.000001"),
        (0.000001, "0.000001"),
        (0.012345, "0.012345"),
    ] {
        let mut history = CacheSummary::default();
        history.observe(CacheUsage {
            estimated_usd: Some(usd),
            ..CacheUsage::default()
        });
        history.observe(CacheUsage::default());
        let status = history.cost_status();
        assert!(status.contains(&format!("API USD {amount}")), "{status}");
        assert!(
            status.contains("subtotal; 1 unpriced; history incomplete"),
            "{status}"
        );
        assert!(status.contains("retries/compaction excluded"), "{status}");

        let mut cost = estimate();
        cost["usd"] = json!(usd);
        let status = view(&cost_page(cost)?)?
            .cache
            .calls_status()
            .ok_or("cost missing")?;
        assert!(
            status.contains(&format!("subtotal API USD {amount}")),
            "{status}"
        );
        for label in [
            "1/3 priced",
            "missing usage 1, pricing 1",
            "subscription unknown",
        ] {
            assert!(status.contains(label), "{status}");
        }
    }
    Ok(())
}
