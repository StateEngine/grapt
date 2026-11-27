use super::*;

#[test]
fn recorded_door_outage_refusals_keep_their_closed_causes() -> TestResult {
    for (value, expected) in [
        (
            json!({"family":"TransportFailure","tag":"Disconnected","reason":"operation-aborted","effect":"none"}),
            OperationCause::Transport(TransportFailure::Disconnected),
        ),
        (
            json!({"family":"OpenFailure","tag":"Unauthenticated","reason":"capability-required"}),
            OperationCause::Open(OpenFailure::Unauthenticated),
        ),
    ] {
        let failure = failure::decode(&value, false)?;
        assert_eq!(failure.cause, expected);
        assert_eq!(failure.effect, WriteEffect::None);
    }
    Ok(())
}
use crate::ack::SecondaryProjectionState;
use grapt_types::{MailboxAdmissionSeq, MailboxMessageId, SeatRecipientId};
pub(super) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

pub(super) fn request() -> TestResult<OpenRequest> {
    Ok(OpenRequest::new(
        TargetProjectId::new("secode").ok_or("project")?,
        SeatRecipientId::new("seat-delta").ok_or("recipient")?,
        ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?,
        MailTransportClientInstanceId::new("client-1").ok_or("client")?,
    ))
}
pub(super) fn profile() -> ProviderLimits {
    crate::test_support::provider_at_ceilings()
}
pub(super) fn binding_value() -> Value {
    json!({"binding_id":"bind-1","protocol":REMOTE_SEAT_MAIL_TRANSPORT_V1,"adapter_version":"stateengine-native-mail/1","projects":{"source":"stateengine","target":"secode"},"verified_principal":"seat-delta","local_recipient":"seat-delta","mapping_version":"exact-seat-id/v1","takeover":{"generation":1,"displaced":null},"limits":decode::profile_json(profile()),"capabilities":{"atomic_subscribe_and_snapshot":true,"durable_delivery_ack":true,"bulk_dismiss":true,"explicit_project_pair":true,"opaque_authenticated_cursors":true,"advisory_payload_bytes":0,"coalesced_pending_signals":1}})
}
pub(super) fn binding() -> TestResult<MailBinding> {
    Ok(decode::binding(&binding_value(), &request()?, profile())?)
}
pub(super) fn row(id: &str, kind: &str, attention: &str) -> Value {
    json!({"protocol":REMOTE_SEAT_MAIL_TRANSPORT_V1,"source_binding":"bind-1","projects":{"source":"stateengine","target":"secode"},"source_message":id,"source_cursor":format!("cursor-{id}"),"sender":"seat-alpha","recipient":"seat-delta","kind":kind,"body":"A bounded UTF-8 reply: é 🦀","refs":["docs/one","docs/two"],"supersession":{"supersedes":null,"superseded_by":null},"archive":{"disposition":"active","reason":null},"source_created":"2026-10-13T15:00:00.000Z","attention":attention})
}
pub(super) fn ack() -> TestResult<DeliveryAdmissionAck> {
    Ok(DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
        ack_id: DeliveryAdmissionAckId::new("ack-1").ok_or("ack")?,
        source_binding: SourceBindingId::new("bind-1").ok_or("binding")?,
        projects: decode::pair(&json!({"source":"stateengine","target":"secode"}))?,
        source_message: RemoteMailboxMessageId::new("msg-1").ok_or("message")?,
        source_cursor: ExclusiveSourceCursor::new("cursor-msg-1").ok_or("cursor")?,
        recipient: SeatRecipientId::new("seat-delta").ok_or("recipient")?,
        local_message: MailboxMessageId::new("local-1").ok_or("local")?,
        local_admission: MailboxAdmissionSeq::FIRST,
        canonical_message_digest: MailboxMessageDigest::new("digest-1").ok_or("digest")?,
        protocol: ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?,
    }))
}
pub(super) fn ack_value(state: &str) -> Value {
    let written = matches!(state, "posted" | "already-present");
    let repair = !written && state != "unarmed";
    json!({"tag":"Recorded","receipt":{"ack_id":"ack-1","source_message":"msg-1","projects":{"source":"stateengine","target":"secode"}},"primary_delivery":{"witnessed":true},"floor_projection":{"state":state,"outcome":if written {"written"} else if state == "unreadable" {"unreadable"} else {"refused"},"disposition":if written {json!(state)} else {Value::Null},"reason":if written {Value::Null} else {json!(format!("secondary-{state}"))},"outcome_record":{"state":"unreadable"},"retry_required":repair,"retry":if repair {json!({"operation":"acknowledge_delivery","exact_coordinates_required":true})} else {Value::Null}}})
}

#[test]
fn original_row_project_is_preserved_and_ack_cannot_replace_it_with_binding_project() -> TestResult
{
    let binding = binding()?;
    let mut value = row("msg-1", "handoff", "wake");
    value["projects"]["source"] = json!("original-origin");
    let mail = decode::message(&value, &binding)?;
    assert_eq!(mail.projects().source().as_str(), "original-origin");
    assert_eq!(
        mail.projects().target(),
        binding.receipt().projects().target()
    );
    let old = ack()?;
    let original = DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
        ack_id: old.ack_id().clone(),
        source_binding: old.source_binding().clone(),
        projects: mail.projects().clone(),
        source_message: old.source_message().clone(),
        source_cursor: old.source_cursor().clone(),
        recipient: old.recipient().clone(),
        local_message: old.local_message().clone(),
        local_admission: old.local_admission(),
        canonical_message_digest: old.canonical_message_digest().clone(),
        protocol: old.protocol().clone(),
    });
    let mut reply = ack_value("posted");
    assert!(operations::decode_ack(&reply, &original).is_err());
    reply["receipt"]["projects"]["source"] = json!("original-origin");
    assert_eq!(
        operations::decode_ack(&reply, &original)?
            .receipt()
            .projects(),
        mail.projects()
    );
    value["projects"]["target"] = json!("foreign-target");
    assert!(decode::message(&value, &binding).is_err());
    Ok(())
}

#[test]
fn exact_authenticated_principal_cannot_be_replaced_by_recipient_or_project() -> TestResult {
    let request = request()?;
    assert_eq!(
        decode::binding(&binding_value(), &request, profile())?
            .receipt()
            .verified_principal()
            .as_str(),
        "seat-delta"
    );
    for (key, value, expected) in [
        (
            "verified_principal",
            json!("seat-foreign"),
            OpenFailure::ExactPrincipalMismatch,
        ),
        (
            "local_recipient",
            json!("seat-foreign"),
            OpenFailure::RecipientMismatch,
        ),
        (
            "mapping_version",
            json!("guess/v1"),
            OpenFailure::MappingVersionMismatch,
        ),
        (
            "protocol",
            json!("unknown/v2"),
            OpenFailure::ProtocolVersionUnsupported,
        ),
    ] {
        let mut v = binding_value();
        v[key] = value;
        assert_eq!(
            decode::binding(&v, &request, profile()).err(),
            Some(expected.into())
        );
    }
    let mut v = binding_value();
    v["projects"]["target"] = json!("foreign");
    assert_eq!(
        decode::binding(&v, &request, profile()).err(),
        Some(OpenFailure::ForeignProjectMismatch.into())
    );
    Ok(())
}

#[test]
fn incomplete_or_zero_profile_and_unknown_capabilities_fail_before_binding() -> TestResult {
    let request = request()?;
    let fields = decode::profile_json(profile());
    for field in fields.as_object().ok_or("profile")?.keys() {
        for absent in [true, false] {
            let mut v = binding_value();
            if absent {
                v["limits"].as_object_mut().ok_or("limits")?.remove(field);
            } else {
                v["limits"][field] = json!(0);
            }
            assert!(decode::binding(&v, &request, profile()).is_err(), "{field}");
        }
    }
    let mut v = binding_value();
    v["capabilities"]["durable_delivery_ack"] = json!(false);
    assert!(decode::binding(&v, &request, profile()).is_err());
    let mut v = binding_value();
    v["capabilities"]["extra"] = json!(true);
    assert!(decode::binding(&v, &request, profile()).is_err());
    Ok(())
}

#[test]
fn attention_is_mandatory_closed_and_never_inferred_from_kind_or_body() -> TestResult {
    let b = binding()?;
    for (kind, attention, expected) in [
        ("done", "quiet", MailAttention::Quiet),
        ("handoff", "wake", MailAttention::Wake),
        ("done", "wake", MailAttention::Wake),
        ("handoff", "quiet", MailAttention::Quiet),
    ] {
        let v = row("msg-1", kind, attention);
        assert_eq!(decode::message(&v, &b)?.attention(), expected);
    }
    for invalid in [Value::Null, json!(false), json!("unknown"), json!(1)] {
        let mut v = row("msg-1", "handoff", "wake");
        v["attention"] = invalid;
        assert!(decode::message(&v, &b).is_err());
    }
    let mut v = row("msg-1", "handoff", "wake");
    v.as_object_mut().ok_or("row")?.remove("attention");
    assert!(decode::message(&v, &b).is_err());
    Ok(())
}

#[test]
fn full_row_preserves_ordered_refs_and_rejects_foreign_or_unknown_envelopes() -> TestResult {
    let b = binding()?;
    let v = row("msg-1", "handoff", "wake");
    let m = decode::message(&v, &b)?;
    assert_eq!(
        m.refs()
            .as_slice()
            .iter()
            .map(BoundedMailRef::as_str)
            .collect::<Vec<_>>(),
        vec!["docs/one", "docs/two"]
    );
    for (key, value) in [
        ("recipient", json!("foreign")),
        ("source_binding", json!("foreign")),
        ("protocol", json!("foreign")),
        ("kind", json!("")),
        ("body", json!(false)),
        ("source_created", Value::Null),
    ] {
        let mut v = v.clone();
        v[key] = value;
        assert!(decode::message(&v, &b).is_err(), "{key}");
    }
    let mut v = v.clone();
    v["extra"] = json!(true);
    assert!(decode::message(&v, &b).is_err());
    Ok(())
}

#[test]
fn duplicate_json_keys_do_not_disappear_into_a_valid_envelope() -> TestResult {
    for input in [
        br#"{"ok":false,"ok":true,"value":{}}"#.as_slice(),
        br#"{"x":{"attention":"quiet","attention":"wake"}}"#,
        br#"{"x":{"a":1,"\u0061":2}}"#,
    ] {
        assert!(decode::strict_value(input).is_err());
    }
    let valid = br#"{"x":[{"a":"escaped \" quote, bracket ] and colon :"},{"a":2}],"y":{"a":3}}"#;
    assert_eq!(
        decode::strict_value(valid)?,
        serde_json::from_slice::<Value>(valid)?
    );
    assert!(decode::strict_value(br#"{"x":"\ud800"}"#).is_err());
    Ok(())
}

#[test]
fn primary_delivery_proof_survives_secondary_failure_but_never_missing_proof() -> TestResult {
    let ack = ack()?;
    for (state, expected) in [
        ("posted", SecondaryProjectionState::Posted),
        ("already-present", SecondaryProjectionState::AlreadyPresent),
        ("unarmed", SecondaryProjectionState::Unarmed),
        ("refused", SecondaryProjectionState::Refused),
        ("unreadable", SecondaryProjectionState::Unreadable),
    ] {
        let v = ack_value(state);
        let first = operations::decode_ack(&v, &ack)?;
        assert_eq!(first.receipt().projection(), Some(expected));
        let mut retry = v.clone();
        retry["tag"] = json!("ExactRetry");
        assert_eq!(
            operations::decode_ack(&retry, &ack)?.receipt().ack_id(),
            first.receipt().ack_id()
        );
        let mut invalid = v.clone();
        invalid["primary_delivery"]["witnessed"] = json!(false);
        assert!(operations::decode_ack(&invalid, &ack).is_err());
        let mut invalid = v.clone();
        invalid["receipt"]["source_message"] = json!("foreign");
        assert!(operations::decode_ack(&invalid, &ack).is_err());
        for field in [
            "outcome",
            "reason",
            "outcome_record",
            "retry_required",
            "retry",
        ] {
            let mut invalid = v.clone();
            invalid["floor_projection"][field] = json!(123);
            assert!(
                operations::decode_ack(&invalid, &ack).is_err(),
                "{state}/{field}"
            );
        }
    }
    Ok(())
}

#[test]
fn failure_envelope_preserves_uncertain_effect_and_distinct_source_states() -> TestResult {
    for (state, expected) in [
        ("absent", operation::SourceState::Absent),
        ("incomplete", operation::SourceState::Incomplete),
        ("unreadable", operation::SourceState::Unreadable),
    ] {
        let v = json!({"family":"TransportFailure","tag":"TransportUnavailable","source_state":state,"effect":"unknown-retry-exact"});
        let f = failure::decode(&v, true)?;
        assert_eq!(f.source_state, Some(expected));
        assert_eq!(f.effect, WriteEffect::UnknownRetryExact);
    }
    let partial = json!({"family":"TransportFailure","tag":"TransportUnavailable","dismissed":["msg-1"],"already_dismissed":["msg-2"],"effect":"unknown-retry-exact"});
    let f = failure::decode(&partial, true)?;
    assert_eq!(f.partial_dismiss.ok_or("partial")?.dismissed.len(), 1);
    let auth = json!({"family":"OpenFailure","tag":"Unauthenticated"});
    assert_eq!(
        failure::decode(&auth, true)?.cause,
        OperationCause::Open(OpenFailure::Unauthenticated)
    );
    let unknown = json!({"family":"OpenFailure","tag":"future"});
    assert!(failure::decode(&unknown, false).is_err());
    let malformed =
        json!({"family":"TransportFailure","tag":"TransportUnavailable","source_state":false});
    assert!(failure::decode(&malformed, true).is_err());
    Ok(())
}

#[test]
fn a_matching_binding_id_cannot_substitute_foreign_receipt_coordinates() -> TestResult {
    let actual = binding()?;
    let foreign =
        crate::test_support::binding_between("foreign", "grapt").ok_or("foreign fixture")?;
    assert_eq!(
        actual.receipt().binding_id(),
        foreign.receipt().binding_id()
    );
    let mut port = HttpMailTransport::new(
        NonZeroU16::MIN,
        Arc::new(Secret::new("fixture-secret".to_owned())),
    )?;
    port.binding = Some(actual.receipt().clone());
    assert!(port.check(&actual).is_ok());
    assert_eq!(
        port.check(&foreign).err(),
        Some(TransportFailure::Disconnected.into())
    );
    Ok(())
}
