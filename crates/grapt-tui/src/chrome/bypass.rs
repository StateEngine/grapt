// Copyright 2025 OpenAI. Licensed under Apache-2.0.
// . Modified by Grapt: reserve
// the right edge for the effective bypass warning, wrapping before clipping.
use super::{cells, inner, Frame, Rect, Ui};
use grapt_client::preset::BYPASS_INDICATOR;
use ratatui::{
    layout::Alignment,
    widgets::{Clear, Paragraph},
};

fn warning_area(area: Rect) -> Rect {
    if area.width > 2 {
        inner(area)
    } else {
        area
    }
}

pub fn footer_height(ui: &Ui, width: u16) -> u16 {
    let area = Rect::new(0, 0, width, 0);
    let warning_rows = if ui.bypass_permissions {
        cells::wrap(BYPASS_INDICATOR, usize::from(warning_area(area).width)).len()
    } else {
        0
    };
    let state = &super::footer(ui, usize::from(inner(area).width))[0];
    warning_rows.max(1) as u16 + u16::from(!state.is_empty())
}

pub fn draw_footer(ui: &Ui, frame: &mut Frame<'_>, area: Rect) {
    let p = ui.palette;
    let inner = inner(area);
    let warning_rows = if ui.bypass_permissions {
        cells::wrap(BYPASS_INDICATOR, usize::from(warning_area(area).width)).len()
    } else {
        0
    };
    // The warning owns the bottom row first; optional labels use what remains.
    let context_width = if warning_rows > 1 {
        0
    } else {
        usize::from(inner.width).saturating_sub(if warning_rows == 1 {
            cells::cells(BYPASS_INDICATOR) + 1
        } else {
            0
        })
    };
    let mut rows = super::footer(ui, usize::from(inner.width));
    rows[1] = super::session_context(ui, context_width);
    frame.render_widget(Clear, area);
    let style = p.normal().fg(if ui.mail_recovery.error(ui.mail_attention) {
        p.bad
    } else {
        p.muted
    });
    if !rows[0].is_empty() && warning_rows < usize::from(area.height) {
        frame.render_widget(
            Paragraph::new(rows[0].as_str()).style(style),
            Rect { height: 1, ..inner },
        );
    }
    if warning_rows <= 1 && inner.height > u16::from(!rows[0].is_empty()) {
        frame.render_widget(
            Paragraph::new(rows[1].as_str()).style(style),
            Rect {
                y: inner.bottom().saturating_sub(1),
                height: 1,
                ..inner
            },
        );
    }
    draw_bypass(ui, frame, area);
}

pub fn draw_bypass(ui: &Ui, frame: &mut Frame<'_>, area: Rect) {
    if !ui.bypass_permissions || area.is_empty() {
        return;
    }
    let area = warning_area(area);
    let rows = cells::wrap(BYPASS_INDICATOR, usize::from(area.width));
    let height = (rows.len() as u16).min(area.height);
    let width = rows.iter().map(|row| cells::cells(row)).max().unwrap_or(0) as u16;
    frame.render_widget(
        Paragraph::new(rows.join("\n"))
            .alignment(Alignment::Right)
            .style(ui.palette.normal().fg(ui.palette.bad)),
        Rect {
            x: area.right().saturating_sub(width),
            y: area.bottom().saturating_sub(height),
            width,
            height,
        },
    );
}
