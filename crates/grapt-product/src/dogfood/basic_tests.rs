use super::*;
use grapt_inbox::WakeDisposition;

use super::support::bounded_message;

#[test]
fn empty_prompt_collections_remain_empty_and_content_is_never_discarded() {
    assert_eq!(
        support::compose_prompt_extensions(&json!({"files":[], "agents":[]}), "hello".to_owned()),
        Ok("hello".to_owned())
    );
    assert_eq!(
        support::compose_prompt_extensions(
            &json!({"files":[{"uri":"data:image/png;base64,AA==","name":"image.png"}]}),
            "look".to_owned(),
        ),
        Ok("look\n\n[Image attached: image.png]".to_owned())
    );
    assert_eq!(
        support::compose_prompt_extensions(
            &json!({"files":[{"uri":"file:///work/notes.txt","name":"notes.txt"}]}),
            "read".to_owned(),
        ),
        Ok("read\n\n[ACP attachment: notes.txt]\nfile:///work/notes.txt".to_owned())
    );
    for value in [
        json!({"files":[{"type":"file"}]}),
        json!({"agents":["agent"]}),
        json!({"files":null}),
        json!({"metadata":{}}),
    ] {
        assert!(support::compose_prompt_extensions(&value, "text".to_owned()).is_err());
    }
}

#[test]
fn error_messages_are_bounded_on_utf8_boundaries() {
    let message = "λ".repeat(1_000);
    let bounded = bounded_message(message);
    assert!(bounded.len() <= 1_024);
    assert!(bounded.is_char_boundary(bounded.len()));
}

#[test]
fn stable_retry_rechecks_durable_pending_work_through_the_coordinator() {
    assert!(should_execute(&AdmitOutcome::Reconciled {
        wake: WakeDisposition::WakeAdvised,
    }));
}

#[tokio::test]
async fn retained_location_refuses_invalid_or_unresolved_workspace_without_fallback() {
    let query = |value| {
        decode::<ProjectCurrentLocationQueryDto>(value)
            .unwrap_or_else(|error| panic!("location query evidence did not decode: {error}"))
    };
    let catalog = CatalogProjection {
        accounts: Default::default(),
        protocols: Default::default(),
        inventory: None,
        project: ProjectId::new("project-a")
            .unwrap_or_else(|| panic!("fixture project id is invalid")),
        directory: "/work/project".to_owned(),
        provider: "openai".to_owned(),
        model: "fixture".to_owned(),
        default_effort: None,
        credential_record: None,
    };
    for value in [
        json!({"location": {"directory": "relative-location"}}),
        json!({"location": {"workspace": "wrk_other"}}),
        json!({"location": {"directory": "/work/project", "workspace": "wrk_other"}}),
    ] {
        assert!(
            locations::resolve(&catalog, &query(value)).await.is_err(),
            "invalid location silently selected the default"
        );
    }
}

#[test]
fn seat_startup_metadata_is_a_closed_presentation_label_not_arbitrary_metadata() {
    assert_eq!(
        support::compose_prompt_extensions(
            &json!({"metadata":{"origin":"seat-startup"}}),
            "packet".into()
        ),
        Ok("packet".into())
    );
    for metadata in [
        json!({"origin":"human"}),
        json!({"origin":"seat-startup","bypass":true}),
        json!({"origin":true}),
    ] {
        assert!(
            support::compose_prompt_extensions(&json!({"metadata":metadata}), "packet".into())
                .is_err()
        );
    }
}
