// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// . Grapt adds bounded end repeat.
use super::{MouseButton, MouseEvent, MouseEventKind, Position, Rect};
use crate::ui::Ui;
use std::time::Duration;

pub(crate) enum Grab {
    Thumb {
        area: Rect,
        offset: u16,
    },
    Step {
        area: Rect,
        point: Position,
        rows: i64,
        next: Option<Duration>,
    },
}

pub(super) fn handle(event: MouseEvent, ui: &mut Ui) -> bool {
    let point = Position::new(event.column, event.row);
    if event.kind == MouseEventKind::Up(MouseButton::Left) {
        let grabbed = ui.scrollbar_grab.is_some();
        super::cancel(ui);
        return grabbed;
    }
    if event.kind == MouseEventKind::Drag(MouseButton::Left) {
        let Some(grab) = &ui.scrollbar_grab else {
            return false;
        };
        match grab {
            Grab::Step { point: held, .. } => {
                if *held != point {
                    super::cancel(ui);
                }
            }
            Grab::Thumb { area, offset } => {
                if *area == ui.scrollbar_area && eligible(ui) {
                    ui.launch.cancel();
                    ui.transcript.drag_scrollbar(*area, event.row, *offset);
                } else {
                    super::cancel(ui);
                }
            }
        }
        return true;
    }
    if event.kind != MouseEventKind::Down(MouseButton::Left) || !eligible(ui) {
        return false;
    }
    let Some(bar) = ui.transcript.scrollbar_geometry(ui.scrollbar_area) else {
        return false;
    };
    // Include the adjacent padding column without taking text.
    let zone = Rect::new(
        bar.area.x.saturating_sub(1),
        bar.area.y,
        bar.area.width.saturating_add(u16::from(bar.area.x > 0)),
        bar.area.height,
    );
    if !zone.contains(point) {
        super::cancel(ui);
        return false;
    }
    ui.launch.cancel();
    ui.selection.clear();
    super::cancel(ui);
    let on_thumb = event.row >= bar.thumb.y && event.row < bar.thumb.bottom();
    if ui.navigation.loading() && !on_thumb {
        return true;
    }
    if bar.track != bar.area && (event.row == bar.area.y || event.row == bar.area.bottom() - 1) {
        let rows = if event.row == bar.area.y { 1 } else { -1 };
        ui.navigation.scroll(rows);
        ui.scrollbar_grab = Some(Grab::Step {
            area: bar.area,
            point,
            rows,
            next: None,
        });
    } else if event.row >= bar.thumb.y && event.row < bar.thumb.bottom() {
        ui.navigation.clear_scroll();
        ui.transcript.begin_scrollbar_drag(bar.area);
        ui.scrollbar_grab = Some(Grab::Thumb {
            area: bar.area,
            offset: event.row - bar.thumb.y,
        });
    } else {
        let page = bar.visible.min(1_000_000) as i64;
        ui.navigation
            .scroll(if event.row < bar.thumb.y { page } else { -page });
    }
    true
}

fn eligible(ui: &Ui) -> bool {
    !super::blocked(ui) && !ui.mentions.visible() && !ui.slash.visible()
}

pub(crate) fn ticking(ui: &Ui) -> bool {
    matches!(ui.scrollbar_grab, Some(Grab::Step { .. }))
}

/// Uses the event loop's elapsed time; one step per tick prevents catch-up bursts.
pub(crate) fn tick(ui: &mut Ui, now: Duration) -> bool {
    if !eligible(ui)
        || (ticking(ui) && ui.navigation.loading())
        || (ticking(ui)
            && ui
                .transcript
                .scrollbar_geometry(ui.scrollbar_area)
                .is_none())
    {
        super::cancel(ui);
        return false;
    }
    let Some(Grab::Step {
        area, rows, next, ..
    }) = &mut ui.scrollbar_grab
    else {
        return false;
    };
    if *area != ui.scrollbar_area {
        super::cancel(ui);
        return false;
    }
    let Some(deadline) = *next else {
        *next = Some(now.saturating_add(Duration::from_millis(400)));
        return false;
    };
    if now < deadline {
        return false;
    }
    *next = Some(now.saturating_add(Duration::from_millis(70)));
    ui.launch.cancel();
    ui.navigation.scroll(*rows);
    true
}
