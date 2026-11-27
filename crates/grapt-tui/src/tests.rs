use crate::ui::Ui;
use grapt_client::{Slice, SliceKind};
use grapt_tui_transcript::{
    markdown::{self, Markdown},
    palette::Palette,
    transcript::Transcript,
};
use ratatui::{backend::TestBackend, Terminal};
use unicode_width::UnicodeWidthStr;

fn slice(text: impl Into<String>, closed: bool) -> Slice {
    Slice {
        compaction: None,
        kind: SliceKind::Text,
        tool_id: None,
        tool_phase: None,
        ordinal: 0,
        text: text.into(),
        read_detail_start: None,
        tool_summary: None,
        marks: Vec::new(),
        closed,
        footer: None,
        truncated_bytes: 0,
    }
}

#[test]
fn unicode_and_controls_remain_bounded_and_visible() {
    let source = "中👩🏽‍💻\u{1b}[2J\u{202e} end";
    let rendered = Markdown::new().render(source, 6, Palette::new(false));
    assert!(rendered.iter().all(|line| line
        .spans
        .iter()
        .map(|s| s.content.width())
        .sum::<usize>()
        <= 6));
    let text = rendered
        .iter()
        .flat_map(|line| line.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("👩🏽‍💻"));
    assert!(text.contains("\\u{1b}"));
    assert!(text.contains("\\u{202e}"));
}

#[test]
fn markdown_code_and_diff_source_survive_narrow_reflow() {
    let source =
        "# Heading\n\n**bold** and `inline`\n\n```rust\nfn main() { println!(\"中👩🏽‍💻\"); }\n```\n";
    for light in [true, false] {
        let lines = Markdown::new().render(source, 36, Palette::new(light));
        assert_eq!(Markdown::new().height(source, 36), lines.len());
        let text = lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .map(|s| s.content.as_ref())
            .collect::<String>();
        assert!(text.contains("Heading"));
        assert!(text.contains("bold"));
        assert!(text.contains("fn main()"));
        assert!(text.contains("中👩🏽‍💻"));
        assert!(lines.iter().all(|line| line.width() <= 36));
    }
    assert_eq!(markdown::literal("a\t\u{7}"), "a    \\u{7}");
}

#[test]
fn scrolling_measures_hidden_cells_without_painting_the_hidden_prefix() {
    let mut transcript = Transcript::new();
    let rows = (0..50_000)
        .map(|ordinal| {
            let mut row = slice(format!("marker {ordinal:05}"), true);
            row.ordinal = ordinal;
            row.kind = SliceKind::User;
            row
        })
        .collect();
    transcript.update(true, 0, rows);
    transcript.visible(80, 20, Palette::new(false));
    transcript.scroll = 30_000;
    let rows = transcript.visible(80, 20, Palette::new(false));
    assert_eq!(rows.len(), 20, "fill the viewport after a large jump");
    assert!(
        transcript.painted_cells() < 20,
        "only visible cells get styled layouts"
    );
    let rows = transcript.visible(40, 20, Palette::new(true));
    assert_eq!(rows.len(), 20, "fill the viewport after a large jump");
    assert!(
        transcript.painted_cells() < 20,
        "resize does not paint the hidden prefix"
    );
}

#[test]
fn live_tail_drains_in_order_and_finalizes_without_losing_unicode() {
    let mut transcript = Transcript::new();
    let source = "hello 中👩🏽‍💻\n".repeat(200);
    transcript.update(true, 0, vec![slice(source.clone(), false)]);
    assert!(transcript.draining());
    transcript.drain();
    assert!(transcript.draining());
    transcript.update(false, 0, vec![slice(source, true)]);
    for _ in 0..100 {
        transcript.drain();
    }
    assert!(!transcript.draining());
    let lines = transcript.visible(80, 20, Palette::new(false));
    assert!(lines
        .iter()
        .any(|line| line.spans.iter().any(|s| s.content.contains("👩🏽‍💻"))));
}

#[test]
fn token_fragments_do_not_paint_until_a_line_or_block_finishes() {
    let mut transcript = Transcript::new();
    let palette = Palette::new(false);
    transcript.update(true, 0, vec![slice("", false)]);
    let initial = transcript.visible(80, 20, palette);
    assert!(!transcript.update(false, 0, vec![slice("partial", false)]));
    assert!(!transcript.draining());
    assert_eq!(transcript.visible(80, 20, palette), initial);
    transcript.update(false, 0, vec![slice("partial line\ntrailing", false)]);
    assert!(transcript.draining());
    assert!(transcript.drain());
    assert!(!transcript.draining());
    let lines = transcript.visible(80, 20, palette);
    let text = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(text.contains("partial line"));
    assert!(!text.contains("trailing"));
    transcript.update(false, 0, vec![slice("partial line\ntrailing", true)]);
    assert!(transcript.drain());
    let lines = transcript.visible(80, 20, palette);
    assert!(lines
        .iter()
        .any(|l| l.spans.iter().any(|s| s.content.contains("trailing"))));
}

#[test]
fn reasoning_and_tools_only_publish_whole_lifecycle_cells() {
    let mut transcript = Transcript::new();
    let mut reasoning = slice("private reasoning fragment", false);
    reasoning.kind = SliceKind::Reasoning;
    transcript.update(true, 0, vec![reasoning.clone()]);
    reasoning.text.push_str(" more");
    assert!(!transcript.update(false, 0, vec![reasoning.clone()]));
    reasoning.closed = true;
    assert!(transcript.update(false, 0, vec![reasoning]));
    assert!(!transcript.draining());
    let mut tool = slice("shell: cargo test", false);
    tool.kind = SliceKind::Tool;
    transcript.update(false, 1, vec![tool.clone()]);
    tool.text.push_str(" buffered output");
    assert!(!transcript.update(false, 1, vec![tool.clone()]));
    tool.closed = true;
    assert!(transcript.update(false, 1, vec![tool]));
    let lines = transcript.visible(80, 20, Palette::new(false));
    let text = lines
        .iter()
        .flat_map(|l| l.spans.iter())
        .map(|s| s.content.as_ref())
        .collect::<String>();
    assert!(!text.contains("private reasoning"));
    assert!(!text.contains("buffered output"));
    transcript.toggle_group(1);
    transcript.toggle_tool(1);
    let text = transcript
        .visible(80, 20, Palette::new(false))
        .iter()
        .map(ToString::to_string)
        .collect::<String>();
    assert!(!text.contains("private reasoning"));
    assert!(text.contains("buffered output"));
}

#[test]
fn partial_tool_arguments_stay_hidden_behind_a_preparing_group() {
    use grapt_client::projection::ToolPhase;
    let mut transcript = Transcript::new();
    let palette = Palette::new(false);
    let mut tool = slice("partial arguments", false);
    tool.kind = SliceKind::Tool;
    tool.tool_phase = Some(ToolPhase::Streaming);
    transcript.update(true, 0, vec![tool.clone()]);
    let preparing = transcript.visible(80, 20, palette);
    assert_eq!(preparing.len(), 2);
    assert!(!preparing[0].to_string().contains("partial arguments"));
    tool.text = "shell: cargo test".into();
    tool.tool_phase = Some(ToolPhase::Running);
    assert!(transcript.update(false, 0, vec![tool.clone()]));
    let started = transcript.visible(80, 20, palette);
    tool.text.push_str(" intermediate");
    assert!(!transcript.update(false, 0, vec![tool.clone()]));
    assert_eq!(started, transcript.visible(80, 20, palette));
    tool.closed = true;
    tool.tool_phase = Some(ToolPhase::Completed);
    assert!(transcript.update(false, 0, vec![tool]));
    assert_ne!(started, transcript.visible(80, 20, palette));
}

#[test]
fn styled_zwj_clusters_use_one_cell_symbol_and_clear_old_wide_text() -> Result<(), String> {
    use ratatui::{
        style::{Color, Style},
        text::{Line, Span},
        widgets::Paragraph,
    };
    let source = Line::from(vec![
        Span::styled("👩", Style::default().fg(Color::Red)),
        Span::styled("\u{200d}", Style::default().fg(Color::Blue)),
        Span::raw("💻"),
    ]);
    let rows = markdown::wrap(vec![source], 2);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].width(), 2);
    assert!(rows[0].spans.iter().any(|span| span.content == "👩‍💻"));
    let mut terminal = Terminal::new(TestBackend::new(8, 2)).map_err(|e| e.to_string())?;
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new(rows.clone()), frame.area()))
        .map_err(|e| e.to_string())?;
    assert_eq!(terminal.backend().buffer()[(0, 0)].symbol(), "👩‍💻");
    terminal
        .draw(|frame| frame.render_widget(Paragraph::new("x"), frame.area()))
        .map_err(|e| e.to_string())?;
    assert_eq!(terminal.backend().buffer()[(2, 0)].symbol(), " ");
    Ok(())
}

#[test]
fn composer_cursor_and_wrap_measure_joined_emoji_as_a_cluster() {
    let mut editor = ratatui_textarea::TextArea::default();
    editor.insert_str("👩🏽‍💻x");
    let (rows, cursor) = grapt_tui_forms::editor::rows(&editor, 3, Palette::new(false));
    assert_eq!(cursor, 1);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].width(), 3);
    assert!(rows[0].spans.iter().any(|span| span.content.contains("👩🏽‍💻")));
    let (rows, _) = grapt_tui_forms::editor::rows(&editor, 2, Palette::new(false));
    assert!(rows.iter().all(|line| line.width() <= 2));
    assert!(rows
        .iter()
        .any(|line| line.spans.iter().any(|span| span.content == "👩🏽‍💻")));
}

#[test]
fn fifty_thousand_messages_leave_the_composer_visible_at_both_geometries() -> Result<(), String> {
    for (width, height, light) in [(120, 40, false), (80, 24, true)] {
        let mut ui = Ui::new(light);
        ui.prompt_ready = true;
        ui.composer.insert_str("xy 中👩🏽‍💻");
        ui.transcript.update(
            true,
            0,
            (0..50_000)
                .map(|i| slice(format!("message {i}"), true))
                .collect(),
        );
        let mut terminal =
            Terminal::new(TestBackend::new(width, height)).map_err(|e| e.to_string())?;
        terminal
            .draw(|frame| ui.draw(frame))
            .map_err(|e| e.to_string())?;
        let text = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("message 49999"));
        assert!(text.contains("❯ xy"));
        ui.transcript.top();
        let rows = ui.transcript.visible(width, height, ui.palette);
        assert!(rows
            .iter()
            .any(|line| line.spans.iter().any(|s| s.content.contains("message 0"))));
    }
    Ok(())
}

#[test]
fn streamed_blocks_match_fresh_rows_after_append_replacement_and_resize() {
    let source = "# Title\n\nFirst paragraph with **bold** and 中👩🏽‍💻.\n\nSecond paragraph\ncontinued.\n\n- first\n- second\n\n| A | B |\n| --- | --- |\n| one | two |\n";
    let palette = Palette::new(false);
    let mut live = Transcript::new();
    let mut end = 0;
    for line in source.split_inclusive('\n') {
        end += line.len();
        live.update(false, 0, vec![slice(&source[..end], false)]);
        while live.draining() {
            live.drain();
        }
        let mut fresh = Transcript::new();
        fresh.update(true, 0, vec![slice(&source[..end], true)]);
        assert_eq!(
            live.visible(40, 1000, palette),
            fresh.visible(40, 1000, palette)
        );
    }
    for (width, palette) in [(80, palette), (40, Palette::new(true))] {
        let mut fresh = Transcript::new();
        fresh.update(true, 0, vec![slice(source, true)]);
        assert_eq!(
            live.visible(width, 1000, palette),
            fresh.visible(width, 1000, palette)
        );
    }
    let replacement = "# Replacement\n\nA different paragraph.\n";
    live.update(false, 0, vec![slice(replacement, false)]);
    while live.draining() {
        live.drain();
    }
    let mut fresh = Transcript::new();
    fresh.update(true, 0, vec![slice(replacement, true)]);
    assert_eq!(
        live.visible(40, 1000, palette),
        fresh.visible(40, 1000, palette)
    );
}
