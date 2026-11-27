use super::{page, tests::*, *};
mod refusals;
fn binding_v2() -> TestResult<MailBinding> {
    let request = request()?;
    let request = OpenRequest::new(
        request.expected_local_project().clone(),
        request.expected_recipient().clone(),
        ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V2).ok_or("protocol")?,
        request.client_instance().clone(),
    );
    let mut value = binding_value();
    value["protocol"] = json!(REMOTE_SEAT_MAIL_TRANSPORT_V2);
    value["adapter_version"] = json!("stateengine-native-mail/2");
    Ok(decode::binding(&value, &request, profile())?)
}
fn gap() -> Value {
    json!({"msg_id":null,"start":0,"end":12,"reason":"MalformedEnvelope","source_cursor":"ref1.opaque"})
}
fn mixed() -> Value {
    json!({"messages":[row("one","handoff","wake")],"refused_rows":[gap()],"next_after":"ref1.opaque","last_cursor":"ref1.opaque"})
}
fn parsed(value: &Value, max: u32) -> TestResult<RemoteMailPage> {
    Ok(page::decode(
        value,
        &binding_v2()?,
        None,
        PageLimits::new(max, 262144),
        MailTransportVersion::V2,
    )?)
}
#[test]
fn nmv2_last_cursor_is_supplied_opaque_combined_progress() -> TestResult {
    let page = parsed(&mixed(), 64)?;
    assert!(page.has_more());
    assert_eq!(page.entry_count(), 2);
    assert_ne!(
        page.last_cursor(),
        page.messages().last().map(|m| m.source_cursor())
    );
    let mut value = mixed();
    value["next_after"] = Value::Null;
    assert_eq!(
        parsed(&value, 64)?.last_cursor().map(|c| c.as_str()),
        Some("ref1.opaque")
    );
    value["messages"] = json!([]);
    assert_eq!(parsed(&value, 64)?.entry_count(), 1);
    value["refused_rows"] = json!([]);
    value["last_cursor"] = Value::Null;
    assert_eq!(parsed(&value, 64)?.entry_count(), 0);
    Ok(())
}

#[test]
fn nmv2_finding_keeps_other_messages() -> TestResult {
    let value = json!({"messages":[row("one","handoff","wake"),row("two","finding","wake")],
        "refused_rows":[],"next_after":null,"last_cursor":"cursor-two"});
    let page = parsed(&value, 64)?;
    assert_eq!(page.messages().len(), 2);
    assert_eq!(page.messages()[0].kind(), &RemoteSeatMailKind::Handoff);
    assert_eq!(page.messages()[1].kind(), &RemoteSeatMailKind::Finding);
    Ok(())
}
#[test]
fn nmv2_closed_gap_shape_reason_id_range_and_cursor() -> TestResult {
    for (key, value) in [
        ("reason", json!("schema:unknown")),
        ("start", json!(-1)),
        ("end", json!(0)),
        ("end", json!(9_007_199_254_740_992_u64)),
        ("msg_id", json!("")),
        ("msg_id", json!("x".repeat(257))),
        ("source_cursor", Value::Null),
        ("source_cursor", json!("x".repeat(145))),
        ("unexpected", json!(true)),
    ] {
        let mut page = mixed();
        page["refused_rows"][0][key] = value;
        assert!(parsed(&page, 64).is_err(), "accepted invalid {key}");
    }
    let mut value = mixed();
    value["next_after"] = Value::Null;
    value["last_cursor"] = json!("unemitted");
    assert!(parsed(&value, 64).is_err());
    value = mixed();
    value["next_after"] = json!("different");
    assert!(parsed(&value, 64).is_err());
    value = mixed();
    value["refused_rows"] = json!([gap(), gap()]);
    assert!(parsed(&value, 64).is_err());
    Ok(())
}
#[test]
fn nmv2_combined_count_and_bytes_are_not_per_array() -> TestResult {
    assert!(parsed(&mixed(), 1).is_err());
    let gaps=(0..64).map(|i|json!({"msg_id":null,"start":i*2,"end":i*2+1,"reason":"malformed_json","source_cursor":format!("ref1.{i}")})).collect::<Vec<_>>();
    let mut value =
        json!({"messages":[],"refused_rows":gaps,"next_after":null,"last_cursor":"ref1.63"});
    assert_eq!(parsed(&value, 64)?.entry_count(), 64);
    value["messages"] = json!([row("one", "handoff", "wake")]);
    assert!(parsed(&value, 64).is_err());
    let mut value = mixed();
    value["messages"][0]["body"] = json!("x".repeat(600000));
    assert!(parsed(&value, 64).is_err());
    Ok(())
}
#[test]
fn nmv2_route_request_mismatch_refuses_before_network() -> TestResult {
    super::wire_tests::run(async {
        let mut port = HttpMailTransport::new(
            NonZeroU16::new(9).ok_or("port")?,
            Arc::new(Secret::new("fixture-secret".into())),
        )?;
        let old = request()?;
        let request = OpenRequest::new(
            old.expected_local_project().clone(),
            old.expected_recipient().clone(),
            ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V2).ok_or("protocol")?,
            old.client_instance().clone(),
        );
        let error = port
            .open(request, profile())
            .await
            .err()
            .ok_or("mismatch accepted")?;
        assert!(matches!(
            error.cause,
            OperationCause::Open(OpenFailure::ProtocolVersionUnsupported)
        ));
        Ok(())
    })
}

#[test]
fn nmv2_page_metadata_byte_ceiling_survives_valid_body_bounds() -> TestResult {
    let rows = (0..64)
        .map(|i| {
            let mut row = row(&format!("m{i}"), "handoff", "wake");
            row["body"] = json!("\u{0}".repeat(1500));
            row
        })
        .collect::<Vec<_>>();
    let value =
        json!({"messages":rows,"refused_rows":[],"next_after":null,"last_cursor":"cursor-m63"});
    let error = parsed(&value, 64)
        .err()
        .ok_or("oversize metadata accepted")?;
    let failure = error
        .downcast_ref::<OperationFailure>()
        .ok_or("wrong error layer")?;
    assert!(matches!(
        failure.cause,
        OperationCause::Transport(TransportFailure::PageDecodedBytesExceeded)
    ));
    Ok(())
}

#[test]
fn empty_body_row_is_admitted_beside_healthy_mail_with_opaque_progress() -> TestResult {
    for protocol in [MailTransportVersion::V1, MailTransportVersion::V2] {
        let binding = if protocol == MailTransportVersion::V2 {
            binding_v2()?
        } else {
            decode::binding(&binding_value(), &request()?, profile())?
        };
        for empty_last in [false, true] {
            let mut empty = row("empty", "handoff", "wake");
            empty["body"] = json!("");
            let healthy = row("healthy", "handoff", "wake");
            let messages = if empty_last {
                vec![healthy, empty]
            } else {
                vec![empty, healthy]
            };
            let last = messages.last().ok_or("last row")?["source_cursor"].clone();
            let mut value = json!({"messages":messages,"next_after":last});
            if protocol == MailTransportVersion::V2 {
                value["last_cursor"] = last.clone();
                value["refused_rows"] = json!([]);
            }
            let page = page::decode(
                &value,
                &binding,
                None,
                PageLimits::new(64, 262144),
                protocol,
            )?;
            // An empty body is still mail: admitted beside the healthy row,
            // never skipped behind a decode-failure count.
            assert_eq!(page.messages().len(), 2);
            assert!(page.decode_failures().is_empty());
            let empty = page
                .messages()
                .iter()
                .find(|message| message.source_message().as_str() == "empty")
                .ok_or("empty row admitted")?;
            assert!(empty.body().is_empty());
            assert_eq!(page.entry_count(), 2);
            assert_eq!(
                page.next_after().map(|cursor| cursor.as_str()),
                last.as_str()
            );
        }
    }
    Ok(())
}

#[test]
fn inadmissible_kind_label_skips_only_its_row_and_retains_opaque_progress() -> TestResult {
    for protocol in [MailTransportVersion::V1, MailTransportVersion::V2] {
        let binding = if protocol == MailTransportVersion::V2 {
            binding_v2()?
        } else {
            decode::binding(&binding_value(), &request()?, profile())?
        };
        for messages in [
            vec![
                row("unknown", "", "wake"),
                row("healthy", "handoff", "wake"),
            ],
            vec![
                row("healthy", "handoff", "wake"),
                row("unknown", "", "wake"),
            ],
            vec![row("unknown", "", "wake")],
        ] {
            let last = messages.last().ok_or("last")?["source_cursor"].clone();
            let healthy = messages
                .iter()
                .filter(|row| row["kind"] == "handoff")
                .count();
            let mut value = json!({"messages":messages,"next_after":last});
            if protocol == MailTransportVersion::V2 {
                value["last_cursor"] = last;
                value["refused_rows"] = json!([]);
            }
            let parsed = page::decode(
                &value,
                &binding,
                None,
                PageLimits::new(64, 262144),
                protocol,
            )?;
            assert_eq!(parsed.messages().len(), healthy);
            assert_eq!(parsed.decode_failures().len(), 1);
            assert_eq!(parsed.entry_count(), healthy + 1);
            assert!(parsed.refused_rows().is_empty());
            assert_eq!(
                parsed.next_after().map(|c| c.as_str()),
                value["next_after"].as_str()
            );
            assert_eq!(
                parsed.decode_failures()[0].cursor().as_str(),
                "cursor-unknown"
            );
        }
    }
    Ok(())
}

#[test]
fn inadmissible_kind_label_keeps_binding_structure_and_resource_checks_closed() -> TestResult {
    let mut value = json!({"messages":[row("unknown","","wake"),row("healthy","handoff","wake")],
        "refused_rows":[],"next_after":null,"last_cursor":"cursor-healthy"});
    assert!(parsed(&value, 1).is_err());
    assert!(page::decode(
        &value,
        &binding_v2()?,
        None,
        PageLimits::new(64, 1),
        MailTransportVersion::V2
    )
    .is_err());
    for (field, bad) in [
        ("recipient", json!("other-seat")),
        ("source_created", json!("")),
        ("body", json!(7)),
        ("unexpected", json!(true)),
    ] {
        let mut invalid = value.clone();
        invalid["messages"][0][field] = bad;
        assert!(
            parsed(&invalid, 64).is_err(),
            "accepted invalid unknown row {field}"
        );
    }
    value["messages"][1]["source_cursor"] = json!("cursor-unknown");
    value["last_cursor"] = json!("cursor-unknown");
    assert!(parsed(&value, 64).is_err());
    Ok(())
}
