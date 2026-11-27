//! Closed resource bounds for every CLI ingress and retained surface.

use std::num::{NonZeroU32, NonZeroUsize};

use thiserror::Error;

pub const MAX_STDIN_BYTES: usize = 1_048_576;
pub const MAX_STDIN_RECORD_BYTES: usize = 262_144;
pub const MAX_STDOUT_BYTES: usize = 16_777_216;
pub const MAX_STDOUT_RECORDS: usize = 65_536;
pub const MAX_STDERR_BYTES: usize = 1_048_576;
pub const MAX_SSE_FRAME_BYTES: usize = grapt_api::MAX_SSE_FRAME_BYTES;
pub const MAX_RETAINED_CURSORS: usize = 4_096;
pub const MAX_PRE_VISIBLE_RETRIES: u32 = 3;
/// The Session store's title rule (`grapt-session` title: a nonempty printable
/// line of at most 100 characters), checked at the CLI ingress.
pub const MAX_TITLE_CHARACTERS: usize = 100;
pub const MAX_ACP_INFLIGHT: usize = 64;
pub const MAX_CHILD_OUTPUT_BYTES: usize = 1_048_576;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliLimitValues {
    pub stdin_bytes: usize,
    pub stdin_record_bytes: usize,
    pub stdout_bytes: usize,
    pub stdout_records: usize,
    pub stderr_bytes: usize,
    pub sse_frame_bytes: usize,
    pub retained_cursors: usize,
    pub pre_visible_retries: u32,
    pub acp_inflight: usize,
    pub child_output_bytes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CliLimits {
    stdin_bytes: NonZeroUsize,
    stdin_record_bytes: NonZeroUsize,
    stdout_bytes: NonZeroUsize,
    stdout_records: NonZeroUsize,
    stderr_bytes: NonZeroUsize,
    sse_frame_bytes: NonZeroUsize,
    retained_cursors: NonZeroUsize,
    pre_visible_retries: NonZeroU32,
    acp_inflight: NonZeroUsize,
    child_output_bytes: NonZeroUsize,
}

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum CliLimitsError {
    #[error("CLI bound {bound} must be positive")]
    Zero { bound: &'static str },
    #[error("CLI bound {bound} exceeds its hard maximum of {maximum}")]
    TooLarge { bound: &'static str, maximum: usize },
}

impl CliLimits {
    pub fn production() -> Self {
        Self {
            stdin_bytes: nonzero(MAX_STDIN_BYTES),
            stdin_record_bytes: nonzero(MAX_STDIN_RECORD_BYTES),
            stdout_bytes: nonzero(MAX_STDOUT_BYTES),
            stdout_records: nonzero(MAX_STDOUT_RECORDS),
            stderr_bytes: nonzero(MAX_STDERR_BYTES),
            sse_frame_bytes: nonzero(MAX_SSE_FRAME_BYTES),
            retained_cursors: nonzero(MAX_RETAINED_CURSORS),
            pre_visible_retries: nonzero_u32(MAX_PRE_VISIBLE_RETRIES),
            acp_inflight: nonzero(MAX_ACP_INFLIGHT),
            child_output_bytes: nonzero(MAX_CHILD_OUTPUT_BYTES),
        }
    }

    pub fn try_new(values: CliLimitValues) -> Result<Self, CliLimitsError> {
        Ok(Self {
            stdin_bytes: checked("stdin_bytes", values.stdin_bytes, MAX_STDIN_BYTES)?,
            stdin_record_bytes: checked(
                "stdin_record_bytes",
                values.stdin_record_bytes,
                MAX_STDIN_RECORD_BYTES,
            )?,
            stdout_bytes: checked("stdout_bytes", values.stdout_bytes, MAX_STDOUT_BYTES)?,
            stdout_records: checked("stdout_records", values.stdout_records, MAX_STDOUT_RECORDS)?,
            stderr_bytes: checked("stderr_bytes", values.stderr_bytes, MAX_STDERR_BYTES)?,
            sse_frame_bytes: checked(
                "sse_frame_bytes",
                values.sse_frame_bytes,
                MAX_SSE_FRAME_BYTES,
            )?,
            retained_cursors: checked(
                "retained_cursors",
                values.retained_cursors,
                MAX_RETAINED_CURSORS,
            )?,
            pre_visible_retries: checked_u32(
                "pre_visible_retries",
                values.pre_visible_retries,
                MAX_PRE_VISIBLE_RETRIES,
            )?,
            acp_inflight: checked("acp_inflight", values.acp_inflight, MAX_ACP_INFLIGHT)?,
            child_output_bytes: checked(
                "child_output_bytes",
                values.child_output_bytes,
                MAX_CHILD_OUTPUT_BYTES,
            )?,
        })
    }

    pub const fn values(self) -> CliLimitValues {
        CliLimitValues {
            stdin_bytes: self.stdin_bytes.get(),
            stdin_record_bytes: self.stdin_record_bytes.get(),
            stdout_bytes: self.stdout_bytes.get(),
            stdout_records: self.stdout_records.get(),
            stderr_bytes: self.stderr_bytes.get(),
            sse_frame_bytes: self.sse_frame_bytes.get(),
            retained_cursors: self.retained_cursors.get(),
            pre_visible_retries: self.pre_visible_retries.get(),
            acp_inflight: self.acp_inflight.get(),
            child_output_bytes: self.child_output_bytes.get(),
        }
    }
}

fn checked(
    bound: &'static str,
    value: usize,
    maximum: usize,
) -> Result<NonZeroUsize, CliLimitsError> {
    let value = NonZeroUsize::new(value).ok_or(CliLimitsError::Zero { bound })?;
    if value.get() > maximum {
        return Err(CliLimitsError::TooLarge { bound, maximum });
    }
    Ok(value)
}

fn checked_u32(
    bound: &'static str,
    value: u32,
    maximum: u32,
) -> Result<NonZeroU32, CliLimitsError> {
    let value = NonZeroU32::new(value).ok_or(CliLimitsError::Zero { bound })?;
    if value.get() > maximum {
        return Err(CliLimitsError::TooLarge {
            bound,
            maximum: maximum as usize,
        });
    }
    Ok(value)
}

fn nonzero(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => unreachable!("nonzero production constant"),
    }
}

fn nonzero_u32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => unreachable!("nonzero production constant"),
    }
}
