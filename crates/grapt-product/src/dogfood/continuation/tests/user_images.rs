//! The actual durable continuation reconstructs user pixels after restart.
use super::*;
use base64::Engine as _;
use grapt_inbox::{AdmitRequest, PromoteScope};
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn user_pixels_survive_admission_promotion_and_restart_and_missing_pixels_refuse(
) -> Result<(), String> {
    let mut fixture = Fixture::new("user-images")?;
    let mut png = Cursor::new(Vec::new());
    ::image::DynamicImage::new_rgb8(8, 6)
        .write_to(&mut png, ::image::ImageFormat::Png)
        .map_err(|e| e.to_string())?;
    let receipt = super::super::super::tools::image::prepare_input(png.into_inner())?;
    let captured = fixture
        .artifacts()?
        .capture(
            Cursor::new(receipt),
            CaptureBound::new(1024 * 1024, 8192).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|e| e.to_string())?;
    let images = [captured.reference().clone()];
    {
        let mut inbox = lock(fixture.inbox()?);
        let event: EventId = "evt_user_image"
            .parse()
            .map_err(|e: grapt_types::IdError| e.to_string())?;
        inbox
            .admit_with_images(
                AdmitRequest {
                    event_id: &event,
                    session: &fixture.session,
                    input: &fixture.input,
                    text: "describe image",
                    delivery: grapt_types::DeliveryMode::Queue,
                    resume: false,
                    created_ms: 1,
                },
                &images,
            )
            .map_err(|e| e.to_string())?;
        let promoted: EventId = "evt_user_image_promoted"
            .parse()
            .map_err(|e: grapt_types::IdError| e.to_string())?;
        inbox
            .promote_next(
                &fixture.session,
                PromoteScope::InputAtIdle,
                &mut || promoted.clone(),
                2,
            )
            .map_err(|e| e.to_string())?;
    }
    fn pixels(input: &[CodexInputItem]) -> Result<Vec<u8>, String> {
        let image = input
            .iter()
            .find_map(|item| {
                if let CodexInputItem::ImageUser { text, images } = item {
                    assert_eq!(text.as_str(), "describe image");
                    images.items().first()
                } else {
                    None
                }
            })
            .ok_or("actual provider continuation has no user pixels")?;
        assert_eq!(image.dimensions(), (8, 6));
        base64::engine::general_purpose::STANDARD
            .decode(image.encoded())
            .map_err(|e| e.to_string())
    }
    let before = fixture
        .continuation(fixture.input.clone())?
        .request_input("describe image", false)?;
    assert!(!before
        .iter()
        .any(|item| matches!(item, CodexInputItem::AssistantResponseStart)));
    let original = pixels(&before)?;
    let later: MessageId = "msg_image_retention_next".parse().map_err(|_| "later")?;
    let retained = super::super::retained_users::prefix(
        &lock(fixture.inbox()?),
        fixture.artifacts()?,
        &fixture.session,
        &later,
        grapt_types::EventSeq::new(1_000_000),
        true,
    )?;
    assert_eq!(
        retained, before,
        "retention changed original text or pixels"
    );
    fixture.close();
    let reopened = fixture.reopen(fixture.input.clone())?;
    assert_eq!(
        pixels(&reopened.request_input("describe image", false)?)?,
        original
    );
    // Only this private fixture's captured object is removed: an immutable
    // reference with missing pixels must fail rather than become text-only.
    let id = images[0].id().as_str();
    std::fs::remove_file(
        fixture
            .artifact_root
            .join("objects")
            .join(&id[..2])
            .join(id),
    )
    .map_err(|e| e.to_string())?;
    assert!(reopened.request_input("describe image", false).is_err());
    assert!(
        super::super::retained_users::prefix(
            &lock(&reopened.inbox),
            &reopened.artifacts,
            &fixture.session,
            &later,
            grapt_types::EventSeq::new(1_000_000),
            true,
        )
        .is_err(),
        "retention silently downgraded missing pixels to text"
    );
    Ok(())
}
