//! A repeated packet ACK needs the producer's durable, Session-bound receipt.
use super::{Verified, E};
use grapt_types::SessionId;
use serde::Deserialize;
use serde_json::Value;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Acknowledgement {
    state: String,
    packet_id: String,
    #[serde(rename = "recorded")]
    _recorded: bool,
    packet_sha256: String,
    project_id: String,
    agent_id: String,
    session_id: String,
    acknowledgement_id: String,
    acknowledged_ts: String,
}

pub(super) fn verify(
    packet: &Verified,
    reply: &Value,
    project: &str,
    agent: &str,
    session: &SessionId,
) -> Result<(), E> {
    let packet_id = packet.id().ok_or(E::PacketAcknowledgement)?;
    let value = reply
        .get("packet_acknowledgement")
        .ok_or(E::PacketAcknowledgement)?;
    if value.get("state").and_then(Value::as_str) != Some("acknowledged")
        || value.get("packet_id").and_then(Value::as_str) != Some(packet_id)
    {
        return Err(E::PacketAcknowledgement);
    }
    // Older producers prove a new append without returning receipt coordinates.
    // They cannot prove a duplicate; partial modern receipts must not use this path.
    if value.get("recorded") == Some(&Value::Bool(true))
        && value.as_object().is_some_and(|object| object.len() == 3)
    {
        return Ok(());
    }
    let ack: Acknowledgement =
        serde_json::from_value(value.clone()).map_err(|_| E::PacketAcknowledgement)?;
    let acknowledged_session: SessionId = ack
        .session_id
        .parse()
        .map_err(|_| E::PacketAcknowledgement)?;
    if ack.state != "acknowledged"
        || ack.packet_id != packet_id
        || !digest(&ack.packet_id)
        || ack.packet_sha256 != packet.pointer.sha256
        || ack.project_id != project
        || ack.agent_id != agent
        || &acknowledged_session != session
        || !digest(&ack.acknowledgement_id)
        || !timestamp(&ack.acknowledged_ts)
    {
        return Err(E::PacketAcknowledgement);
    }
    Ok(())
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn timestamp(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 24
        || [4, 7, 10, 13, 16, 19, 23]
            .into_iter()
            .zip(b"--T::.Z")
            .any(|(index, separator)| bytes[index] != *separator)
        || bytes.iter().enumerate().any(|(index, byte)| {
            ![4, 7, 10, 13, 16, 19, 23].contains(&index) && !byte.is_ascii_digit()
        })
    {
        return false;
    }
    let number = |start: usize, end: usize| {
        bytes[start..end]
            .iter()
            .fold(0_u32, |value, digit| value * 10 + u32::from(digit - b'0'))
    };
    let year = number(0, 4);
    let days = match number(5, 7) {
        2 if year.is_multiple_of(400) || (year.is_multiple_of(4) && !year.is_multiple_of(100)) => {
            29
        }
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        _ => return false,
    };
    (1..=days).contains(&number(8, 10))
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60
}

#[cfg(test)]
mod tests;
