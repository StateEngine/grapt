//! A retried Step reuses its message opening: the response must still group
//! with the content that actually supplied it, not with the reused opening.
use super::*;
use grapt_event::session_message::{
    self, AssistantMessageOpenedFact, ContentAppendedFact, ContentBlockKind, ContentBlockOpenedFact,
};
use grapt_types::{
    ArtifactRef, CaptureDisposition, ContentKind, ContentOrdinal, MessageId, NormalizedContentItem,
};

const RETRY_MESSAGE: &str = "msg_retried_response";

fn reference(fixture: &Fixture, text: &str) -> Result<ArtifactRef, String> {
    recent::capture(fixture.artifacts()?, text)
}

/// Write one row exactly as the durable log would hold it, then move the
/// aggregate cursor up to it so the real writer's next row follows. The
/// positions are fabricated so a retry's opening and the content that replaced
/// it sit at different sequences; a pre-visibility retry leaves that shape.
fn raw_event(
    fixture: &Fixture,
    seq: i64,
    kind: &str,
    data: &serde_json::Value,
) -> Result<(), String> {
    let conn = recent::raw_conn(fixture)?;
    conn.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES (?1, 'session', ?2, ?3, 1_788_220_800_001, ?4, ?5, 1)",
        rusqlite::params![
            format!("evt_retry_{seq}"),
            fixture.session.as_str(),
            seq,
            kind,
            data.to_string()
        ],
    )
    .map_err(|error| error.to_string())?;
    conn.execute(
        "UPDATE event_sequence SET seq = max(seq, ?2)
         WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        rusqlite::params![fixture.session.as_str(), seq],
    )
    .map_err(|error| error.to_string())?;
    Ok(())
}

#[test]
fn a_retried_response_groups_with_the_content_that_supplied_it() -> Result<(), String> {
    let fixture = Fixture::new("retried-response-order")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    // The message opens under one attempt at 10 ...
    let message: MessageId = RETRY_MESSAGE
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let opener = PhysicalAttemptId::new("pat_retry_opener").ok_or("opener")?;
    raw_event(
        &fixture,
        10,
        "session.assistant-message.opened.1",
        &session_message::encode_opened(&AssistantMessageOpenedFact::new(
            opener,
            StepNumber::new(1).ok_or("step")?,
            message.clone(),
        )),
    )?;
    // ... a user input follows the reused opening at 11 ...
    let next: MessageId = "msg_after_reused_opening"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "next input")?;
    // ... and only at 30 does a later attempt supply the visible text.
    let supplier = PhysicalAttemptId::new("pat_retry_supplier").ok_or("supplier")?;
    raw_event(
        &fixture,
        30,
        "session.content-block.opened.1",
        &session_message::encode_block_opened(&ContentBlockOpenedFact::new(
            supplier,
            message.clone(),
            0,
            ContentBlockKind::Text,
        )),
    )?;
    let item = NormalizedContentItem::new(
        ContentOrdinal::new(0),
        ContentKind::Text,
        reference(&fixture, "retry text")?,
    );
    raw_event(
        &fixture,
        31,
        "session.content.appended.1",
        &session_message::encode_appended(
            &ContentAppendedFact::new(message, item, CaptureDisposition::Complete)
                .map_err(|error| error.to_string())?,
        ),
    )?;

    let context = fixture
        .continuation(next)?
        .request_context("next input", false, None, true)?;
    let rendered: Vec<_> = context
        .into_input()
        .iter()
        .map(|item| match item {
            CodexInputItem::AssistantResponseStart => "response".to_owned(),
            CodexInputItem::Message(message) => format!("{:?}:{}", message.role, message.content),
            other => format!("other:{other:?}"),
        })
        .collect();
    assert_eq!(
        rendered,
        [
            "User:inspect",
            "User:next input",
            "response",
            "Assistant:retry text",
        ],
        "the retried response was grouped at its reused opening"
    );
    Ok(())
}
