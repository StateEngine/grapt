use super::tests::{control, key};
use super::*;
use crossterm::event::KeyCode;
use grapt_client::wire::{decode_session_forms, project_id, session_id, FormLocation};
use serde_json::json;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn setup(fields: serde_json::Value, global: bool) -> Result<Board> {
    let rows = decode_session_forms(&serde_json::to_vec(&json!({"data":[{
        "id":"frm_constraints","sessionID":if global {"global"} else {"ses_constraints"},
        "title":"Constraints","fields":fields}]}))?)?;
    let mut board = Board::default();
    board.update(
        Some(session_id("ses_constraints")?),
        Some(FormLocation {
            project: project_id("prj_constraints")?,
            directory: "/work/a".into(),
        }),
        rows,
    );
    Ok(board)
}
fn answers(request: Request) -> Result<BTreeMap<String, FormValue>> {
    match request.decision {
        Some(Decision::Answer(values)) => Ok(values),
        _ => Err("answer".into()),
    }
}
#[test]
fn conditional_visibility_cascades_and_hidden_drafts_return_in_both_scopes() -> Result {
    for global in [false, true] {
        let mut board = setup(
            json!([
                {"key":"enabled","type":"boolean","default":true,"required":true},
                {"key":"branch","type":"string","default":"release","required":true,"when":[{"key":"enabled","op":"eq","value":true}]},
                {"key":"secret","type":"string","default":"kept","required":true,"when":[{"key":"branch","op":"eq","value":"release"}]},
                {"key":"different","type":"string","default":"also kept","when":[{"key":"branch","op":"neq","value":"other"}]}
            ]),
            global,
        )?;
        assert_eq!(board.drafts[0].visible_fields(), [0, 1, 2, 3]);
        board.key(key(KeyCode::Tab));
        board.key(key(KeyCode::Tab));
        board.paste(" edited");
        board.key(key(KeyCode::Tab));
        board.key(key(KeyCode::Tab));
        board.key(key(KeyCode::Char(' ')));
        assert_eq!(board.drafts[0].visible_fields(), [0]);
        board.key(key(KeyCode::Tab));
        assert_eq!(board.drafts[0].field, 0);
        let (_, projected) = conditions::project(&board.drafts[0].row, &board.drafts[0].values)?;
        assert_eq!(
            projected,
            BTreeMap::from([("enabled".into(), FormValue::Flag(false))])
        );
        assert!(validate(&board.drafts[0].row, &board.drafts[0].values).is_err());
        board.key(key(KeyCode::Char(' ')));
        board.key(key(KeyCode::Tab));
        board.key(key(KeyCode::Tab));
        assert_eq!(board.drafts[0].editor.lines(), ["kept edited"]);
        board.key(key(KeyCode::BackTab));
        board.key(key(KeyCode::BackTab));
        board.key(key(KeyCode::Char(' ')));
        let request = board.key(control('s')).ok_or("explicit inactive answer")?;
        assert_eq!(answers(request.clone())?, projected);
        board.key(key(KeyCode::Char(' ')));
        board.key(control('e'));
        board.paste("uncertain edit");
        assert_eq!(board.drafts[0].decision, request.decision);
        assert_eq!(board.drafts[0].visible_fields(), [0]);
    }
    Ok(())
}
#[test]
fn conditions_compare_typed_numbers_and_multiselect_membership_but_absence_never_matches_neq(
) -> Result {
    let mut board = setup(
        json!([
            {"key":"number","type":"integer","default":0},
            {"key":"many","type":"multiselect","default":["a"],"options":[{"value":"a","label":"A"}]},
            {"key":"child","type":"string","when":[{"key":"number","op":"eq","value":0},{"key":"many","op":"eq","value":"a"}]},
            {"key":"missing","type":"string","when":[{"key":"child","op":"neq","value":"x"}]}
        ]),
        false,
    )?;
    assert_eq!(board.drafts[0].visible_fields(), [0, 1, 2]);
    board.key(key(KeyCode::Backspace));
    assert_eq!(board.drafts[0].visible_fields(), [0, 1]);
    Ok(())
}
#[test]
fn custom_single_and_multi_values_require_explicit_choice_and_remain_removable() -> Result {
    for global in [false, true] {
        let mut board = setup(
            json!([
                {"key":"one","type":"string","custom":true,"default":"seed","options":[{"value":"a","label":"A"}]},
                {"key":"many","type":"multiselect","custom":true,"minItems":2,"options":[]}
            ]),
            global,
        )?;
        board.key(control('e'));
        assert_eq!(board.drafts[0].editor.lines(), ["seed"]);
        board.paste(" edited");
        assert_eq!(
            board.drafts[0].values.get("one"),
            Some(&FormValue::Text("seed".into()))
        );
        assert!(board.key(control('s')).is_none());
        board.key(key(KeyCode::Tab));
        board.key(key(KeyCode::BackTab));
        board.key(control('e'));
        assert_eq!(board.drafts[0].editor.lines(), ["seed edited"]);
        board.key(key(KeyCode::Enter));
        assert_eq!(
            board.drafts[0].values.get("one"),
            Some(&FormValue::Text("seed edited".into()))
        );
        board.key(key(KeyCode::Tab));
        for text in ["custom-one", "custom-two"] {
            board.key(control('e'));
            board.drafts[0].editor = ratatui_textarea::TextArea::default();
            board.paste(text);
            board.key(key(KeyCode::Enter));
        }
        assert_eq!(
            board.drafts[0].values.get("many"),
            Some(&FormValue::Texts(vec![
                "custom-one".into(),
                "custom-two".into()
            ]))
        );
        board.key(key(KeyCode::Home));
        board.key(key(KeyCode::Char(' ')));
        assert_eq!(
            board.drafts[0].values.get("many"),
            Some(&FormValue::Texts(vec!["custom-two".into()]))
        );
        assert!(board.key(control('s')).is_none());
        board.key(control('r'));
        assert_eq!(board.drafts[0].decision, Some(Decision::Cancel));
    }
    Ok(())
}
#[test]
fn pattern_validation_stays_with_product_and_unknown_or_external_fields_remain_unresolved() -> Result
{
    let mut board = setup(
        json!([{"key":"code","type":"string","pattern":"^(a)\\1$","default":"bad"}]),
        false,
    )?;
    assert!(board.key(control('s')).is_some());
    let mut board = setup(
        json!([{"key":"code","type":"string","placeholder":"unknown"}]),
        false,
    )?;
    assert!(board.key(control('s')).is_none());
    assert_eq!(
        board.key(control('r')).ok_or("decline")?.decision,
        Some(Decision::Cancel)
    );
    let mut board = setup(
        json!([{"key":"ack","type":"external","url":"https://example.test"}]),
        false,
    )?;
    assert!(board.key(control('s')).is_none());
    assert_eq!(
        board.key(control('r')).ok_or("decline external")?.decision,
        Some(Decision::Cancel)
    );
    Ok(())
}

#[test]
fn committing_offered_or_existing_custom_text_focuses_that_exact_choice() -> Result {
    for kind in ["string", "multiselect"] {
        let mut board = setup(
            json!([{"key":"choice","type":kind,"custom":true,
            "options":[{"value":"a","label":"A"},{"value":"b","label":"B"}]}]),
            false,
        )?;
        for text in ["a", "custom-one", "custom-two", "custom-one"] {
            board.key(control('e'));
            board.drafts[0].editor = ratatui_textarea::TextArea::default();
            board.paste(text);
            board.key(key(KeyCode::Enter));
            let draft = &board.drafts[0];
            let options = draft.choice_values(&draft.row.fields[0]);
            let cursor = draft
                .choices
                .get("choice")
                .copied()
                .ok_or("choice cursor")?;
            assert_eq!(options.get(cursor).map(String::as_str), Some(text));
            if text == "a" {
                board.key(key(KeyCode::Char(' ')));
                assert_eq!(
                    board.drafts[0].values.get("choice"),
                    Some(&if kind == "string" {
                        FormValue::Text("a".into())
                    } else {
                        FormValue::Texts(vec![])
                    })
                );
            }
        }
        board.key(key(KeyCode::Char(' ')));
        assert_eq!(
            board.drafts[0].values.get("choice"),
            Some(&if kind == "string" {
                FormValue::Text("custom-one".into())
            } else {
                FormValue::Texts(vec!["custom-two".into()])
            })
        );
    }
    Ok(())
}
