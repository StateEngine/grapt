// Copyright 2023-2026 SpaceXAI. Licensed under Apache-2.0.
// at 2bdd1d6a6369de0e8c68132ea4539e9abd9e14a8. Modified by Grapt:
// select the painted viewport, with bounded storage and no independent transcript.
use ratatui::{
    buffer::Buffer,
    layout::{Position, Rect},
};
use unicode_width::UnicodeWidthStr;

#[derive(Default)]
pub struct Selection {
    anchor: Option<Position>,
    end: Position,
    dragged: bool,
    pressed: bool,
    canvas: Option<Buffer>,
}

impl Selection {
    pub fn clear(&mut self) -> bool {
        let changed = self.anchor.is_some();
        *self = Self::default();
        changed
    }

    pub fn pressed(&self) -> bool {
        self.pressed
    }

    pub fn start(&mut self, point: Position) {
        self.clear();
        self.anchor = Some(point);
        self.end = point;
        self.pressed = true;
    }

    // Capture once on press, before another paint can move the transcript.
    pub fn capture(&mut self, source: &Buffer, area: Rect) {
        if self.anchor.is_none() || self.canvas.is_some() {
            return;
        }
        if area.is_empty()
            || area != area.intersection(source.area)
            || usize::from(area.width) * usize::from(area.height) > 65_536
        {
            self.clear();
            return;
        }
        let mut canvas = Buffer::empty(area);
        let mut bytes = 0usize;
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                let cell = &source[(x, y)];
                bytes = bytes.saturating_add(cell.symbol().len());
                if bytes > 262_144 {
                    self.clear();
                    return;
                }
                canvas[(x, y)] = cell.clone();
            }
        }
        self.canvas = Some(canvas);
    }

    pub fn drag(&mut self, point: Position) {
        if let (Some(anchor), Some(canvas)) = (self.anchor, &self.canvas) {
            self.end = Position::new(
                point.x.clamp(canvas.area.x, canvas.area.right() - 1),
                point.y.clamp(canvas.area.y, canvas.area.bottom() - 1),
            );
            self.dragged |= self.end != anchor;
        }
    }

    pub fn release(&mut self, point: Position) -> (bool, Option<String>) {
        self.drag(point);
        self.pressed = false;
        (self.dragged, self.text())
    }

    fn ordered(&self) -> Option<(Position, Position)> {
        let anchor = self.anchor?;
        if !self.dragged {
            return None;
        }
        Some(if (anchor.y, anchor.x) <= (self.end.y, self.end.x) {
            (anchor, self.end)
        } else {
            (self.end, anchor)
        })
    }

    fn columns(area: Rect, first: Position, last: Position, y: u16) -> (u16, u16) {
        (
            if y == first.y { first.x } else { area.x },
            if y == last.y {
                last.x.saturating_add(1)
            } else {
                area.right()
            },
        )
    }

    fn text(&self) -> Option<String> {
        let (first, last) = self.ordered()?;
        let canvas = self.canvas.as_ref()?;
        let mut result = String::new();
        for y in first.y..=last.y {
            if y != first.y {
                result.push('\n');
            }
            let (start, end) = Self::columns(canvas.area, first, last, y);
            let mut x = canvas.area.x;
            let mut line = String::new();
            while x < canvas.area.right() {
                let symbol = canvas[(x, y)].symbol();
                let width = UnicodeWidthStr::width(symbol).max(1) as u16;
                if x < end && x.saturating_add(width) > start {
                    line.push_str(symbol);
                }
                x = x.saturating_add(width);
            }
            result.push_str(line.trim_end());
        }
        (!result.trim().is_empty()).then_some(result)
    }

    // Keep the visible text still during a grab while model updates continue.
    pub fn hold(&mut self, target: &mut Buffer, area: Rect) -> bool {
        let Some(canvas) = &self.canvas else {
            return false;
        };
        if !self.pressed {
            return false;
        }
        if area != canvas.area || area != area.intersection(target.area) {
            self.clear();
            return false;
        }
        for y in area.y..area.bottom() {
            for x in area.x..area.right() {
                target[(x, y)] = canvas[(x, y)].clone();
            }
        }
        true
    }

    pub fn paint(
        &mut self,
        target: &mut Buffer,
        area: Rect,
        palette: grapt_tui_transcript::palette::Palette,
    ) {
        let Some((first, last)) = self.ordered() else {
            return;
        };
        let Some(canvas) = &self.canvas else {
            return;
        };
        if area != canvas.area || area != area.intersection(target.area) {
            self.clear();
            return;
        }
        // A stream may continue elsewhere. Only invalidate if the selected rows move.
        for y in first.y..=last.y {
            for x in area.x..area.right() {
                if canvas[(x, y)].symbol() != target[(x, y)].symbol() {
                    self.clear();
                    return;
                }
            }
        }
        for y in first.y..=last.y {
            let (start, end) = Self::columns(area, first, last, y);
            let mut x = area.x;
            while x < area.right() {
                let width = UnicodeWidthStr::width(canvas[(x, y)].symbol()).max(1) as u16;
                if x < end && x.saturating_add(width) > start {
                    for column in x..x.saturating_add(width).min(area.right()) {
                        target[(column, y)]
                            .set_bg(palette.accent)
                            .set_fg(palette.background);
                    }
                }
                x = x.saturating_add(width);
            }
        }
    }
}
