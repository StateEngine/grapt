use super::*;
use ratatui::{backend::TestBackend, widgets::Paragraph, Terminal};

fn transcript() -> Transcript {
    let mut transcript = Transcript::new();
    transcript.update(
        true,
        0,
        (0..200)
            .map(|ordinal| Slice {
                compaction: None,
                kind: SliceKind::Text,
                tool_id: None,
                tool_phase: None,
                ordinal,
                text: format!("row {ordinal} **bold** 中👩🏽‍💻\n\ncontinued\n"),
                read_detail_start: None,
                tool_summary: None,
                marks: Vec::new(),
                closed: true,
                footer: None,
                truncated_bytes: 0,
            })
            .collect(),
    );
    transcript
}

#[test]
fn borrowed_rows_match_paragraph_cells_and_paging_through_resize_and_empty_rectangles(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut borrowed = transcript();
    let mut owned = transcript();
    for light in [false, true] {
        for (width, height) in [(1, 0), (4, 1), (40, 33), (80, 260)] {
            for top in [false, true] {
                for scroll in [0, 1, 29, 100_000] {
                    borrowed.scroll = scroll;
                    borrowed.from_top = top;
                    owned.scroll = scroll;
                    owned.from_top = top;
                    let mut actual = Terminal::new(TestBackend::new(width + 4, height + 2))?;
                    let mut expected = Terminal::new(TestBackend::new(width + 4, height + 2))?;
                    let area = Rect::new(2, 1, width, height);
                    let palette = Palette::new(light);
                    actual.draw(|frame| borrowed.paint(frame, area, palette))?;
                    expected.draw(|frame| {
                        frame.render_widget(
                            Paragraph::new(owned.visible(width, height, palette)),
                            area,
                        )
                    })?;
                    assert_eq!(
                        actual.backend().buffer(),
                        expected.backend().buffer(),
                        "{width}x{height} scroll={scroll} top={top}"
                    );
                    assert_eq!(
                        (borrowed.scroll, borrowed.from_top, borrowed.overflow),
                        (owned.scroll, owned.from_top, owned.overflow)
                    );
                }
            }
        }
    }
    Ok(())
}

fn text_slice(text: &str, closed: bool) -> Slice {
    Slice {
        compaction: None,
        kind: SliceKind::Text,
        tool_id: None,
        tool_phase: None,
        ordinal: 0,
        text: text.to_owned(),
        read_detail_start: None,
        tool_summary: None,
        marks: Vec::new(),
        closed,
        footer: None,
        truncated_bytes: 0,
    }
}

#[test]
fn identical_completion_reuses_rows_and_still_publishes_the_final_partial_line() {
    for light in [false, true] {
        for width in [1, 40] {
            for source in [
                "- First **item**\n- Second item\n\nAfter list.\n",
                "First line\nFinal **partial** line",
                "```text\ncode\n```",
                "Only a partial line",
            ] {
                let palette = Palette::new(light);
                let mut streamed = Transcript::new();
                streamed.update(true, 0, vec![text_slice(source, false)]);
                while streamed.draining() {
                    streamed.drain();
                }
                streamed.layout(0, width, palette);
                let epoch = streamed.paint_epoch;
                assert!(streamed.update(false, 0, vec![text_slice(source, true)]));
                while streamed.draining() {
                    streamed.drain();
                }
                let actual = streamed.layout(0, width, palette).to_vec();
                if source.ends_with('\n') {
                    assert_eq!(streamed.paint_epoch, epoch, "identical rows stay cached");
                }
                let mut fresh = Transcript::new();
                fresh.update(true, 0, vec![text_slice(source, true)]);
                assert_eq!(actual, fresh.layout(0, width, palette), "{source:?}");
            }
        }
    }
}

#[test]
fn changed_completion_source_or_truncation_rebuilds_the_output() {
    for replacement in [false, true] {
        let mut transcript = Transcript::new();
        transcript.update(true, 0, vec![text_slice("First line\n", false)]);
        while transcript.draining() {
            transcript.drain();
        }
        let palette = Palette::new(false);
        transcript.layout(0, 40, palette);
        let epoch = transcript.paint_epoch;
        let mut closed = text_slice(
            if replacement {
                "New source\n"
            } else {
                "First line\n"
            },
            true,
        );
        closed.truncated_bytes = 12;
        transcript.update(false, 0, vec![closed.clone()]);
        while transcript.draining() {
            transcript.drain();
        }
        let actual = transcript.layout(0, 40, palette).to_vec();
        assert!(transcript.paint_epoch > epoch);
        let mut fresh = Transcript::new();
        fresh.update(true, 0, vec![closed]);
        assert_eq!(actual, fresh.layout(0, 40, palette));
    }
}

#[test]
fn automated_packet_is_one_line_until_expanded_and_never_a_recallable_prompt() {
    let mut transcript = Transcript::new();
    let mut slice = text_slice(
        "PRIVATE_WAKE_PACKET\nresume instructions\nmail instructions",
        true,
    );
    slice.kind = SliceKind::Automated;
    transcript.update(true, 0, vec![slice]);
    let palette = Palette::new(false);
    let collapsed = transcript.visible(120, 40, palette);
    assert_eq!(collapsed.len(), 1);
    assert!(collapsed[0].to_string().contains("F8 expand"));
    assert!(!collapsed[0].to_string().contains("PRIVATE_WAKE_PACKET"));
    assert!(transcript.latest_prompt().is_none());
    assert_eq!(transcript.prompts().count(), 0);
    transcript.toggle_details();
    let expanded = transcript.visible(120, 40, palette);
    assert!(expanded
        .iter()
        .any(|row| row.to_string().contains("PRIVATE_WAKE_PACKET")));
    transcript.toggle_details();
    assert_eq!(transcript.visible(120, 40, palette).len(), 1);
}

#[test]
fn collapsed_packets_share_the_existing_rendered_cache_bound() {
    let mut transcript = Transcript::new();
    let slices = (0..200)
        .map(|ordinal| {
            let mut slice = text_slice("packet", true);
            slice.kind = SliceKind::SeatStartup;
            slice.ordinal = ordinal;
            slice
        })
        .collect();
    transcript.update(true, 0, slices);
    for index in 0..200 {
        transcript.layout(index, 80, Palette::new(false));
    }
    assert_eq!(transcript.cached.len(), 128);
    assert_eq!(
        transcript
            .cells
            .iter()
            .filter(|cell| cell.cache.is_some())
            .count(),
        128
    );
}

#[test]
fn assistant_prose_retains_reply_text_across_tool_disclosure(
) -> Result<(), Box<dyn std::error::Error>> {
    for light in [false, true] {
        let p = Palette::new(light);
        let mut transcript = Transcript::new();
        transcript.update(
            true,
            0,
            vec![
                text_slice("First **answer**.", true),
                text_slice("After tools.", true),
            ],
        );
        let rows = transcript.visible(40, 20, p);
        let text = rows
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(text.contains("First answer.") && text.contains("After tools."));

        let mut tool = text_slice("shell: cargo check", true);
        tool.kind = SliceKind::Tool;
        tool.tool_id = Some(grapt_client::wire::tool_call_id("call_glyph_fixture")?);
        tool.tool_phase = Some(ToolPhase::Completed);
        let mut reference = Transcript::new();
        reference.update(true, 0, vec![tool.clone()]);
        let expected_tool = reference.layout(0, 40, p).to_vec();
        transcript.update(
            true,
            0,
            vec![
                text_slice("Before tool.", true),
                tool,
                text_slice("After tool.", true),
            ],
        );
        assert_eq!(transcript.layout(1, 40, p), expected_tool);
        let shown = transcript.visible(40, 20, p);
        assert!(shown.iter().any(|l| l.to_string() == "● Before tool."));
        assert!(shown.iter().any(|l| l.to_string() == "● After tool."));
        assert!(!shown.iter().any(|l| l.to_string().contains("Grapt")));
    }
    Ok(())
}

#[test]
fn prose_gutter_cold_warm_and_streamed_heights_match_painted_rows(
) -> Result<(), Box<dyn std::error::Error>> {
    for light in [false, true] {
        let p = Palette::new(light);
        for width in [1, 2, 3, 9, 40, 80] {
            for source in [
                "",
                "First line\nSecond line\n",
                "# Title\n\n**bold** 界👩‍💻 text\n\n- first\n- second\n",
                "```text\ncode line\nanother line\n```\n",
            ] {
                let mut cold = Transcript::new();
                cold.update(true, 0, vec![text_slice(source, true)]);
                let height = cold.height(0, width);
                assert_eq!(cold.layout(0, width, p).len(), height, "{width} {source:?}");
                assert_eq!(cold.height(0, width), height);
                let expected = cold.visible(width, 200, p);
                assert_eq!(expected.len(), height);
                assert!(expected
                    .iter()
                    .all(|line| line.width() <= usize::from(width)));

                let mut streamed = Transcript::new();
                let mut end = 0;
                for line in source.split_inclusive('\n') {
                    end += line.len();
                    streamed.update(false, 0, vec![text_slice(&source[..end], false)]);
                    while streamed.draining() {
                        streamed.drain();
                    }
                    streamed.visible(width, 200, p);
                }
                streamed.update(false, 0, vec![text_slice(source, true)]);
                while streamed.draining() {
                    streamed.drain();
                }
                assert_eq!(
                    streamed.visible(width, 200, p),
                    expected,
                    "{width} {source:?}"
                );
                let mut actual = Terminal::new(TestBackend::new(width, 200))?;
                let mut reference = Terminal::new(TestBackend::new(width, 200))?;
                actual.draw(|f| {
                    let area = f.area();
                    streamed.paint(f, area, p);
                })?;
                reference.draw(|f| f.render_widget(Paragraph::new(expected.clone()), f.area()))?;
                assert_eq!(actual.backend().buffer(), reference.backend().buffer());
                let canvas = actual.backend().buffer().clone();
                actual.draw(|f| {
                    f.buffer_mut().clone_from(&canvas);
                    let area = f.area();
                    streamed.paint_changed(f, area, p, Some(&canvas));
                })?;
                assert_eq!(actual.backend().buffer(), reference.backend().buffer());
            }
        }
    }
    Ok(())
}
