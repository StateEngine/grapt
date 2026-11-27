use super::*;
use ratatui::{buffer::Cell, style::Modifier, text::Span};

fn key(row: usize, epoch: u64) -> PaintedRow {
    PaintedRow {
        cell: 0,
        row,
        epoch,
    }
}
fn reference(buffer: &mut Buffer, area: Rect, line: &Line<'_>, palette: Palette) {
    Clear.render(area, buffer);
    Block::default()
        .style(palette.normal())
        .render(area, buffer);
    line.render(area, buffer);
}

#[test]
fn pixels_preserve_styles_unicode_alignment_shortening_and_clipped_hits() {
    let lines = [
        Line::raw("long old text filling the row"),
        Line::from(vec![
            Span::styled(
                "界e\u{301}👩‍💻",
                ratatui::style::Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("\t tail"),
        ]),
        Line::styled(
            "styled space  ",
            ratatui::style::Style::default()
                .bg(ratatui::style::Color::Blue)
                .add_modifier(Modifier::UNDERLINED),
        ),
        Line::raw("x"),
        Line::default(),
        Line::raw("right").right_aligned(),
        Line::raw("center").centered(),
    ];
    for light in [false, true] {
        let palette = Palette::new(light);
        let mut cache = Raster::default();
        for width in [1, 2, 7, 30] {
            for (epoch, line) in lines.iter().enumerate() {
                let mut actual = Buffer::filled(Rect::new(0, 0, 40, 5), Cell::new("!"));
                let mut expected = actual.clone();
                let area = Rect::new(3, 1, width, 1);
                let previous = actual.clone();
                cache.begin(area, palette);
                cache.paint(
                    &mut actual,
                    area,
                    key(0, epoch as u64),
                    line,
                    palette,
                    Some(&previous),
                );
                cache.finish();
                reference(&mut expected, area, line, palette);
                assert_eq!(actual, expected);
                let moved = Rect::new(5, 3, width, 1);
                let previous = actual.clone();
                cache.begin(moved, palette);
                cache.paint(
                    &mut actual,
                    moved,
                    key(0, epoch as u64),
                    line,
                    palette,
                    Some(&previous),
                );
                cache.finish();
                reference(&mut expected, moved, line, palette);
                assert_eq!(actual, expected);
                let crop = Rect::new(7, 3, width.saturating_sub(2), 1);
                let previous = actual.clone();
                let mut clipped = Buffer::filled(crop, Cell::new("!"));
                cache.begin(moved, palette);
                cache.paint(
                    &mut clipped,
                    moved,
                    key(0, epoch as u64),
                    line,
                    palette,
                    Some(&previous),
                );
                cache.finish();
                for x in crop.x..crop.right() {
                    assert_eq!(clipped[(x, 3)], expected[(x, 3)]);
                }
            }
        }
    }
}

#[test]
fn scrolling_and_full_paints_keep_row_identity_without_stale_pixels() {
    let palette = Palette::new(false);
    let area = Rect::new(2, 1, 12, 3);
    let mut actual = Buffer::empty(Rect::new(0, 0, 20, 6));
    let mut expected = actual.clone();
    let mut cache = Raster::default();
    for start in 0..10 {
        let previous = actual.clone();
        cache.begin(area, palette);
        for position in 0..3 {
            let row = start + position;
            let line = Line::raw(format!("row {row} 界"));
            let destination = Rect::new(area.x, area.y + position as u16, area.width, 1);
            cache.paint(
                &mut actual,
                destination,
                key(row, 1),
                &line,
                palette,
                Some(&previous),
            );
            reference(&mut expected, destination, &line, palette);
        }
        cache.finish();
        assert_eq!(actual, expected);
        assert_eq!(cache.rows.len(), 3);
        assert_eq!(cache.rendered, start + 3);
    }
    // A full UI paint can move a row while retaining its content identity.
    let moved = Rect::new(4, 2, 12, 1);
    let line = Line::raw("row 9 界");
    reference(&mut actual, moved, &line, palette);
    reference(&mut expected, moved, &line, palette);
    cache.begin(moved, palette);
    cache.keep(key(9, 1), moved);
    cache.finish();
    let previous = actual.clone();
    let destination = Rect::new(1, 4, 12, 1);
    cache.begin(destination, palette);
    cache.paint(
        &mut actual,
        destination,
        key(9, 1),
        &line,
        palette,
        Some(&previous),
    );
    cache.finish();
    reference(&mut expected, destination, &line, palette);
    assert_eq!(actual, expected);
    // Missing prior canvas takes the ordinary widget path.
    actual = Buffer::filled(actual.area, Cell::new("!"));
    expected = actual.clone();
    cache.begin(destination, palette);
    cache.paint(&mut actual, destination, key(9, 1), &line, palette, None);
    cache.finish();
    reference(&mut expected, destination, &line, palette);
    assert_eq!(actual, expected);
    cache.begin(Rect::new(1, 4, 11, 1), palette);
    assert!(cache.rows.is_empty());
    cache.begin(destination, Palette::new(true));
    assert!(cache.rows.is_empty());
}

#[test]
fn row_budget_evicts_old_viewports_and_falls_back_without_stale_content() {
    let palette = Palette::new(false);
    let mut cache = Raster::default();
    let area = Rect::new(0, 0, 1000, 40);
    let mut actual = Buffer::filled(area, Cell::new("!"));
    let mut expected = actual.clone();
    for epoch in 0..3 {
        let previous = actual.clone();
        cache.begin(area, palette);
        for row in 0..40 {
            let destination = Rect::new(0, row, 1000, 1);
            let line = Line::raw("short 界");
            cache.paint(
                &mut actual,
                destination,
                key(usize::from(row), epoch),
                &line,
                palette,
                Some(&previous),
            );
            reference(&mut expected, destination, &line, palette);
            assert!(cache.rows.len() * 1000 <= 2 * MAX_CELLS);
        }
        cache.finish();
        assert_eq!(actual, expected);
        assert_eq!(cache.rows.len(), 32);
    }
}
