//! Optional bounded frame observations; no terminal text or session data is recorded.
use std::{
    fs::{File, OpenOptions},
    io::{self, Write},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
};

static ENABLED: AtomicBool = AtomicBool::new(false);
static EVENTS: AtomicU64 = AtomicU64::new(0);
static LAYOUTS: AtomicU64 = AtomicU64::new(0);
static HEIGHTS: AtomicU64 = AtomicU64::new(0);
static PUMPS: AtomicU64 = AtomicU64::new(0);
static UPDATES: AtomicU64 = AtomicU64::new(0);
static ITERATIONS: AtomicU64 = AtomicU64::new(0);
static PARSED_BYTES: AtomicU64 = AtomicU64::new(0);
static HEIGHT_BYTES: AtomicU64 = AtomicU64::new(0);
const MAX_FRAMES: u64 = 100_000;

pub fn decoded(count: usize) {
    if ENABLED.load(Ordering::Relaxed) {
        EVENTS.fetch_add(count as u64, Ordering::Relaxed);
        PUMPS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn layout(height_only: bool) {
    if ENABLED.load(Ordering::Relaxed) {
        let counter = if height_only { &HEIGHTS } else { &LAYOUTS };
        counter.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn update() {
    if ENABLED.load(Ordering::Relaxed) {
        UPDATES.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn iteration() {
    if ENABLED.load(Ordering::Relaxed) {
        ITERATIONS.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn markdown(bytes: usize, painted: bool) {
    if ENABLED.load(Ordering::Relaxed) {
        let counter = if painted {
            &PARSED_BYTES
        } else {
            &HEIGHT_BYTES
        };
        counter.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

pub struct Capture {
    file: Option<File>,
    frames: u64,
    pointer: Option<tokio::time::Instant>,
}

impl std::fmt::Debug for Capture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Capture").finish_non_exhaustive()
    }
}

impl Capture {
    pub fn from_env() -> io::Result<Self> {
        let file = std::env::var_os("GRAPT_TUI_R_PROFILE")
            .map(|path| OpenOptions::new().write(true).create_new(true).open(path))
            .transpose()?;
        ENABLED.store(file.is_some(), Ordering::Relaxed);
        Ok(Self {
            file,
            frames: 0,
            pointer: None,
        })
    }

    pub fn begin(&self) -> Option<tokio::time::Instant> {
        self.file.as_ref().map(|_| tokio::time::Instant::now())
    }

    /// Keep the earliest coalesced drag until its terminal flush completes.
    pub fn pointer(&mut self) {
        if self.pointer.is_none() {
            self.pointer = self.begin();
        }
    }

    pub fn record(&mut self, started: Option<tokio::time::Instant>, bytes: u64) -> io::Result<()> {
        let (Some(file), Some(started)) = (self.file.as_mut(), started) else {
            return Ok(());
        };
        let elapsed = started.elapsed().as_micros();
        let pointer = self.pointer.take().map_or_else(
            || "null".to_owned(),
            |at| at.elapsed().as_micros().to_string(),
        );
        self.frames += 1;
        writeln!(file,
            "{{\"frame\":{},\"events_decoded\":{},\"cells_laid_out\":{},\"heights_measured\":{},\"bytes_written\":{},\"frame_us\":{elapsed},\"pointer_to_flush_us\":{pointer},\"protocol_pumps\":{},\"protocol_updates\":{},\"ui_iterations\":{},\"markdown_source_bytes\":{},\"height_source_bytes\":{}}}",
            self.frames, EVENTS.swap(0, Ordering::Relaxed), LAYOUTS.swap(0, Ordering::Relaxed),
            HEIGHTS.swap(0, Ordering::Relaxed), bytes, PUMPS.swap(0, Ordering::Relaxed),
            UPDATES.swap(0, Ordering::Relaxed), ITERATIONS.swap(0, Ordering::Relaxed),
            PARSED_BYTES.swap(0, Ordering::Relaxed), HEIGHT_BYTES.swap(0, Ordering::Relaxed))?;
        if self.frames == MAX_FRAMES {
            file.flush()?;
            self.file = None;
            ENABLED.store(false, Ordering::Relaxed);
        }
        Ok(())
    }
}
