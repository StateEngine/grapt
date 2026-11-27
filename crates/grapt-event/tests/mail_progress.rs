use grapt_event::{mail_progress::*, *};
use grapt_types::{
    remote_mail::{progress::*, *},
    *,
};
use serde_json::json;
type TestResult = Result<(), Box<dyn std::error::Error>>;
fn fact() -> Result<MailPageRecorded, Box<dyn std::error::Error>> {
    let stream = MailStream {
        projects: ProjectPair::new(
            SourceProjectId::try_new("source")?,
            TargetProjectId::try_new("secode")?,
        ),
        principal: SourcePrincipalId::try_new("coder")?,
        recipient: SeatRecipientId::try_new("coder")?,
        protocol: MailTransportVersion::V2,
    };
    let gap = RefusedMailRow::new(
        None,
        0,
        8,
        RefusedRowReason::MalformedEnvelope,
        ExclusiveSourceCursor::try_new("ref1.opaque")?,
    )
    .ok_or("gap")?;
    Ok(MailPageRecorded::new(
        stream,
        None,
        gap.cursor().clone(),
        vec![],
        vec![gap],
    )?)
}
#[test]
fn nmv2_local_progress_roundtrip_is_not_a_message_or_session_fact() -> TestResult {
    let fact = fact()?;
    assert_eq!(MailPageRecorded::decode(&fact.encode())?, fact);
    let aggregate = AggregateId::Mail(MailStreamId::try_new("mail-stream")?);
    let epoch = WriterEpoch::new(1).ok_or("epoch")?;
    let envelope = EventEnvelopeV2::new(
        "evt_v2".parse()?,
        aggregate.clone(),
        EventSeq::new(0),
        epoch,
        EnvelopeKind::NativeMail(EventKind::MailPageRecordedV1),
        Timestamp::from_millis(17),
        canonical_string(&fact.encode()).into_bytes(),
    )?;
    assert_eq!(
        decode_canonical(&envelope.canonical_bytes())?.canonical_bytes(),
        envelope.canonical_bytes()
    );
    assert!(EventEnvelopeV2::new(
        "evt_wrong".parse()?,
        AggregateId::Session("ses_v2".parse()?),
        EventSeq::new(0),
        epoch,
        EnvelopeKind::RustSession(EventKind::MailPageRecordedV1),
        Timestamp::from_millis(17),
        vec![]
    )
    .is_err());
    assert!(EventEnvelopeV2::new(
        "evt_wrong".parse()?,
        aggregate,
        EventSeq::new(0),
        epoch,
        EnvelopeKind::NativeMail(EventKind::SessionCreatedV1),
        Timestamp::from_millis(17),
        vec![]
    )
    .is_err());
    Ok(())
}
#[test]
fn nmv2_local_progress_rejects_unknown_gap_and_unemitted_checkpoint() -> TestResult {
    let fact = fact()?;
    for (field, value) in [
        ("transport", json!("future")),
        ("checkpoint", json!("unemitted")),
        ("unexpected", json!(true)),
    ] {
        let mut v = fact.encode();
        v[field] = value;
        assert!(MailPageRecorded::decode(&v).is_err());
    }
    let mut v = fact.encode();
    v["integrity_gaps"][0]["cause"] = json!("schema:unknown");
    assert!(MailPageRecorded::decode(&v).is_err());
    let mut v = fact.encode();
    v["integrity_gaps"][0]["byte_range"] = json!([4, 4]);
    assert!(MailPageRecorded::decode(&v).is_err());
    Ok(())
}

#[test]
fn local_decode_failures_are_distinct_bounded_progress_and_legacy_hash_is_unchanged() -> TestResult
{
    let original = fact()?;
    let legacy = original.encode();
    assert_eq!(legacy.as_object().ok_or("object")?.len(), 6);
    assert!(legacy.get("decode_failures").is_none());
    assert_eq!(MailPageRecorded::decode(&legacy)?.encode(), legacy);
    let cursor = ExclusiveSourceCursor::try_new("opaque-local-unknown-kind")?;
    let failure = MailDecodeFailure::unknown_kind(cursor.clone()).ok_or("failure")?;
    let recorded = MailPageRecorded::new_with_decode_failures(
        original.stream.clone(),
        None,
        cursor.clone(),
        vec![],
        vec![],
        vec![failure.clone()],
    )?;
    assert!(recorded.gaps().is_empty());
    assert!(recorded.messages().is_empty());
    assert_eq!(MailPageRecorded::decode(&recorded.encode())?, recorded);
    let payload = recorded.encode();
    assert_eq!(
        payload["decode_failures"],
        json!([{"cause":"unknown-kind","position":cursor.as_str()}])
    );
    for bad in [
        json!([]),
        json!([{"cause":"future","position":cursor.as_str()}]),
        json!([{"cause":"unknown-kind","position":cursor.as_str(),"start":0}]),
    ] {
        let mut value = payload.clone();
        value["decode_failures"] = bad;
        assert!(MailPageRecorded::decode(&value).is_err());
    }
    let mut value = payload;
    value["checkpoint"] = json!("unemitted");
    assert!(MailPageRecorded::decode(&value).is_err());
    let mut v1 = original.stream;
    v1.protocol = MailTransportVersion::V1;
    assert!(MailPageRecorded::new_with_decode_failures(
        v1,
        None,
        cursor,
        vec![],
        vec![],
        vec![failure]
    )
    .is_err());
    Ok(())
}
