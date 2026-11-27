//! A wire case that folds a decoded page through the projection.
use crate::wire::*;

#[test]
fn synthetic_prompts_keep_their_origin_and_full_text() -> Result<(), Box<dyn std::error::Error>> {
    let raw = br#"{"data":[{"type":"synthetic","id":"msg_automated","time":{"created":1.0},"text":"PRIVATE_WAKE_PACKET\nsecond line","metadata":{"native_mail_wake":1}}],"cursor":{}}"#;
    let page = decode_messages(raw)?;
    assert_eq!(page.unsupported, 0);
    assert_eq!(
        page.messages,
        vec![DurableMessage::Automated {
            text: "PRIVATE_WAKE_PACKET\nsecond line".into()
        }]
    );
    let session = session_id("ses_automated")?;
    let mut view = crate::projection::SessionView::new(&session);
    view.apply_durable(&page.messages[0], crate::Bounds::PRODUCTION)?;
    assert_eq!(view.user_row_count(), 1);
    assert_eq!(view.slices[0].kind, crate::SliceKind::Automated);
    assert_eq!(view.slices[0].text, "PRIVATE_WAKE_PACKET\nsecond line");
    Ok(())
}
