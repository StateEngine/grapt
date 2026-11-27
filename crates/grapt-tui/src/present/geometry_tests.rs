//! A console size query is synchronous on Windows; steady paints must avoid it.
use super::*;
use ratatui::{
    backend::{ClearType, TestBackend, WindowSize},
    layout::{Position, Size},
};
use std::convert::Infallible;

struct Counted {
    inner: TestBackend,
    sizes: Cell<usize>,
    clears: usize,
}
impl Backend for Counted {
    type Error = Infallible;
    fn draw<'a, I>(&mut self, content: I) -> Result<(), Infallible>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }
    fn hide_cursor(&mut self) -> Result<(), Infallible> {
        self.inner.hide_cursor()
    }
    fn show_cursor(&mut self) -> Result<(), Infallible> {
        self.inner.show_cursor()
    }
    fn get_cursor_position(&mut self) -> Result<Position, Infallible> {
        self.inner.get_cursor_position()
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, at: P) -> Result<(), Infallible> {
        self.inner.set_cursor_position(at)
    }
    fn clear(&mut self) -> Result<(), Infallible> {
        self.clears += 1;
        self.inner.clear()
    }
    fn clear_region(&mut self, kind: ClearType) -> Result<(), Infallible> {
        self.inner.clear_region(kind)
    }
    fn size(&self) -> Result<Size, Infallible> {
        self.sizes.set(self.sizes.get() + 1);
        self.inner.size()
    }
    fn window_size(&mut self) -> Result<WindowSize, Infallible> {
        self.inner.window_size()
    }
    fn flush(&mut self) -> Result<(), Infallible> {
        self.inner.flush()
    }
}

fn scene() -> Ui {
    let mut ui = Ui::new(false);
    ui.composer.insert_str("draft 中👩🏽‍💻\nsecond line");
    ui.transcript.update(
        true,
        0,
        vec![grapt_client::Slice {
            compaction: None,
            kind: grapt_client::SliceKind::User,
            tool_id: None,
            tool_phase: None,
            ordinal: 0,
            text: "Existing conversation é and 界".into(),
            read_detail_start: None,
            tool_summary: None,
            marks: Vec::new(),
            closed: true,
            footer: None,
            truncated_bytes: 0,
        }],
    );
    ui
}

#[test]
fn paints_use_attached_geometry_and_resize_events_preserve_full_cells_and_cursor(
) -> Result<(), String> {
    let mut fixed = terminal(Counted {
        inner: TestBackend::new(120, 40),
        sizes: Cell::new(0),
        clears: 0,
    })
    .map_err(|error| error.to_string())?;
    assert_eq!(fixed.backend().sizes.get(), 1, "attachment size query");
    let mut ui = scene();
    let mut presenter = Presenter::default();
    for (width, height) in [(120, 40), (80, 24), (12, 2), (140, 45), (120, 40)] {
        fixed.backend_mut().inner.resize(width, height);
        fixed
            .resize(Rect::new(0, 0, width, height))
            .map_err(|error| error.to_string())?;
        let mut reference =
            Terminal::new(TestBackend::new(width, height)).map_err(|error| error.to_string())?;
        let mut fresh = scene();
        let size_reads = fixed.backend().sizes.get();
        for _ in 0..16 {
            fixed
                .draw(|frame| presenter.render(&mut ui, frame, false))
                .map_err(|error| error.to_string())?;
            reference
                .draw(|frame| fresh.draw(frame))
                .map_err(|error| error.to_string())?;
            assert_eq!(fixed.backend().inner.buffer(), reference.backend().buffer());
            assert_eq!(
                fixed.backend_mut().get_cursor_position(),
                reference.backend_mut().get_cursor_position()
            );
        }
        assert_eq!(
            fixed.backend().sizes.get(),
            size_reads,
            "steady paint queried geometry after the {width}×{height} resize"
        );
    }
    Ok(())
}

#[test]
fn terminal_attachment_clears_foreign_console_contents_before_the_first_diff(
) -> Result<(), Infallible> {
    let terminal = terminal(Counted {
        inner: TestBackend::new(80, 24),
        sizes: Cell::new(0),
        clears: 0,
    })?;
    assert_eq!(terminal.backend().clears, 1);
    Ok(())
}
