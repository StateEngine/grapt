use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use grapt_client::wire::{form_id, session_id, FormField, FormOwner};
use serde_json::json;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn field(
    key: &str,
    kind: &str,
    required: bool,
    attributes: serde_json::Value,
) -> Result<FormField> {
    Ok(FormField {
        key: key.into(),
        kind: kind.into(),
        required,
        title: None,
        attributes: attributes
            .as_object()
            .ok_or("attributes object")?
            .iter()
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect(),
    })
}
fn board(fields: Vec<FormField>) -> Result<Board> {
    let session = session_id("ses_inputs")?;
    let mut board = Board::default();
    board.update(
        Some(session.clone()),
        None,
        vec![FormRow {
            metadata: Default::default(),
            id: form_id("frm_inputs")?,
            owner: FormOwner::Session(session),
            title: "Typed question".into(),
            fields,
        }],
    );
    Ok(board)
}
fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}
fn submit() -> KeyEvent {
    KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)
}
fn replace(board: &mut Board, text: &str) {
    board.drafts[0].editor = ratatui_textarea::TextArea::default();
    board.paste(text);
}

#[test]
fn numeric_drafts_preserve_invalid_optional_input_and_enforce_types_and_bounds() -> Result {
    let mut board = board(vec![
        field("count", "integer", true, json!({"minimum":1,"maximum":10}))?,
        field("ratio", "number", false, json!({"minimum":-2,"maximum":0}))?,
    ])?;
    board.paste("1.5");
    assert!(board.key(submit()).is_none());
    assert!(board.drafts[0].note.contains("integer"));
    replace(&mut board, "11");
    assert!(board.key(submit()).is_none());
    assert!(board.drafts[0].note.contains("maximum"));
    replace(&mut board, "3");
    board.key(key(KeyCode::Tab));
    board.paste("not a number");
    board.key(key(KeyCode::Tab));
    assert_eq!(board.drafts[0].editor.lines(), ["3"]);
    assert!(board.key(submit()).is_none());
    assert!(board.drafts[0].note.contains("ratio"));
    board.key(key(KeyCode::Tab));
    assert_eq!(board.drafts[0].editor.lines(), ["not a number"]);
    replace(&mut board, "-1.25");
    let request = board.key(submit()).ok_or("typed answer")?;
    let Some(Decision::Answer(answer)) = request.decision else {
        return Err("expected answer".into());
    };
    let json: serde_json::Value =
        serde_json::from_slice(&grapt_client::wire::encode_form_reply(&answer)?)?;
    assert_eq!(json, json!({"answer":{"count":3,"ratio":-1.25}}));
    assert!(board.key(key(KeyCode::Down)).is_none());
    assert_eq!(board.drafts[0].decision, Some(Decision::Answer(answer)));
    Ok(())
}

#[test]
fn choice_navigation_requires_explicit_selection_and_retains_each_fields_cursor() -> Result {
    let opts = json!({"options":[{"value":"stable","label":"Stable release"},{"value":"canary","label":"Preview release"}],"custom":false});
    let mut board = board(vec![
        field("one", "string", true, opts.clone())?,
        field(
            "many",
            "multiselect",
            true,
            json!({"options":opts["options"],"minItems":1,"maxItems":1}),
        )?,
    ])?;
    board.key(key(KeyCode::Down));
    assert!(board.key(submit()).is_none());
    assert!(board.drafts[0].values.is_empty());
    board.paste("unoffered");
    assert!(board.drafts[0].values.is_empty());
    board.key(key(KeyCode::Enter));
    assert_eq!(
        board.drafts[0].values.get("one"),
        Some(&FormValue::Text("canary".into()))
    );
    board.key(key(KeyCode::Tab));
    board.key(key(KeyCode::Char(' ')));
    board.key(key(KeyCode::Down));
    board.key(key(KeyCode::Char(' ')));
    assert!(board.key(submit()).is_none());
    assert!(board.drafts[0].note.contains("Too many"));
    board.key(key(KeyCode::Char(' ')));
    board.key(key(KeyCode::BackTab));
    assert_eq!(board.drafts[0].choices.get("one"), Some(&1));
    let request = board.key(submit()).ok_or("selection answer")?;
    assert!(
        matches!(request.decision, Some(Decision::Answer(ref answer)) if answer.get("many") == Some(&FormValue::Texts(vec!["stable".into()])))
    );
    Ok(())
}

#[test]
fn constraints_unsupported_semantics_precision_and_total_answer_bounds_are_honest() -> Result {
    for text in [
        "NaN",
        "inf",
        "1e999",
        "1e-999",
        "01",
        "18446744073709551616",
        "\"3\"",
    ] {
        assert!(fields::parse_number(text).is_err(), "accepted {text}");
    }
    assert_eq!(
        fields::parse_number("18446744073709551615")?.as_u64(),
        Some(u64::MAX)
    );
    assert!(fields::parse_number("-0")?
        .as_f64()
        .is_some_and(|number| number == 0.0 && number.is_sign_negative()));
    assert_eq!(fields::parse_number("0e-999")?.as_f64(), Some(0.0));
    for attrs in [json!({"when":false}), json!({"exclusiveMinimum":0})] {
        assert!(!supported(&field("count", "number", false, attrs)?));
    }
    assert!(supported(&field(
        "many",
        "multiselect",
        false,
        json!({"options":[{"value":"a","label":"A"}],"custom":true})
    )?));
    let text = field("text", "string", true, json!({"minLength":2,"maxLength":2}))?;
    assert!(fields::validate_value(&text, &FormValue::Text("😀".into())).is_ok());
    assert!(fields::validate_value(&text, &FormValue::Text("😀a".into())).is_err());
    let choices = field(
        "many",
        "multiselect",
        false,
        json!({"options":[{"value":"a"},{"value":"b"}],"minItems":1}),
    )?;
    assert!(fields::validate_value(&choices, &FormValue::Texts(vec![])).is_err());
    assert!(
        fields::validate_value(&choices, &FormValue::Texts(vec!["a".into(), "a".into()])).is_ok()
    );
    assert!(fields::validate_value(&choices, &FormValue::Texts(vec!["c".into()])).is_err());
    let rows = (0..5)
        .map(|i| field(&format!("f{i}"), "string", true, json!({})))
        .collect::<Result<Vec<_>>>()?;
    let board = board(rows)?;
    let answer = (0..5)
        .map(|i| (format!("f{i}"), FormValue::Text("x".repeat(MAX_FIELD))))
        .collect();
    assert!(validate(&board.drafts[0].row, &answer).is_err());
    Ok(())
}

#[test]
fn long_choice_lists_scroll_and_render_labels_without_terminal_controls() -> Result {
    let options = (0..128)
        .map(|i| json!({"value":format!("value{i}"),"label":format!("Label {i}\u{1b}[2J")}))
        .collect::<Vec<_>>();
    let mut board = board(vec![field(
        "one",
        "string",
        false,
        json!({"options":options}),
    )?])?;
    board.key(key(KeyCode::End));
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(34, 14))?;
    terminal.draw(|frame| {
        board.draw(
            frame,
            frame.area(),
            grapt_tui_transcript::palette::Palette::new(false),
        )
    })?;
    let text = terminal
        .backend()
        .buffer()
        .content
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>();
    assert!(text.contains("Label 127"));
    assert!(!text.contains('\u{1b}'));
    assert!(board.drafts[0].values.is_empty());
    Ok(())
}

#[test]
fn every_option_description_line_is_reachable_in_a_short_viewport() -> Result {
    let words = (0..80)
        .map(|i| format!("detailtoken{i:03}"))
        .collect::<Vec<_>>();
    let mut board = board(vec![field(
        "one",
        "string",
        false,
        json!({"options":[{"value":"offered", "label":"Offered label", "description":words.join("\n")}]}),
    )?])?;
    let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(34, 14))?;
    for direction in [KeyCode::PageDown, KeyCode::PageUp] {
        let mut observed = String::new();
        for _ in 0..words.len() + 64 {
            terminal.draw(|frame| {
                board.draw(
                    frame,
                    frame.area(),
                    grapt_tui_transcript::palette::Palette::new(false),
                )
            })?;
            for cell in &terminal.backend().buffer().content {
                observed.push_str(cell.symbol());
            }
            observed.push('\n');
            board.key(key(direction));
        }
        let missing = words
            .iter()
            .filter(|word| !observed.contains(word.as_str()))
            .collect::<Vec<_>>();
        assert!(
            missing.is_empty(),
            "detail lines skipped with {direction:?}: {missing:?}"
        );
    }
    Ok(())
}
