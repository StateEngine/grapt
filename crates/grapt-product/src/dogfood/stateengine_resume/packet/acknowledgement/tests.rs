use super::*;
use crate::dogfood::stateengine_resume::packet::{tests::Fixture, Cache, Pointer};
use serde_json::json;

fn receipt(packet: &Verified, session: &SessionId) -> Value {
    json!({"packet_acknowledgement":{
        "state":"acknowledged", "packet_id":packet.id(), "recorded":false,
        "packet_sha256":packet.pointer.sha256, "project_id":"fixture",
        "agent_id":"agt_fixture", "session_id":session.as_str(),
        "acknowledgement_id":"b".repeat(64),
        "acknowledged_ts":"2026-10-29T06:00:00.123Z"
    }})
}

#[test]
fn prior_durable_ack_survives_a_fresh_packet_cache() -> Result<(), Box<dyn std::error::Error>> {
    let body = format!("# StateEngine\npacket_id: {}\n", "a".repeat(64));
    let fixture = Fixture::new(body.as_bytes())?;
    let pointer = Pointer::parse(&fixture.pointer(body.as_bytes()).to_string())?;
    let session: SessionId = "ses_packet_restart".parse()?;
    let mut cache = Cache::default();
    let packet = cache.load(&session, "binding", &pointer)?;
    let mut first = receipt(&packet, &session);
    first["packet_acknowledgement"]["recorded"] = json!(true);
    verify(&packet, &first, "fixture", "agt_fixture", &session)?;
    drop(packet);
    drop(cache);

    let mut restarted = Cache::default();
    let packet = restarted.load(&session, "binding", &pointer)?;
    let prior = receipt(&packet, &session);
    verify(&packet, &prior, "fixture", "agt_fixture", &session)?;
    assert_eq!(restarted.entries.len(), 1);
    // A fresh process can verify the producer's receipt without locally caching it.
    assert!(!format!("{packet:?}").contains("acknowledgement_id"));
    Ok(())
}

#[test]
fn prior_ack_requires_every_bound_coordinate() -> Result<(), Box<dyn std::error::Error>> {
    let body = format!("packet_id: {}\n", "a".repeat(64));
    let fixture = Fixture::new(body.as_bytes())?;
    let pointer = Pointer::parse(&fixture.pointer(body.as_bytes()).to_string())?;
    let session: SessionId = "ses_packet_proof".parse()?;
    let packet = Cache::default().load(&session, "binding", &pointer)?;
    let good = receipt(&packet, &session);
    for (key, value) in [
        ("state", json!("refused")),
        ("packet_id", json!("c".repeat(64))),
        ("packet_sha256", json!("d".repeat(64))),
        ("project_id", json!("other")),
        ("agent_id", json!("agt_other")),
        ("session_id", json!("ses_other")),
        ("session_id", json!(42)),
        ("recorded", json!("false")),
        ("acknowledgement_id", json!("b".repeat(63))),
        ("acknowledgement_id", json!("G".repeat(64))),
        ("acknowledged_ts", json!("not-a-time")),
        ("acknowledged_ts", json!("2026-02-29T06:00:00.123Z")),
        ("unexpected", json!(true)),
    ] {
        let mut bad = good.clone();
        bad["packet_acknowledgement"][key] = value;
        assert_eq!(
            verify(&packet, &bad, "fixture", "agt_fixture", &session),
            Err(E::PacketAcknowledgement),
            "accepted changed {key}"
        );
    }
    let object = good["packet_acknowledgement"]
        .as_object()
        .ok_or("missing receipt fixture")?;
    for key in object.keys() {
        let mut bad = good.clone();
        bad["packet_acknowledgement"]
            .as_object_mut()
            .ok_or("missing receipt fixture")?
            .remove(key);
        assert_eq!(
            verify(&packet, &bad, "fixture", "agt_fixture", &session),
            Err(E::PacketAcknowledgement),
            "accepted missing {key}"
        );
    }
    // A partial modern receipt cannot masquerade as an old fresh-append response.
    let mut partial = json!({"packet_acknowledgement":{
        "state":"acknowledged", "packet_id":packet.id(), "recorded":true,
        "packet_sha256":pointer.sha256
    }});
    assert_eq!(
        verify(&packet, &partial, "fixture", "agt_fixture", &session),
        Err(E::PacketAcknowledgement)
    );
    partial["packet_acknowledgement"] = json!(null);
    assert_eq!(
        verify(&packet, &partial, "fixture", "agt_fixture", &session),
        Err(E::PacketAcknowledgement)
    );
    Ok(())
}

#[test]
fn receipt_time_requires_the_producer_iso_timestamp() {
    for good in ["2026-10-29T06:00:00.123Z", "2000-02-29T23:59:59.999Z"] {
        assert!(timestamp(good), "rejected {good}");
    }
    for bad in [
        "2026-10-29T06:00:00Z",
        "2026-10-29T06:00:00.123+00:00",
        "2026-13-29T06:00:00.123Z",
        "2026-00-29T06:00:00.123Z",
        "2026-10-00T06:00:00.123Z",
        "2026-04-31T06:00:00.123Z",
        "1900-02-29T06:00:00.123Z",
        "2026-10-29T24:00:00.123Z",
        "2026-10-29T06:60:00.123Z",
        "2026-10-29T06:00:60.123Z",
        "2026-10-29T06:00:00.12XZ",
        "2026-10-29T06:00:00.123\n",
    ] {
        assert!(!timestamp(bad), "accepted {bad}");
    }
}
