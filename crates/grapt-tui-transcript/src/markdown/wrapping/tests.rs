use super::*;
use crate::{
    markdown::{Incremental, Markdown},
    palette::Palette,
    transcript::Transcript,
};
use grapt_client::{wire, Bounds, SessionView};
use ratatui::{
    backend::TestBackend,
    layout::Rect,
    style::{Color, Modifier},
    Terminal,
};

// Audit run-04, frames 11-second-turn and 01-first-turn-folded.
// The original 114-column prose rows ended in "negativ" and "comple".
const NEXT_CHECK: &str = "The failure was an absent optional config file, not a failed arithmetic test.\n\nNext, add a regression case for a negative value paired with zero. The existing four cases cover positive, negative, mixed-sign and zero inputs.";
const RESULT: &str = "Signed addition is fixed. All four tests passed.\n\nThe optional config file was absent; that read failed without affecting the change. The long validation log completed successfully.\n\nThe change keeps negative operands signed instead of applying absolute values.";

fn strings(lines: &[Line<'_>]) -> Vec<String> {
    lines.iter().map(ToString::to_string).collect()
}

#[test]
fn audit_prose_keeps_complete_words_at_the_operated_width() {
    for light in [false, true] {
        let renderer = Markdown::new();
        for source in [NEXT_CHECK, RESULT] {
            let rows = renderer.render(source, 114, Palette::new(light));
            assert_eq!(renderer.height(source, 114), rows.len());
            assert!(rows.iter().all(|row| row.width() <= 114));
            assert!(!rows.iter().any(|row| {
                let text = row.to_string();
                text.ends_with("negativ") || text.ends_with("comple")
            }));
        }
        let rows = strings(&renderer.render(NEXT_CHECK, 114, Palette::new(light)));
        assert!(rows
            .iter()
            .any(|row| row == "negative, mixed-sign and zero inputs."));
        assert!(rows.iter().any(|row| row.ends_with("cover positive,")));
    }
}

#[test]
fn audit_reply_through_the_public_codec_paints_word_boundaries(
) -> Result<(), Box<dyn std::error::Error>> {
    let dto = wire::assistant_message(
        "msg_audit_words",
        &[wire::MessageItem::Text(NEXT_CHECK.into())],
        Some(wire::DurableFinish::Stop),
        None,
        None,
    )?;
    let page = wire::decode_messages(&wire::encode_messages(vec![dto], None)?)?;
    let mut view = SessionView::new(&wire::session_id("ses_audit_words")?);
    for message in &page.messages {
        view.apply_durable(message, Bounds::PRODUCTION)?;
    }
    for light in [false, true] {
        let mut transcript = Transcript::new();
        transcript.update(true, 0, view.slices.clone());
        let mut terminal = Terminal::new(TestBackend::new(120, 40))?;
        terminal
            .draw(|frame| transcript.paint(frame, Rect::new(2, 0, 116, 40), Palette::new(light)))?;
        let rows: Vec<String> = (0..40)
            .map(|y| {
                (0..120)
                    .map(|x| terminal.backend().buffer()[(x, y)].symbol())
                    .collect()
            })
            .collect();
        let index = rows
            .iter()
            .position(|row| row.contains("Next, add a regression"))
            .ok_or("audit paragraph absent")?;
        assert!(rows[index].trim_end().ends_with("cover positive,"));
        assert!(rows[index + 1].trim() == "negative, mixed-sign and zero inputs.");
    }
    Ok(())
}

#[test]
fn words_and_clusters_cross_styles_without_losing_their_styles() {
    let rows = prose(
        vec![Line::from(vec![
            Span::raw("alpha nega"),
            Span::styled("tive", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled(" 👩", Style::default().fg(Color::Red)),
            Span::styled("\u{200d}💻e\u{301}", Style::default().fg(Color::Blue)),
        ])],
        8,
    );
    assert_eq!(strings(&rows), ["alpha", "negative", "👩‍💻é"]);
    assert!(rows[1]
        .spans
        .iter()
        .any(|span| span.content == "tive" && span.style.add_modifier.contains(Modifier::BOLD)));
    assert!(rows[2]
        .spans
        .iter()
        .any(|span| span.content == "👩‍💻" && span.style.fg == Some(Color::Red)));
    assert!(rows.iter().all(|row| row.width() <= 8));
}

#[test]
fn overlong_tokens_use_whole_graphemes_and_literal_paths_keep_their_breaks() {
    assert_eq!(
        strings(&prose(vec![Line::raw("ab👩‍💻écd")], 1)),
        ["a", "b", "�", "é", "c", "d"]
    );
    assert_eq!(
        strings(&prose(vec![Line::raw("abc defghijklm")], 5)),
        ["abc", "defgh", "ijklm"]
    );
    assert_eq!(
        strings(&prose(vec![Line::raw("go a\u{a0}b now")], 4)),
        ["go", "a\u{a0}b", "now"]
    );
    assert_eq!(
        strings(&literal(vec![Line::raw("alpha beta gamma")], 8)),
        ["alpha be", "ta gamma"]
    );
    let renderer = Markdown::new();
    let code = "```text\nalpha beta gamma\n```";
    let rows = renderer.render(code, 8, Palette::new(false));
    assert_eq!(strings(&rows), ["  text", "alpha be", "ta gamma"]);
    assert_eq!(renderer.height(code, 8), rows.len());
}

#[test]
fn appended_audit_paragraphs_match_fresh_rows_and_heights() {
    for source in [NEXT_CHECK, RESULT] {
        for width in [1, 8, 32, 114] {
            for light in [false, true] {
                let renderer = Markdown::new();
                let palette = Palette::new(light);
                let mut incremental = Incremental::default();
                let mut rows = vec![Line::raw("reserved heading")];
                for end in source.char_indices().map(|(i, c)| i + c.len_utf8()) {
                    let prefix = &source[..end];
                    incremental.append(&renderer, prefix, width, palette, &mut rows);
                    assert_eq!(
                        &rows[1..],
                        renderer.render(prefix, width, palette),
                        "width {width}, source {prefix:?}"
                    );
                    assert_eq!(renderer.height(prefix, width), rows.len() - 1);
                }
            }
        }
    }
}
