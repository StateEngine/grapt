use super::*;
use grapt_event::session_message::{ContentAppendedFact, ContentBlockKind, ContentBlockOpenedFact};
use grapt_session::CommittedPosition;
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CaptureDisposition, ContentOrdinal,
    NormalizedContentItem,
};

#[test]
fn equal_length_reused_blocks_keep_distinct_commit_and_public_coordinates(
) -> Result<(), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_block_projection".parse()?;
    let message: MessageId = "msg_block_projection".parse()?;
    let attempt = PhysicalAttemptId::new("pat_block_projection").ok_or("attempt")?;
    let opening = ContentBlockOpenedFact::new(attempt, message.clone(), 7, ContentBlockKind::Text);
    for ordinal in [0_u64, 1] {
        let position = CommittedPosition {
            event: format!("evt_block_open_{ordinal}").parse()?,
            seq: ordinal * 2,
            created_ms: 123,
        };
        let started = serde_json::to_value(started(&position, &opening, &session, ordinal)?)?;
        assert_eq!(started["id"], position.event.as_str());
        assert_eq!(started["durable"]["seq"], position.seq);
        assert_eq!(started["data"]["ordinal"], ordinal);
        let reference = ArtifactRef::new(
            ArtifactId::new(format!("art_block_{ordinal}")).ok_or("artifact")?,
            5,
            ArtifactDigest::new("sha256:block").ok_or("digest")?,
        );
        let captured = ContentAppendedFact::new(
            message.clone(),
            NormalizedContentItem::new(
                ContentOrdinal::new(ordinal + 3),
                ContentKind::Text,
                reference,
            ),
            CaptureDisposition::Complete,
        )?;
        let ended_position = CommittedPosition {
            event: format!("evt_block_content_{ordinal}").parse()?,
            seq: position.seq + 1,
            created_ms: 456,
        };
        let block = LiveBlock {
            block: 7,
            kind: ContentKind::Text,
            opening: opening.clone(),
            position: position.clone(),
            text: "hello".into(),
            public: Some((position.seq, ordinal)),
        };
        let value = serde_json::to_value(ended(&ended_position, &captured, &session, &block)?)?;
        assert_eq!(value["id"], ended_position.event.as_str());
        assert_eq!(value["created"], 456.0);
        assert_eq!(value["durable"]["seq"], ended_position.seq);
        assert_eq!(value["data"]["ordinal"], ordinal);
        let incomplete = LiveBlock {
            text: "shortened public value".into(),
            ..block
        };
        assert!(ended(&ended_position, &captured, &session, &incomplete).is_err());
    }
    Ok(())
}

/// The ended frame for `text`, built either way, and its coordinates.
fn ended_frames(text: &str) -> Result<(String, String), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_block_elision".parse()?;
    let message: MessageId = "msg_block_elision".parse()?;
    let attempt = PhysicalAttemptId::new("pat_block_elision").ok_or("attempt")?;
    let opening = ContentBlockOpenedFact::new(attempt, message.clone(), 7, ContentBlockKind::Text);
    let position = CommittedPosition {
        event: "evt_block_elision_open".parse()?,
        seq: 4,
        created_ms: 123,
    };
    let captured = ContentAppendedFact::new(
        message,
        NormalizedContentItem::new(
            ContentOrdinal::new(3),
            ContentKind::Text,
            ArtifactRef::new(
                ArtifactId::new("art_block_elision").ok_or("artifact")?,
                text.len() as u64,
                ArtifactDigest::new("sha256:block").ok_or("digest")?,
            ),
        ),
        CaptureDisposition::Complete,
    )?;
    let ended_position = CommittedPosition {
        event: "evt_block_elision_content".parse()?,
        seq: 5,
        created_ms: 456,
    };
    let block = LiveBlock {
        block: 7,
        kind: ContentKind::Text,
        opening,
        position: position.clone(),
        text: text.into(),
        public: Some((position.seq, 0)),
    };
    Ok((
        serde_json::to_string(&ended(&ended_position, &captured, &session, &block)?)?,
        serde_json::to_string(&ended_elided(&ended_position, &captured, &session, &block)?)?,
    ))
}

/// Elision ruling (1): an ended frame that fits is exactly the
/// frozen shape it always was, byte for byte; the elision fields never
/// appear on it.
#[test]
fn an_ended_frame_that_fits_is_byte_identical_to_the_frozen_shape(
) -> Result<(), Box<dyn std::error::Error>> {
    let (whole, _) = ended_frames("hello")?;
    assert_eq!(
        whole,
        concat!(
            r#"{"type":"session.text.ended","id":"evt_block_elision_content","created":456.0,"#,
            r#""durable":{"aggregateID":"ses_block_elision","seq":5,"version":1},"#,
            r#""data":{"sessionID":"ses_block_elision","assistantMessageID":"msg_block_elision","#,
            r#""ordinal":0,"text":"hello"}}"#
        )
    );
    Ok(())
}

/// Elision ruling (2): the elided frame carries an empty text,
/// the raw length in bytes and the lowercase hex SHA-256 of the raw text.
#[test]
fn an_elided_ended_frame_carries_the_length_and_digest_of_the_raw_text(
) -> Result<(), Box<dyn std::error::Error>> {
    let (_, elided) = ended_frames("hello")?;
    let value: serde_json::Value = serde_json::from_str(&elided)?;
    assert_eq!(value["type"], "session.text.ended");
    assert_eq!(
        value["data"],
        serde_json::json!({
            "sessionID": "ses_block_elision",
            "assistantMessageID": "msg_block_elision",
            "ordinal": 0,
            "text": "",
            "textBytes": 5,
            // SHA-256("hello"), a published known answer.
            "textSha256": "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824",
        })
    );
    // Multi-byte text: the length counts bytes, not characters.
    let (_, elided) = ended_frames("é\u{1}")?;
    let value: serde_json::Value = serde_json::from_str(&elided)?;
    assert_eq!(value["data"]["textBytes"], 3);
    Ok(())
}
