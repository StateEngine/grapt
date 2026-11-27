use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::wire::{form_id, session_id, FormField, FormOwner};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
pub(super) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
pub(super) fn control(ch: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL)
}
pub(super) fn row(id: &str) -> Result<FormRow> {
    Ok(FormRow {
        metadata: Default::default(),
        id: form_id(id)?,
        owner: FormOwner::Session(session_id("ses_forms")?),
        title: "Choose branch".into(),
        fields: vec![
            FormField {
                attributes: Default::default(),
                key: "branch".into(),
                kind: "string".into(),
                title: None,
                required: true,
            },
            FormField {
                attributes: Default::default(),
                key: "dry_run".into(),
                kind: "boolean".into(),
                title: None,
                required: true,
            },
        ],
    })
}
fn board() -> Result<Board> {
    let mut board = Board::default();
    board.update(Some(session_id("ses_forms")?), None, vec![row("frm_one")?]);
    Ok(board)
}
#[test]
fn required_unsupported_and_bounds_are_honest_and_cancel_is_explicit() -> Result {
    let mut board = board()?;
    assert!(board.key(control('s')).is_none());
    assert!(board.drafts[0].note.contains("Still needed"));
    board.paste(&"x".repeat(MAX_FIELD + 1));
    assert!(board.drafts[0].values.is_empty());
    board.drafts[0].row.fields[0].kind = "external".into();
    board.paste("42");
    assert!(board.drafts[0].values.is_empty());
    assert!(board.key(control('s')).is_none());
    board.key(key(KeyCode::Esc));
    assert!(!board.focused);
    assert!(board.drafts[0].decision.is_none());
    board.focused = true;
    assert_eq!(
        board.key(control('r')).ok_or("cancel")?.decision,
        Some(Decision::Cancel)
    );
    Ok(())
}
#[test]
fn pending_writes_freeze_the_original_answer_and_checks_never_replay_it() -> Result {
    let mut board = board()?;
    board.paste("original");
    board.key(key(KeyCode::Tab));
    board.key(key(KeyCode::Char(' ')));
    let request = board.key(control('s')).ok_or("answer")?;
    board.reply(Reply {
        validation_refused: false,
        target: request.target.clone(),
        form: request.form.id.clone(),
        state: None,
        note: "unconfirmed".into(),
    });
    board.paste("changed");
    assert!(board.key(control('r')).is_none());
    assert!(board.key(control('o')).ok_or("check")?.decision.is_none());
    board.reply(Reply {
        validation_refused: false,
        target: request.target.clone(),
        form: request.form.id.clone(),
        state: Some(FormState::Pending),
        note: "still open".into(),
    });
    assert_eq!(
        board.key(control('s')).ok_or("explicit retry")?.decision,
        request.decision
    );
    Ok(())
}
#[test]
fn refresh_and_navigation_retain_only_session_and_schema_bound_drafts() -> Result {
    let mut board = board()?;
    board.paste("first");
    board.update(
        Some(session_id("ses_forms")?),
        None,
        vec![row("frm_one")?, row("frm_two")?],
    );
    board.key(control('n'));
    board.paste("second");
    board.key(control('p'));
    assert_eq!(board.drafts[board.selected].editor.lines(), ["first"]);
    board.update(Some(session_id("ses_forms")?), None, vec![]);
    assert!(!board.drafts[0].listed);
    assert!(board.key(control('s')).is_none());
    board.update(Some(session_id("ses_other")?), None, vec![]);
    assert!(board.drafts.is_empty());
    assert!(!board.focused);
    assert!(!board.reply(Reply {
        validation_refused: false,
        target: Target::Session(session_id("ses_forms")?),
        form: form_id("frm_one")?,
        state: Some(FormState::Cancelled),
        note: "old response".into()
    }));
    Ok(())
}

#[test]
fn constrained_fields_and_unattributed_owners_stay_unresolved() -> Result {
    let mut board = board()?;
    board.drafts[0].row.fields[0].attributes.insert(
        "when".into(),
        serde_json::json!([{"key":"earlier","op":"eq","value":"main"}]),
    );
    board.paste("unoffered branch");
    assert!(board.key(control('s')).is_none());
    assert!(board.drafts[0].note.contains("earlier field"));
    board.drafts[0].row.owner = FormOwner::Unattributed("global".into());
    assert!(board.key(control('r')).is_none());
    assert!(board.key(control('o')).is_none());
    assert!(board.drafts[0].note.contains("Unattributed"));
    Ok(())
}

#[test]
fn confirmed_settlement_remains_visible_and_failed_reads_do_not_erase_it() -> Result {
    let mut board = board()?;
    let session = session_id("ses_forms")?;
    let form = form_id("frm_one")?;
    board.reply(Reply {
        validation_refused: false,
        target: Target::Session(session.clone()),
        form: form.clone(),
        state: Some(FormState::Cancelled),
        note: "Cancellation confirmed".into(),
    });
    board.update(Some(session.clone()), None, vec![]);
    assert!(board.focused);
    assert_eq!(board.drafts.len(), 1);
    board.reply(Reply {
        validation_refused: false,
        target: Target::Session(session),
        form,
        state: None,
        note: "State unreadable".into(),
    });
    assert_eq!(board.drafts[0].state, Some(FormState::Cancelled));
    assert!(board.key(control('s')).is_none());
    Ok(())
}

#[test]
fn an_offscreen_unconfirmed_question_survives_disappearance_and_late_replies() -> Result {
    let mut board = board()?;
    board.update(
        Some(session_id("ses_forms")?),
        None,
        vec![row("frm_one")?, row("frm_two")?],
    );
    let original = board.key(control('r')).ok_or("decline")?;
    board.key(control('n'));
    board.paste("second draft");
    board.update(Some(session_id("ses_forms")?), None, vec![row("frm_two")?]);
    assert_eq!(board.drafts.len(), 2);
    assert!(board.reply(Reply {
        validation_refused: false,
        target: original.target,
        form: original.form.id,
        state: None,
        note: "unconfirmed".into()
    }));
    board.key(control('p'));
    assert_eq!(board.drafts[board.selected].row.id, form_id("frm_one")?);
    assert_eq!(
        board.drafts[board.selected].decision,
        Some(Decision::Cancel)
    );
    assert!(board
        .key(control('o'))
        .ok_or("state check")?
        .decision
        .is_none());
    Ok(())
}

#[test]
fn newly_listed_questions_do_not_evict_existing_drafts_at_the_cache_bound() -> Result {
    let mut board = Board::default();
    let rows: Vec<_> = (0..MAX_FORMS)
        .map(|index| row(&format!("frm_bound{index}")))
        .collect::<Result<_>>()?;
    board.update(Some(session_id("ses_forms")?), None, rows.clone());
    board.key(control('r')).ok_or("first decline")?;
    board.key(control('p'));
    board.paste("last draft must stay");
    let mut current = rows[1..].to_vec();
    current.push(row("frm_new")?);
    board.update(Some(session_id("ses_forms")?), None, current);
    assert_eq!(board.drafts.len(), MAX_FORMS);
    assert_eq!(board.omitted, 1);
    assert_eq!(
        board.drafts[board.selected].editor.lines(),
        ["last draft must stay"]
    );
    assert_eq!(board.drafts[0].decision, Some(Decision::Cancel));
    Ok(())
}

#[test]
fn dismissed_unconfirmed_drafts_stay_hidden_across_unchanged_polls() -> Result {
    let mut board = board()?;
    board.key(control('r')).ok_or("decline")?;
    board.update(Some(session_id("ses_forms")?), None, vec![]);
    board.key(key(KeyCode::Esc));
    for _ in 0..3 {
        board.update(Some(session_id("ses_forms")?), None, vec![]);
        assert!(!board.focused);
        assert_eq!(board.drafts[0].decision, Some(Decision::Cancel));
    }
    // The same question reappearing is still the retained draft, not discovery.
    board.update(Some(session_id("ses_forms")?), None, vec![row("frm_one")?]);
    assert!(!board.focused);
    board.update(
        Some(session_id("ses_forms")?),
        None,
        vec![row("frm_one")?, row("frm_new")?],
    );
    assert!(board.focused);
    assert_eq!(board.drafts[board.selected].row.id, form_id("frm_new")?);
    Ok(())
}
