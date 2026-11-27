use super::tests::{control, key};
use super::*;
use crossterm::event::KeyCode;
use grapt_client::wire::{decode_session_forms, project_id, session_id, FormLocation};
use serde_json::json;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn setup(fields: serde_json::Value, global: bool) -> Result<Board> {
    let rows = decode_session_forms(&serde_json::to_vec(
        &json!({"data":[{"id":"frm_defaults","sessionID":if global {"global"}else{"ses_defaults"},"title":"Defaults","fields":fields}]}),
    )?)?;
    let mut board = Board::default();
    board.update(
        Some(session_id("ses_defaults")?),
        Some(FormLocation {
            project: project_id("prj_defaults")?,
            directory: "/work/a".into(),
        }),
        rows,
    );
    Ok(board)
}
#[test]
fn typed_defaults_seed_once_without_submitting_and_remain_editable_in_both_scopes() -> Result {
    for global in [false, true] {
        let mut board = setup(
            json!([
                {"key":"text","type":"string","default":"seed","required":true},
                {"key":"number","type":"number","default":1.25,"required":true},
                {"key":"flag","type":"boolean","default":false,"required":true},
                {"key":"choice","type":"string","default":"b","options":[{"value":"a","label":"A"},{"value":"b","label":"B"}]},
                {"key":"many","type":"multiselect","default":["b"],"options":[{"value":"a","label":"A"},{"value":"b","label":"B"}]},
                {"key":"zero","type":"integer","default":0}
            ]),
            global,
        )?;
        assert!(board.drafts[0].decision.is_none() && !board.drafts[0].pending);
        assert_eq!(board.drafts[0].editor.lines(), ["seed"]);
        board.paste(" edited");
        board.key(key(KeyCode::Tab));
        assert_eq!(board.drafts[0].editor.lines(), ["1.25"]);
        board.key(key(KeyCode::Tab));
        board.key(key(KeyCode::Char(' ')));
        let request = board.key(control('s')).ok_or("explicit answer")?;
        let Some(Decision::Answer(values)) = request.decision else {
            return Err("answer".into());
        };
        let actual: BTreeMap<_, _> = values
            .into_iter()
            .map(|(key, value)| (key, value.to_json()))
            .collect();
        assert_eq!(
            serde_json::to_value(actual)?,
            json!({"text":"seed edited","number":1.25,"flag":true,"choice":"b","many":["b"],"zero":0})
        );
    }
    Ok(())
}
#[test]
fn refresh_and_changed_defaults_never_replace_edits_or_reapply_cleared_choices() -> Result {
    let mut board = setup(
        json!([
            {"key":"text","type":"string","default":"seed"},
            {"key":"choice","type":"string","default":"b","options":[{"value":"a","label":"A"},{"value":"b","label":"B"}]}
        ]),
        false,
    )?;
    board.paste(" edited");
    board.key(key(KeyCode::Tab));
    board.key(key(KeyCode::Delete));
    let mut row = board.drafts[0].row.clone();
    row.fields[0]
        .attributes
        .insert("default".into(), json!("new suggestion"));
    for _ in 0..3 {
        board.update(
            board.session.clone(),
            board.location.clone(),
            vec![row.clone()],
        );
    }
    assert_eq!(
        board.drafts[0].values.get("text"),
        Some(&FormValue::Text("seed edited".into()))
    );
    assert!(!board.drafts[0].values.contains_key("choice"));
    board.key(key(KeyCode::BackTab));
    assert_eq!(board.drafts[0].editor.lines(), ["seed edited"]);
    Ok(())
}
#[test]
fn optional_empty_defaults_are_typed_until_the_user_explicitly_clears_them() -> Result {
    let mut board = setup(
        json!([
            {"key":"text","type":"string","default":""},
            {"key":"flag","type":"boolean","default":false},
            {"key":"many","type":"multiselect","default":[],"options":[{"value":"a","label":"A"}]}
        ]),
        false,
    )?;
    let request = board.key(control('s')).ok_or("empty default answer")?;
    let Some(Decision::Answer(values)) = request.decision else {
        return Err("answer".into());
    };
    assert_eq!(values.get("text"), Some(&FormValue::Text(String::new())));
    assert_eq!(values.get("flag"), Some(&FormValue::Flag(false)));
    assert_eq!(values.get("many"), Some(&FormValue::Texts(vec![])));
    let mut cleared = setup(json!([{"key":"text","type":"string","default":""}]), false)?;
    cleared.key(key(KeyCode::Delete));
    let request = cleared
        .key(control('s'))
        .ok_or("explicitly cleared answer")?;
    assert_eq!(request.decision, Some(Decision::Answer(BTreeMap::new())));
    Ok(())
}
#[test]
fn known_formats_defer_to_product_and_only_a_validation_receipt_unfreezes_a_refused_answer(
) -> Result {
    for format in ["email", "uri", "date", "date-time"] {
        let mut board = setup(
            json!([{"key":"value","type":"string","format":format,"default":"bad format"}]),
            false,
        )?;
        let request = board
            .key(control('s'))
            .ok_or("backend must validate format")?;
        board.reply(Reply {
            target: request.target.clone(),
            form: request.form.id.clone(),
            state: Some(FormState::Pending),
            note: "lost reply".into(),
            validation_refused: false,
        });
        board.paste("no edit");
        assert_eq!(board.drafts[0].editor.lines(), ["bad format"]);
        board.reply(Reply {
            target: request.target,
            form: request.form.id,
            state: Some(FormState::Pending),
            note: "Answer refused".into(),
            validation_refused: true,
        });
        board.paste(" corrected");
        assert_eq!(board.drafts[0].editor.lines(), ["bad format corrected"]);
        assert!(board.drafts[0].decision.is_none());
    }
    let mut unknown = setup(json!([{"key":"value","type":"string"}]), false)?;
    unknown.drafts[0].row.fields[0]
        .attributes
        .insert("format".into(), json!("password"));
    unknown.paste("secret");
    assert!(unknown.key(control('s')).is_none());
    assert_eq!(
        unknown.key(control('r')).ok_or("decline")?.decision,
        Some(Decision::Cancel)
    );
    Ok(())
}

#[test]
fn default_bounds_and_unknown_semantics_never_turn_into_silent_answers() -> Result {
    // Each field fits the wire's 8 KiB scalar budget, while their combined
    // answer exceeds the editor's 64 KiB budget.
    let fields: Vec<_> = (0..16).map(|index| json!({"key":format!("field{index}"),"type":"string","default":"x".repeat(5_000),"required":true})).collect();
    let mut board = setup(json!(fields), false)?;
    assert!(board.drafts[0].values.is_empty());
    assert!(board.drafts[0].note.contains("64 KiB"));
    assert!(board.key(control('s')).is_none());
    let mut board = setup(
        json!([{"key":"number","type":"number","default":"NaN"}]),
        false,
    )?;
    assert!(defaults::value(&board.drafts[0].row.fields[0]).is_err());
    assert!(board.key(control('s')).is_none());
    assert_eq!(
        board.key(control('r')).ok_or("decline")?.decision,
        Some(Decision::Cancel)
    );
    Ok(())
}

#[test]
fn default_only_refresh_keeps_an_uncertain_original_decision_until_explicit_retry() -> Result {
    let mut board = setup(
        json!([{"key":"text","type":"string","default":"original"}]),
        true,
    )?;
    let request = board.key(control('s')).ok_or("answer")?;
    let mut updated = request.form.clone();
    updated.fields[0]
        .attributes
        .insert("default".into(), json!("replacement"));
    board.update(board.session.clone(), board.location.clone(), vec![updated]);
    assert_eq!(board.drafts[0].decision, request.decision);
    assert!(board.key(control('s')).is_none());
    board.reply(Reply {
        target: request.target,
        form: request.form.id,
        state: Some(FormState::Pending),
        note: "read-only reconciliation".into(),
        validation_refused: false,
    });
    assert_eq!(
        board
            .key(control('s'))
            .ok_or("explicit original retry")?
            .decision,
        request.decision
    );
    Ok(())
}

#[test]
fn canonical_repeated_default_selections_are_preserved_until_explicit_editing() -> Result {
    let fields = json!([{"key":"many","type":"multiselect","default":["a","a"],"options":[{"value":"a","label":"A"}]}]);
    let mut board = setup(fields.clone(), false)?;
    let request = board.key(control('s')).ok_or("repeated canonical values")?;
    assert_eq!(
        request.decision,
        Some(Decision::Answer(BTreeMap::from([(
            "many".into(),
            FormValue::Texts(vec!["a".into(), "a".into()])
        )])))
    );
    let mut board = setup(fields, false)?;
    board.key(key(KeyCode::Char(' ')));
    assert_eq!(
        board.drafts[0].values.get("many"),
        Some(&FormValue::Texts(vec![]))
    );
    board.key(key(KeyCode::Char(' ')));
    assert_eq!(
        board.drafts[0].values.get("many"),
        Some(&FormValue::Texts(vec!["a".into()]))
    );
    Ok(())
}
