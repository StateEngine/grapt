use super::*;
use ratatui::{backend::TestBackend, Terminal};
type Result = std::result::Result<(), Box<dyn std::error::Error>>;

#[test]
fn folded_notices_stay_one_shaded_row_and_expansion_keeps_their_identity() {
    let p = Palette::new(false);
    let folded = notice("Automated prompt · F8 expand", 40, p);
    assert!(folded.to_string().contains("Automated prompt · F8 expand"));
    for (kind, title) in [
        (SliceKind::Automated, "Automated prompt"),
        (SliceKind::SeatStartup, "Seat startup prompt"),
    ] {
        let expanded = rows(kind, "Please review the new message.", 40, p);
        assert!(expanded.iter().any(|row| row.to_string().contains(title)));
        assert!(expanded
            .iter()
            .any(|row| row.to_string().contains("Please review")));
    }
}

#[test]
fn prompt_text_is_literal_and_tiny_widths_stay_bounded() {
    for width in 0..=8 {
        let rows = rows(
            SliceKind::User,
            "\x1b[31m中👩‍💻hello",
            width,
            Palette::new(false),
        );
        assert!(rows.iter().all(|line| !line.to_string().contains('\x1b')));
        assert!(rows.iter().all(|line| line.width() <= usize::from(width)));
    }
}

#[test]
fn human_prompts_and_wrapped_notices_have_distinct_surfaces_and_exact_heights() -> Result {
    use super::super::Transcript;
    use grapt_client::Slice;
    for light in [false, true] {
        for width in [12, 40, 80] {
            let p = Palette::new(light);
            let mut transcript = Transcript::new();
            let slices = [
                (SliceKind::User, "Check this layout"),
                (
                    SliceKind::System,
                    "Mail arrived. Please review the latest instruction.",
                ),
            ]
            .into_iter()
            .enumerate()
            .map(|(ordinal, (kind, text))| Slice {
                compaction: None,
                kind,
                ordinal,
                text: text.to_owned(),
                tool_id: None,
                tool_phase: None,
                read_detail_start: None,
                tool_summary: None,
                marks: Vec::new(),
                closed: true,
                footer: None,
                truncated_bytes: 0,
            })
            .collect();
            transcript.update(true, 0, slices);
            let user_height = transcript.height(0, width);
            let notice_height = transcript.height(1, width);
            assert_eq!(transcript.layout(0, width, p).len(), user_height);
            let notice = transcript.layout(1, width, p);
            assert_eq!(notice.len(), notice_height);
            assert!(notice[0].to_string().starts_with("   ◈ Mail"));
            assert!(!notice
                .iter()
                .any(|line| line.to_string().contains("Session")));
            let mut terminal = Terminal::new(TestBackend::new(
                width,
                (user_height + notice_height) as u16,
            ))?;
            transcript.from_top = true;
            terminal.draw(|frame| {
                frame.render_widget(
                    ratatui::widgets::Block::default().style(p.normal()),
                    frame.area(),
                );
                transcript.paint(frame, frame.area(), p);
            })?;
            let buffer = terminal.backend().buffer();
            let notice_y = user_height as u16;
            assert_eq!(buffer[(3, notice_y)].symbol(), "◈");
            assert_eq!(buffer[(3, notice_y)].fg, p.muted);
            for y in notice_y..(user_height + notice_height - 1) as u16 {
                for x in 0..width {
                    assert_eq!(buffer[(x, y)].bg, p.background);
                }
            }
        }
    }
    Ok(())
}

#[test]
fn ordinary_notices_are_literal_and_bounded_at_tiny_widths() {
    assert!(!is_prompt(SliceKind::System));
    for width in 0..=8 {
        let rows = super::super::notices::rows("\x1b[31m中👩‍💻hello", width, Palette::new(false));
        assert!(rows.iter().all(|line| !line.to_string().contains('\x1b')));
        assert!(rows.iter().all(|line| line.width() <= usize::from(width)));
    }
}
