//! Cache cases that fold decoded pages through the projection; the decoders
//! they exercise live in grapt-client-transport.
use crate::cache::*;
use crate::{wire, Bounds, SessionView};
use serde_json::json;

// Page builders, duplicated from the cache unit tests below the transport line.
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

fn view(page: &wire::MessagePage) -> Result<SessionView, String> {
    let session = wire::session_id("ses_000000000001").map_err(|error| error.to_string())?;
    let mut view = SessionView::new(&session);
    for message in &page.messages {
        view.apply_durable(message, Bounds::PRODUCTION)
            .map_err(|error| error.to_string())?;
    }
    view.cache.history_complete(page.next.is_none());
    Ok(view)
}

#[test]
fn history_share_weights_input_and_rebuilds_without_double_counting() -> Result<(), String> {
    let messages = page(&[
        Some(json!({"inputTotal":100,"cacheRead":0})),
        Some(json!({"inputTotal":900,"cacheRead":900})),
    ])?;
    let first = view(&messages)?.cache;
    assert_eq!(first.latest_share(), Some(1.0));
    assert_eq!(first.history_share(), Some(0.9));
    assert_eq!(first, view(&messages)?.cache);
    assert!(first.status().contains("history 90.0%"));
    Ok(())
}

#[test]
fn cache_write_tokens_belong_in_the_anthropic_input_denominator() -> Result<(), String> {
    let messages = page(&[Some(json!({
        "inputFresh":10,"cacheRead":90,"cacheWrite":100
    }))])?;
    assert_eq!(view(&messages)?.cache.history_share(), Some(0.45));
    Ok(())
}

#[test]
fn missing_reads_are_unknown_and_do_not_borrow_legacy_zeroes() -> Result<(), String> {
    for observation in [None, Some(json!({"inputTotal":100}))] {
        let cache = view(&page(&[observation])?)?.cache;
        assert_eq!(cache.latest_share(), None);
        assert_eq!(cache.history_share(), None);
        assert!(cache.status().contains("unreported"));
    }
    let cache = view(&page(&[Some(json!({"inputTotal":100,"cacheRead":0}))])?)?.cache;
    assert_eq!(cache.history_share(), Some(0.0));
    assert!(cache.status().contains("cause unreported"));
    Ok(())
}

#[test]
fn partial_history_cannot_display_a_complete_history_share() -> Result<(), String> {
    let messages = page(&[None, Some(json!({"inputTotal":100,"cacheRead":95}))])?;
    let cache = view(&messages)?.cache;
    assert_eq!(cache.latest_share(), Some(0.95));
    assert_eq!(cache.history_share(), None);
    assert!(cache.status().contains("1 message unreported"));
    Ok(())
}

#[test]
fn zero_input_is_measured_without_division_or_a_fake_hit() -> Result<(), String> {
    let messages = page(&[Some(json!({"inputTotal":0,"cacheRead":0}))])?;
    let cache = view(&messages)?.cache;
    assert_eq!(cache.latest_share(), None);
    assert!(cache.status().contains("no input"));
    Ok(())
}

#[test]
fn cache_reads_do_not_require_cost_or_complete_compatibility_usage() -> Result<(), String> {
    let messages = page_with_legacy(&[Some(json!({"inputTotal":100,"cacheRead":95}))], None)?;
    let view = view(&messages)?;
    assert_eq!(view.usage, crate::Usage::default());
    assert_eq!(view.cache.history_share(), Some(0.95));
    Ok(())
}

#[test]
fn an_unmeasured_draft_preserves_the_last_completed_observation() -> Result<(), String> {
    let messages = page(&[Some(json!({"inputTotal":100,"cacheRead":95}))])?;
    let mut view = view(&messages)?;
    for finish in [None, Some(wire::DurableFinish::Unknown)] {
        let draft = wire::DurableMessage::Assistant {
            items: Vec::new(),
            finish,
            usage: None,
            cache: Box::default(),
            footer: None,
            error: None,
        };
        view.apply_durable(&draft, Bounds::PRODUCTION)
            .map_err(|e| e.to_string())?;
        assert_eq!(view.cache.latest_share(), Some(0.95));
        assert_eq!(view.cache.history_share(), Some(0.95));
    }
    Ok(())
}

#[test]
fn request_diagnostic_metadata_is_closed_and_does_not_invent_a_miss_cause() -> Result<(), String> {
    for (kind, index, shown) in [
        ("tools_changed", None, Some("tools changed")),
        ("input_changed", Some("2"), Some("input item 3 changed")),
        (
            "input_shortened",
            Some("512"),
            Some("input shortened at item 513"),
        ),
        ("identical", None, Some("identical body")),
        ("first_observation", None, Some("no previous comparison")),
        ("input_changed", Some("65535"), None),
        ("input_changed", None, None),
        ("untrusted arbitrary text", None, None),
    ] {
        let message = wire::assistant_message(
            "msg_000000000001",
            &[],
            Some(wire::DurableFinish::Stop),
            None,
            None,
        )
        .map_err(|error| error.to_string())?;
        let mut value = serde_json::to_value(message).map_err(|error| error.to_string())?;
        value["observedUsage"] = json!({"inputTotal":100,"cacheRead":40});
        value["metadata"] = json!({"cacheRequestChange":kind,"cacheRequestChangedItem":index});
        let message = serde_json::from_value(value).map_err(|error| error.to_string())?;
        let bytes = wire::encode_messages(
            vec![grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
                message,
            ))],
            None,
        )
        .map_err(|error| error.to_string())?;
        let messages = wire::decode_messages(&bytes).map_err(|error| error.to_string())?;
        let cache = view(&messages)?.cache;
        assert_eq!(cache.latest_share(), Some(0.4));
        let status = cache.status();
        assert!(status.contains("cause unreported"));
        if let Some(shown) = shown {
            assert!(status.contains(shown), "{status}");
        } else {
            assert!(!status.contains("request:"), "{status}");
        }
    }
    Ok(())
}

#[test]
fn live_cost_wire_keeps_zero_and_refuses_billed_negative_or_unbound_estimates() -> Result<(), String>
{
    for (cost, expected) in [
        (
            json!({"status":"estimated","usd":0.0,"policy":"pinned","scope":"assistant-message-api-equivalent"}),
            Some(0.0),
        ),
        (
            json!({"status":"estimated","usd":0.25,"policy":"pinned","scope":"assistant-message-api-equivalent"}),
            Some(0.25),
        ),
        (
            json!({"status":"billed","usd":0.25,"policy":"pinned","scope":"assistant-message-api-equivalent"}),
            None,
        ),
        (
            json!({"status":"estimated","usd":-1.0,"policy":"pinned","scope":"assistant-message-api-equivalent"}),
            None,
        ),
        (
            json!({"status":"estimated","usd":1.0,"scope":"assistant-message-api-equivalent"}),
            None,
        ),
        (
            json!({"status":"estimated","usd":1.0,"policy":"pinned","scope":"full-seat-day"}),
            None,
        ),
        (
            json!({"status":"unavailable","reason":"usage-missing"}),
            None,
        ),
    ] {
        let message = wire::assistant_message(
            "msg_000000000001",
            &[],
            Some(wire::DurableFinish::Stop),
            None,
            None,
        )
        .map_err(|error| error.to_string())?;
        let mut value = serde_json::to_value(message).map_err(|error| error.to_string())?;
        value["observedUsage"] = json!({"inputTotal":100,"cacheRead":90});
        value["metadata"] = json!({"estimatedCost":cost});
        let message = serde_json::from_value(value).map_err(|error| error.to_string())?;
        let bytes = wire::encode_messages(
            vec![grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
                message,
            ))],
            None,
        )
        .map_err(|error| error.to_string())?;
        let page = wire::decode_messages(&bytes).map_err(|error| error.to_string())?;
        assert_eq!(view(&page)?.cache.latest.estimated_usd, expected);
    }
    Ok(())
}

#[test]
fn last_provider_context_includes_output_and_does_not_borrow_absent_usage() -> Result<(), String> {
    for (observation, expected) in [
        (
            Some(json!({"inputTotal":100,"output":25,"total":130})),
            Some(130),
        ),
        (Some(json!({"inputTotal":100,"output":25})), Some(125)),
        (
            Some(json!({"inputFresh":10,"cacheRead":90,"cacheWrite":100,"output":25})),
            Some(225),
        ),
        (Some(json!({"inputTotal":100})), Some(100)),
        (Some(json!({"output":25})), None),
        (Some(json!({"total":130})), Some(130)),
        (None, None),
    ] {
        let messages = page(&[Some(json!({"inputTotal":999,"output":99})), observation])?;
        assert_eq!(view(&messages)?.cache.latest.context, expected);
    }
    Ok(())
}

mod calls_cost;
