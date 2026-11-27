// Copyright 2025 OpenAI
// Licensed under the Apache License, Version 2.0.
// Modified by Grapt: bounded native wake custody and immutable run bindings.
//! Followup wake custody and immutable bindings for successive child runs.
//! Terminal operations remain immutable; a followup creates a distinct run.

use super::*;
use grapt_types::ExecutionId;

mod chain;
mod claim;
mod intent;
mod queue;
pub(super) use queue::highest_generation;

#[derive(Debug)]
pub(super) struct Chain {
    pub root: SubagentOperationId,
    pub current: ChildOperationContext,
}

pub(super) fn read_chain(
    conn: &rusqlite::Connection,
    child: &SessionId,
) -> Result<Option<Chain>, SubagentError> {
    chain::read(conn, child)
}

pub(super) const MAX_RUNS: usize = 1024;
pub(super) const MAX_PENDING: i64 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FollowupCancelReason {
    Interrupted,
    ProcessLoss,
    Unavailable,
}

impl FollowupCancelReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Interrupted => "interrupted",
            Self::ProcessLoss => "process-loss",
            Self::Unavailable => "unavailable",
        }
    }

    fn decode(raw: &str) -> Result<Self, SubagentError> {
        match raw {
            "interrupted" => Ok(Self::Interrupted),
            "process-loss" => Ok(Self::ProcessLoss),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(refused("unknown followup cancellation reason")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowupState {
    Queued,
    Started(SubagentOperationId),
    Consumed,
    Cancelled(FollowupCancelReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowupReceipt {
    pub message: MessageId,
    pub seq: u64,
    pub state: FollowupState,
    /// Only the new admission transfers a wake owner. Retries reconcile it.
    pub admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FollowupRun {
    pub operation: SubagentOperationId,
    pub child: SessionId,
    pub execution: ExecutionId,
    pub input: MessageId,
    pub admitted_seq: u64,
}

#[derive(Debug)]
pub struct FollowupStart<'a> {
    pub message: &'a MessageId,
    pub generation: RuntimeGeneration,
    pub admission: &'a SubagentAdmission,
    pub execution: &'a ExecutionId,
    pub input: &'a MessageId,
    pub input_event: &'a EventId,
    pub created_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FollowupDecision {
    Waiting,
    Consumed,
    Cancelled(FollowupCancelReason),
    AlreadyStarted,
    Started {
        run: FollowupRun,
        prompt: String,
        new_input: bool,
    },
}

fn refused(reason: impl Into<String>) -> SubagentError {
    SubagentError::Followup {
        reason: reason.into(),
    }
}

pub(super) fn installed(conn: &rusqlite::Connection) -> Result<bool, SubagentError> {
    let tables: i64 = conn.query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name IN ('subagent_followup','subagent_run')",
        [],
        |row| row.get(0),
    )?;
    match tables {
        0 => Ok(false),
        2 => Ok(true),
        _ => Err(refused("followup schema is incomplete")),
    }
}

pub(super) const CREATE_FOLLOWUP_TABLES: &str = "CREATE TABLE IF NOT EXISTS subagent_followup (
          message_id TEXT PRIMARY KEY NOT NULL,
          child_session_id TEXT NOT NULL,
          sender_session_id TEXT NOT NULL,
          call_id TEXT NOT NULL,
          admitted_seq INTEGER NOT NULL CHECK(admitted_seq>=0),
          owner_generation INTEGER NOT NULL CHECK(owner_generation>0),
          state TEXT NOT NULL CHECK(state IN ('queued','started','consumed','cancelled')),
          operation_id TEXT UNIQUE REFERENCES subagent_operation(operation_id),
          reason TEXT CHECK(reason IN ('interrupted','process-loss','unavailable')),
          time_created INTEGER NOT NULL CHECK(time_created>=0),
          time_updated INTEGER NOT NULL CHECK(time_updated>=0),
          CHECK((state='started')=(operation_id IS NOT NULL)),
          CHECK((state='cancelled')=(reason IS NOT NULL)),
          CHECK(length(CAST(message_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(child_session_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(sender_session_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(call_id AS BLOB)) BETWEEN 1 AND 512)
        );
        CREATE TABLE IF NOT EXISTS subagent_run (
          operation_id TEXT PRIMARY KEY NOT NULL REFERENCES subagent_operation(operation_id),
          predecessor_id TEXT NOT NULL UNIQUE REFERENCES subagent_operation(operation_id),
          child_session_id TEXT NOT NULL,
          execution_id TEXT NOT NULL UNIQUE,
          input_id TEXT NOT NULL,
          admitted_seq INTEGER NOT NULL CHECK(admitted_seq>=0),
          cause_message_id TEXT NOT NULL UNIQUE REFERENCES subagent_followup(message_id),
          CHECK(operation_id<>predecessor_id),
          CHECK(length(CAST(operation_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(predecessor_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(child_session_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(execution_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(input_id AS BLOB)) BETWEEN 1 AND 512),
          CHECK(length(CAST(cause_message_id AS BLOB)) BETWEEN 1 AND 512)
        );";
