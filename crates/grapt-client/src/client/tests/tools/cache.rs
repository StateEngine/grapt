use super::*;

fn cache_page(two: bool) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let mut messages = Vec::new();
    for (index, (input, read)) in [(100, 0), (900, 900)]
        .into_iter()
        .take(if two { 2 } else { 1 })
        .enumerate()
    {
        let row = crate::wire::assistant_message(
            &format!("msg_{:012}", index + 1),
            &[],
            Some(crate::wire::DurableFinish::Stop),
            None,
            None,
        )?;
        let mut row = serde_json::to_value(row)?;
        row["observedUsage"] = serde_json::json!({"inputTotal":input,"cacheRead":read});
        row["metadata"] = serde_json::json!({"cacheObservedCalls": {
            "scope":"a".repeat(64), "attempts":if two {3} else {1}, "unreported":0,
            "pairedInput":if two {1100} else {100}, "pairedRead":if two {1000} else {0},
            "knownWrite":0, "unreportedWrites":0, "lostAtLeast":0, "overflow":false,
            "cost":{"status":if two {"subtotal"} else {"estimated"},
                "usd":0.000123,"priced":1,"missingUsage":if two {1} else {0},
                "missingPricing":if two {1} else {0},"errors":0,"overflow":false,
                "policy":"pinned-v1","scope":"observed-dispatch-api-equivalent","subscription":"unknown"},
        }});
        messages.push(grapt_api::u3a::SessionMessageInfoDto::Assistant(Box::new(
            serde_json::from_value(row)?,
        )));
    }
    Ok(crate::wire::encode_messages(messages, None)?)
}

#[test]
fn tool_hint_updates_cache_without_erasing_live_text_and_reconnect_does_not_double_count(
) -> Result<(), Box<dyn std::error::Error>> {
    let session = crate::wire::session_id("ses_000000000001")?;
    let peer = ProgressPeer {
        inner: ContractDouble::new(),
        page: cache_page(false)?,
        frames: std::collections::VecDeque::new(),
        reads: 0,
        subscriptions: 0,
        closed: false,
    };
    let mut client = Client::new(peer, Bounds::PRODUCTION);
    client.select_first_offer()?;
    assert_eq!(client.create_session()?, session);
    prove(&mut client);
    assert_eq!(
        client.view().ok_or("view")?.cache.history_share(),
        Some(0.0)
    );
    for (kind, data) in [
        (
            "session.text.started",
            serde_json::json!({"assistantMessageID":"msg_fixture_answer","ordinal":0,"sessionID":session.as_inner().as_str()}),
        ),
        (
            "session.text.delta",
            serde_json::json!({"assistantMessageID":"msg_fixture_answer","ordinal":0,"sessionID":session.as_inner().as_str(),"delta":"UNFINISHED","from":0}),
        ),
    ] {
        let mut frame =
            serde_json::json!({"id":"evt_cache_live","type":kind,"created":1,"data":data});
        if crate::contract::EventMember::parse(kind).is_some_and(|member| member.requires_durable())
        {
            frame["durable"] =
                serde_json::json!({"aggregateID":session.as_inner().as_str(),"seq":1,"version":1});
        }
        let envelope = EventEnvelope::decode(&frame.to_string())?;
        client
            .views
            .get_mut(&session)
            .ok_or("view")?
            .apply(&envelope, Bounds::PRODUCTION)?;
    }
    let hint = serde_json::json!({
        "id":"evt_cache_hint","type":"session.tool.progress","created":1,
        "data":{"sessionID":session.as_inner().as_str(),"assistantMessageID":"msg_000000000001",
            "callID":"call_000000000001","metadata":{"toolCalls":[{"tool":"fixture.add","status":"running"}]}}
    }).to_string();
    client.peer_mut().page = cache_page(true)?;
    client.peer_mut().frames.push_back(FramePoll::Frame(hint));
    assert!(client.pump()?);
    let view = client.view().ok_or("view")?;
    assert!(view
        .slices
        .iter()
        .any(|s| !s.closed && s.text.contains("UNFINISHED")));
    let observed = view.cache;
    assert_eq!(observed.history_share(), Some(0.9));
    let details = crate::view::status::snapshot(&client).lines(200).join("\n");
    for part in [
        "1/3 priced",
        "missing usage 1, pricing 1",
        "subscription unknown",
    ] {
        assert!(details.contains(part), "{details}");
    }
    client.peer_mut().frames.push_back(FramePoll::Closed);
    client.pump()?;
    prove(&mut client);
    client.reconcile(&session)?;
    assert_eq!(client.view().ok_or("recovered view")?.cache, observed);
    Ok(())
}
