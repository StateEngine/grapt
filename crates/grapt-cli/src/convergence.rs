//! Bounded durable-page convergence. Live events only trigger another relist.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::CliLimits;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CursorToken(String);

impl CursorToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, ConvergenceError> {
        let value = value.into();
        if value.is_empty() || value.len() > 1_024 || value.chars().any(char::is_whitespace) {
            return Err(ConvergenceError::InvalidCursor);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorRecord {
    cursor: CursorToken,
    sequence: u64,
    canonical: Vec<u8>,
}

impl CursorRecord {
    pub fn try_new(
        cursor: impl Into<String>,
        sequence: u64,
        canonical: Vec<u8>,
    ) -> Result<Self, ConvergenceError> {
        if canonical.is_empty() || canonical.len() > 262_144 {
            return Err(ConvergenceError::InvalidRecord);
        }
        Ok(Self {
            cursor: CursorToken::try_new(cursor)?,
            sequence,
            canonical,
        })
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn cursor(&self) -> &CursorToken {
        &self.cursor
    }

    pub fn canonical(&self) -> &[u8] {
        &self.canonical
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorPage {
    records: Vec<CursorRecord>,
    next: Option<CursorToken>,
}

impl CursorPage {
    pub fn try_new(
        records: Vec<CursorRecord>,
        next: Option<String>,
        limits: CliLimits,
    ) -> Result<Self, ConvergenceError> {
        let maximum = limits.values().retained_cursors;
        if records.len() > maximum {
            return Err(ConvergenceError::PageTooLarge {
                maximum,
                found: records.len(),
            });
        }
        Ok(Self {
            records,
            next: next.map(CursorToken::try_new).transpose()?,
        })
    }

    pub fn records(&self) -> &[CursorRecord] {
        &self.records
    }

    pub fn next(&self) -> Option<&CursorToken> {
        self.next.as_ref()
    }
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ConvergenceError {
    #[error("cursor is empty, oversized, or contains whitespace")]
    InvalidCursor,
    #[error("cursor record is empty or oversized")]
    InvalidRecord,
    #[error("page has {found} records; maximum is {maximum}")]
    PageTooLarge { maximum: usize, found: usize },
    #[error("sequence gap: expected {expected}, found {found}")]
    Gap { expected: u64, found: u64 },
    #[error("sequence moved backwards from {settled} to {found}")]
    OutOfOrder { settled: u64, found: u64 },
    #[error("sequence {sequence} was repeated with different canonical bytes")]
    ConflictingDuplicate { sequence: u64 },
    #[error("cursor token was reused for a different sequence")]
    CursorReuse,
    #[error("retained cursor set reached its finite {maximum}-entry bound")]
    RetainedBound { maximum: usize },
}

#[derive(Debug)]
pub struct CursorConvergence {
    limits: CliLimits,
    settled: Option<u64>,
    by_sequence: BTreeMap<u64, CursorRecord>,
    by_cursor: BTreeMap<CursorToken, u64>,
}

impl CursorConvergence {
    pub fn new(limits: CliLimits) -> Self {
        Self {
            limits,
            settled: None,
            by_sequence: BTreeMap::new(),
            by_cursor: BTreeMap::new(),
        }
    }

    pub const fn settled(&self) -> Option<u64> {
        self.settled
    }

    pub fn admit_page(&mut self, page: CursorPage) -> Result<Vec<CursorRecord>, ConvergenceError> {
        self.admit(page, true)
    }

    /// Live notifications project only selected aggregate facts. Their cursors
    /// remain ordered and conflict checked, but cannot prove contiguous replay.
    pub fn admit_advisory(
        &mut self,
        page: CursorPage,
    ) -> Result<Vec<CursorRecord>, ConvergenceError> {
        self.admit(page, false)
    }

    fn admit(
        &mut self,
        page: CursorPage,
        contiguous: bool,
    ) -> Result<Vec<CursorRecord>, ConvergenceError> {
        let mut admitted = Vec::with_capacity(page.records.len());
        for record in page.records {
            if let Some(existing_sequence) = self.by_cursor.get(record.cursor()) {
                if *existing_sequence != record.sequence {
                    return Err(ConvergenceError::CursorReuse);
                }
            }
            if let Some(existing) = self.by_sequence.get(&record.sequence) {
                if existing != &record {
                    return Err(ConvergenceError::ConflictingDuplicate {
                        sequence: record.sequence,
                    });
                }
                continue;
            }
            if let Some(settled) = self.settled {
                // Live delivery is advisory: concurrent durable writers can
                // announce an earlier sequence after a later one. That older
                // advisory is already covered by the durable relist it triggers,
                // so it is skipped rather than failing the run. Durable pages
                // stay strictly ordered.
                if record.sequence <= settled && !contiguous {
                    continue;
                }
                if record.sequence <= settled {
                    return Err(ConvergenceError::OutOfOrder {
                        settled,
                        found: record.sequence,
                    });
                }
                let expected = settled.saturating_add(1);
                if contiguous && record.sequence != expected {
                    return Err(ConvergenceError::Gap {
                        expected,
                        found: record.sequence,
                    });
                }
            }
            let maximum = self.limits.values().retained_cursors;
            if self.by_sequence.len() >= maximum {
                return Err(ConvergenceError::RetainedBound { maximum });
            }
            self.by_cursor
                .insert(record.cursor.clone(), record.sequence);
            self.by_sequence.insert(record.sequence, record.clone());
            self.settled = Some(record.sequence);
            admitted.push(record);
        }
        Ok(admitted)
    }
}
