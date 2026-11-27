use super::*;

impl Transcript {
    pub(super) fn footer(&mut self, index: usize, width: u16, palette: Palette) -> bool {
        if self.cells[index].slice.kind != SliceKind::AssistantFooter {
            return false;
        }
        self.paint_epoch = self.paint_epoch.saturating_add(1);
        let rows = self.cells[index]
            .slice
            .footer
            .as_deref()
            .map_or_else(Vec::new, |footer| {
                crate::turn_footer::reply(footer, width, palette)
            });
        let epochs = vec![self.paint_epoch; rows.len()];
        self.cells[index].height = Some((width, rows.len()));
        self.cells[index].cache = Some(CachedOutput {
            width,
            theme: palette.light,
            rendered: rows,
            row_epochs: epochs,
            source_end: 0,
            markdown: None,
        });
        self.remember_cache(index);
        true
    }
}

#[cfg(test)]
mod tests;
