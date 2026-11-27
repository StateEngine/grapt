//! Exact stdout/stderr accounting with visible-output state.

use std::io::{self, Write};

use serde::Serialize;
use thiserror::Error;

use crate::{CliLimits, OutputMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputReceipt {
    pub stdout_bytes: usize,
    pub stdout_records: usize,
    pub stderr_bytes: usize,
    pub visible: bool,
}

#[derive(Debug, Error)]
pub enum OutputError {
    #[error("stdout exceeded its {maximum}-byte bound")]
    StdoutBytes { maximum: usize },
    #[error("stdout exceeded its {maximum}-record bound")]
    StdoutRecords { maximum: usize },
    #[error("stderr exceeded its {maximum}-byte bound")]
    StderrBytes { maximum: usize },
    #[error("machine-readable output encoding failed")]
    Encode,
    #[error("stdout closed before the complete record was written")]
    BrokenPipe,
    #[error("output write failed: {0:?}")]
    Io(io::ErrorKind),
}

#[derive(Debug)]
enum StdoutLifetime {
    Foreground,
    Resident,
    Disconnected,
}

#[derive(Debug)]
pub struct BoundedOutput<O, E> {
    stdout: O,
    stderr: E,
    mode: OutputMode,
    limits: CliLimits,
    receipt: OutputReceipt,
    stdout_lifetime: StdoutLifetime,
}

impl<O: Write, E: Write> BoundedOutput<O, E> {
    pub fn new(stdout: O, stderr: E, mode: OutputMode, limits: CliLimits) -> Self {
        Self {
            stdout,
            stderr,
            mode,
            limits,
            receipt: OutputReceipt {
                stdout_bytes: 0,
                stdout_records: 0,
                stderr_bytes: 0,
                visible: false,
            },
            stdout_lifetime: StdoutLifetime::Foreground,
        }
    }

    pub const fn receipt(&self) -> OutputReceipt {
        self.receipt
    }

    pub const fn mode(&self) -> OutputMode {
        self.mode
    }

    /// A resident's output reader is an observer, not its lifetime owner.
    pub(crate) fn retain_on_stdout_disconnect(&mut self, resident: bool) {
        self.stdout_lifetime = match (&self.stdout_lifetime, resident) {
            (_, false) => StdoutLifetime::Foreground,
            (StdoutLifetime::Disconnected, true) => StdoutLifetime::Disconnected,
            (_, true) => StdoutLifetime::Resident,
        };
    }

    pub fn into_parts(self) -> (O, E, OutputReceipt) {
        (self.stdout, self.stderr, self.receipt)
    }

    pub fn machine<T: Serialize>(&mut self, value: &T) -> Result<(), OutputError> {
        let mut record = serde_json::to_vec(value).map_err(|_| OutputError::Encode)?;
        if matches!(self.mode, OutputMode::Jsonl) {
            record.push(b'\n');
        }
        self.stdout_record(&record)
    }

    pub fn text(&mut self, value: &str) -> Result<(), OutputError> {
        let mut record = value.as_bytes().to_vec();
        if !record.ends_with(b"\n") {
            record.push(b'\n');
        }
        self.stdout_record(&record)
    }

    pub fn diagnostic(&mut self, value: &str) -> Result<(), OutputError> {
        let mut record = value.as_bytes().to_vec();
        if !record.ends_with(b"\n") {
            record.push(b'\n');
        }
        let maximum = self.limits.values().stderr_bytes;
        let next = self.receipt.stderr_bytes.saturating_add(record.len());
        if next > maximum {
            return Err(OutputError::StderrBytes { maximum });
        }
        write_counted(
            &mut self.stderr,
            &record,
            &mut self.receipt.stderr_bytes,
            false,
            &mut self.receipt.visible,
        )?;
        Ok(())
    }

    fn stdout_record(&mut self, record: &[u8]) -> Result<(), OutputError> {
        if matches!(self.stdout_lifetime, StdoutLifetime::Disconnected) {
            return Ok(());
        }
        let values = self.limits.values();
        let next_records = self.receipt.stdout_records.saturating_add(1);
        if next_records > values.stdout_records {
            return Err(OutputError::StdoutRecords {
                maximum: values.stdout_records,
            });
        }
        let next_bytes = self.receipt.stdout_bytes.saturating_add(record.len());
        if next_bytes > values.stdout_bytes {
            return Err(OutputError::StdoutBytes {
                maximum: values.stdout_bytes,
            });
        }
        let written = write_counted(
            &mut self.stdout,
            record,
            &mut self.receipt.stdout_bytes,
            true,
            &mut self.receipt.visible,
        );
        if matches!(written, Err(OutputError::BrokenPipe))
            && matches!(self.stdout_lifetime, StdoutLifetime::Resident)
        {
            // Never retry a partially written JSONL record or redirect it to
            // stderr: its reader may have gone away with the same launcher.
            // Durable Session reads remain available through the service.
            self.stdout_lifetime = StdoutLifetime::Disconnected;
            return Ok(());
        }
        written?;
        self.receipt.stdout_records = next_records;
        Ok(())
    }
}

/// A write blocks for as long as its reader stops draining. On a multi-thread
/// runtime it runs in place of the worker, which hands the worker's queue, timers
/// and I/O to another thread, so the runtime keeps serving every other task,
/// cancellation included.
fn in_place_of_worker(write: impl FnOnce() -> io::Result<usize>) -> io::Result<usize> {
    match tokio::runtime::Handle::try_current() {
        Ok(runtime) if runtime.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread => {
            tokio::task::block_in_place(write)
        }
        _ => write(),
    }
}

fn write_counted(
    writer: &mut impl Write,
    bytes: &[u8],
    count: &mut usize,
    stdout: bool,
    visible: &mut bool,
) -> Result<(), OutputError> {
    let mut written = 0_usize;
    while written < bytes.len() {
        match in_place_of_worker(|| writer.write(&bytes[written..])) {
            Ok(0) => return Err(OutputError::Io(io::ErrorKind::WriteZero)),
            Ok(amount) => {
                written = written.saturating_add(amount);
                *count = count.saturating_add(amount);
                if stdout {
                    *visible = true;
                }
            }
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
                return Err(OutputError::BrokenPipe)
            }
            Err(error) => return Err(OutputError::Io(error.kind())),
        }
    }
    Ok(())
}
