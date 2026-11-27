use super::tests::{control, row};
use super::*;
use grapt_client::wire::{form_id, project_id, session_id, FormLocation, FormOwner};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;
#[test]
fn location_drafts_survive_session_changes_but_reject_old_location_replies() -> Result {
    let location = FormLocation {
        project: project_id("prj_location")?,
        directory: "/work/a".into(),
    };
    let session = session_id("ses_forms")?;
    let mut global = row("frm_global")?;
    global.owner = FormOwner::Global;
    let mut board = Board::default();
    board.update(
        Some(session.clone()),
        Some(location.clone()),
        vec![row("frm_session")?],
    );
    board.paste("session draft");
    board.update(
        Some(session.clone()),
        Some(location.clone()),
        vec![row("frm_session")?, global.clone()],
    );
    assert_eq!(board.drafts[0].editor.lines(), ["session draft"]);
    board.key(control('n'));
    board.paste("location draft");
    board.update(
        Some(session_id("ses_other")?),
        Some(location.clone()),
        vec![global.clone()],
    );
    assert_eq!(board.drafts.len(), 1);
    assert_eq!(board.drafts[0].editor.lines(), ["location draft"]);
    let request = board.key(control('r')).ok_or("decline")?;
    assert_eq!(request.target, Target::Location(location.clone()));
    let next = FormLocation {
        directory: "/work/b".into(),
        ..location
    };
    board.update(Some(session), Some(next), vec![global]);
    assert!(!board.reply(Reply {
        validation_refused: false,
        target: request.target,
        form: form_id("frm_global")?,
        state: Some(FormState::Cancelled),
        note: "old location".into()
    }));
    assert!(board.drafts[0].state.is_none());
    assert!(board.drafts[0].values.is_empty());
    Ok(())
}
#[test]
fn only_an_explicit_global_owner_with_a_resolved_location_is_addressable() -> Result {
    let mut global = row("frm_global")?;
    global.owner = FormOwner::Global;
    let mut board = Board::default();
    board.update(None, None, vec![global.clone()]);
    assert!(board.key(control('r')).is_none());
    let location = FormLocation {
        project: project_id("prj_location")?,
        directory: "/work/a".into(),
    };
    global.owner = FormOwner::Unattributed("future-owner".into());
    board.update(None, Some(location), vec![global]);
    assert!(board.key(control('r')).is_none());
    assert!(board.key(control('o')).is_none());
    Ok(())
}

#[test]
fn url_consent_requires_a_choice_and_keeps_cancel_distinct_without_a_session() -> Result {
    use crossterm::event::KeyCode;
    let form = grapt_client::wire::decode_form(br#"{"data":{"id":"frm_url","sessionID":"global","title":"External interaction","metadata":{"kind":"mcp-url-elicitation","server":"ask","message":"Connect","host":"example.test","url":"https://example.test/connect?secret=human-only","elicitationId":"external-9"},"fields":[{"key":"decision","type":"string","required":true,"custom":false,"options":[{"value":"accept","label":"Continue manually"},{"value":"decline","label":"Decline"}]}]}}"#)?;
    let location = FormLocation {
        project: project_id("prj_location")?,
        directory: "/work/a".into(),
    };
    for action in ["accept", "decline", "cancel"] {
        let mut board = Board::default();
        board.update(None, Some(location.clone()), vec![form.clone()]);
        // Neither displaying a URL, pasting, nor submitting an unchosen required
        // field sends a decision or writes anything into the prompt composer.
        board.paste("must not become an answer");
        assert!(board.key(control('s')).is_none());
        assert!(board.key(super::tests::key(KeyCode::Esc)).is_none());
        board.focused = true;
        let request = if action == "cancel" {
            board.key(control('r')).ok_or("cancel")?
        } else {
            if action == "decline" {
                board.key(super::tests::key(KeyCode::Down));
            }
            board.key(super::tests::key(KeyCode::Char(' ')));
            board.key(control('s')).ok_or("consent")?
        };
        assert_eq!(request.target, Target::Location(location.clone()));
        assert_eq!(
            request.decision,
            Some(if action == "cancel" {
                Decision::Cancel
            } else {
                Decision::Answer(BTreeMap::from([(
                    "decision".into(),
                    FormValue::Text(action.into()),
                )]))
            })
        );
    }
    Ok(())
}
