//! Keep ANSI frame fragments together through Ratatui's internal cursor flushes.
use std::{
    cell::Cell,
    io::{self, Write},
};

pub struct Batch {
    enabled: bool,
    active: Cell<bool>,
}

impl Batch {
    pub fn terminal() -> Self {
        // Legacy console commands require flushing before their Win32 effects.
        #[cfg(windows)]
        let enabled = crossterm::ansi_support::supports_ansi();
        #[cfg(not(windows))]
        let enabled = true;
        Self::new(enabled)
    }

    pub fn new(enabled: bool) -> Self {
        Self {
            enabled,
            active: Cell::new(false),
        }
    }

    pub fn begin(&self) -> Frame<'_> {
        self.active.set(self.enabled);
        Frame(self)
    }
}

pub struct Frame<'a>(&'a Batch);
impl Drop for Frame<'_> {
    fn drop(&mut self) {
        self.0.active.set(false);
    }
}

pub struct Output<'a, W> {
    inner: W,
    bytes: &'a Cell<u64>,
    batch: &'a Batch,
}

impl<'a, W> Output<'a, W> {
    pub fn new(inner: W, bytes: &'a Cell<u64>, batch: &'a Batch) -> Self {
        Self {
            inner,
            bytes,
            batch,
        }
    }
}

impl<W: Write> Write for Output<'_, W> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buffer)?;
        self.bytes
            .set(self.bytes.get().saturating_add(written as u64));
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.batch.active.get() {
            Ok(())
        } else {
            self.inner.flush()
        }
    }
}
