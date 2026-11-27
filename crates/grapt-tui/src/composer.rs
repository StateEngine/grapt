//! The editor owns text/history. Presentation measures whole grapheme clusters,
//! including when its scalar cursor falls within a joined emoji.
use grapt_tui_transcript::palette::Palette;
use ratatui::text::Line;

/// Paint the editor region completely, including stale characters and borders.
pub fn paint(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    composer_rows: Vec<Line<'static>>,
    cursor_row: usize,
    p: Palette,
    activity: Option<&str>,
    images: &mut crate::images::Images,
) {
    use ratatui::{
        layout::{Margin, Rect},
        style::Style,
        widgets::{Block, Borders, Clear, Paragraph},
    };
    frame.render_widget(Clear, area);
    status(frame, area, activity, p);
    let area = Rect {
        y: area.y.saturating_add(u16::from(activity.is_some())),
        height: area.height.saturating_sub(u16::from(activity.is_some())),
        ..area
    };
    frame.render_widget(Block::default().style(p.normal()), area);
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(p.muted))
        .style(p.normal());
    let inner = block.inner(area).inner(Margin::new(1, 0));
    frame.render_widget(block, area);
    let image_rows = (images.len() as u16).min(inner.height.saturating_sub(1));
    images.paint(
        frame,
        Rect {
            height: image_rows,
            ..inner
        },
        p,
    );
    let inner = Rect {
        y: inner.y.saturating_add(image_rows),
        height: inner.height.saturating_sub(image_rows),
        ..inner
    };
    frame.render_widget(
        Paragraph::new("❯ ").style(p.normal().fg(p.accent)),
        Rect {
            width: 2.min(inner.width),
            ..inner
        },
    );
    let top = cursor_row.saturating_sub(usize::from(inner.height.saturating_sub(1)));
    frame.render_widget(
        Paragraph::new(
            composer_rows
                .into_iter()
                .skip(top)
                .take(usize::from(inner.height))
                .collect::<Vec<_>>(),
        ),
        Rect {
            x: inner.x.saturating_add(2),
            width: inner.width.saturating_sub(2),
            ..inner
        },
    );
}

pub fn area(
    viewport: ratatui::layout::Rect,
    rows: usize,
    recovery_rows: usize,
    switcher_rows: u16,
    activity: bool,
    footer_rows: u16,
) -> ratatui::layout::Rect {
    layout(
        viewport,
        rows,
        recovery_rows,
        switcher_rows,
        activity,
        None,
        footer_rows,
    )[2]
}

/// Full and immediate paints use the same header, composer, switcher and footer rows.
pub fn layout(
    viewport: ratatui::layout::Rect,
    rows: usize,
    recovery_rows: usize,
    switcher_rows: u16,
    activity: bool,
    input_band_rows: Option<u16>,
    footer_rows: u16,
) -> [ratatui::layout::Rect; 5] {
    use ratatui::layout::{Constraint, Direction, Layout};
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1 + recovery_rows as u16),
            Constraint::Min(1),
            Constraint::Length(
                input_band_rows.unwrap_or((rows.clamp(1, 5) as u16) + 1 + u16::from(activity)),
            ),
            Constraint::Length(switcher_rows),
            Constraint::Length(footer_rows),
        ])
        .split(viewport);
    [chunks[0], chunks[1], chunks[2], chunks[3], chunks[4]]
}

/// The live row repaints without revisiting history or the editable prompt.
pub fn status(
    frame: &mut ratatui::Frame<'_>,
    area: ratatui::layout::Rect,
    label: Option<&str>,
    p: Palette,
) {
    if let Some(label) = label {
        let row = ratatui::layout::Rect {
            height: area.height.min(1),
            ..area
        };
        // Own the margins too: the composer's Clear leaves a terminal-default
        // background there, which need not match the selected theme.
        let mut line = format!(
            " {}",
            grapt_client::cells::cut(label, usize::from(row.width.saturating_sub(2)))
        );
        let padding = usize::from(row.width)
            .saturating_sub(unicode_width::UnicodeWidthStr::width(line.as_str()));
        line.extend(std::iter::repeat_n(' ', padding));
        frame.render_widget(
            ratatui::widgets::Paragraph::new(line)
                .style(ratatui::style::Style::reset().fg(p.accent).bg(p.background)),
            row,
        );
    }
}
