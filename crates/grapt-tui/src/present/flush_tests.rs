//! Exercise the real ANSI backend without a platform console-size query.
use super::*;
use ratatui::backend::CrosstermBackend;
use std::{cell::RefCell, io, rc::Rc};

#[derive(Default)]
struct Recorded {
    bytes: Vec<u8>,
    writes: Vec<Vec<u8>>,
    fail: bool,
}
#[derive(Clone)]
struct Writer(Rc<RefCell<Recorded>>);
impl Write for Writer {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let mut record = self.0.borrow_mut();
        if record.fail {
            return Err(io::Error::other("injected output failure"));
        }
        record.bytes.extend_from_slice(bytes);
        record.writes.push(bytes.to_vec());
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.0.borrow().fail {
            Err(io::Error::other("injected output failure"))
        } else {
            Ok(())
        }
    }
}

fn frames(enabled: bool) -> Result<(Vec<Vec<u8>>, Vec<usize>), String> {
    let record = Rc::new(RefCell::new(Recorded::default()));
    let bytes = Cell::new(0);
    let batch = output::Batch::new(enabled);
    let writer = output::Output::new(io::BufWriter::new(Writer(record.clone())), &bytes, &batch);
    let mut terminal = Terminal::with_options(
        CrosstermBackend::new(writer),
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 40, 12)),
        },
    )
    .map_err(|e| e.to_string())?;
    let mut presenter = Presenter::default();
    let mut ui = Ui::new(false);
    let mut capture = profile::Capture::from_env().map_err(|e| e.to_string())?;
    let mut all = Vec::new();
    let mut counts = Vec::new();
    for (input, text) in [(false, ""), (true, "q"), (true, "k界")] {
        record.borrow_mut().bytes.clear();
        record.borrow_mut().writes.clear();
        ui.composer.insert_str(text);
        presenter.paint(&mut terminal, &mut ui, &mut capture, &bytes, &batch, input)?;
        let result = record.borrow();
        all.push(result.bytes.clone());
        counts.push(result.writes.len());
        assert!(result.bytes.starts_with(b"\x1b[?2026h"));
        assert!(result.bytes.ends_with(b"\x1b[?2026l"));
    }
    record.borrow_mut().fail = true;
    ui.composer.insert_str("failure");
    assert!(presenter
        .paint(&mut terminal, &mut ui, &mut capture, &bytes, &batch, true)
        .is_err());
    assert!(
        Write::flush(terminal.backend_mut()).is_err(),
        "an output failure left flushes deferred"
    );
    Ok((all, counts))
}

#[test]
fn input_frames_have_identical_vt_and_one_complete_buffered_write() -> Result<(), String> {
    let (reference, fragmented) = frames(false)?;
    let (batched, writes) = frames(true)?;
    assert_eq!(
        reference, batched,
        "batching changed terminal bytes or cursor commands"
    );
    assert!(
        fragmented[1..].iter().all(|count| *count > 1),
        "control did not exercise internal cursor flushes: {fragmented:?}"
    );
    assert_eq!(&writes[1..], &[1, 1], "input frame fragments: {writes:?}");
    Ok(())
}
