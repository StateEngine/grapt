use crate::command::{Command, Durability, Receipt};
use crate::error::WriteError;
use crate::key::RequestKey;
use crate::sqlite::{check_write_plan, Guard};
use rusqlite::{params, params_from_iter, Connection, OptionalExtension};
use std::sync::Arc;

type Staged = Result<(Receipt, bool), WriteError>;

pub(crate) fn run(
    connection: &mut Connection,
    restricted: &Guard,
    durability: Durability,
    commands: &[&Command],
) -> Vec<Staged> {
    let synchronous = match durability {
        Durability::Normal => "NORMAL",
        Durability::Full => "FULL",
    };
    if let Err(error) = connection.pragma_update(None, "synchronous", synchronous) {
        return fail(commands, error);
    }
    if let Err(error) = connection.execute_batch("BEGIN IMMEDIATE") {
        return fail(commands, error);
    }
    let mut results = match stage(connection, restricted, commands) {
        Ok(results) => results,
        Err(error) => {
            rollback(connection);
            return refuse_wal(commands, error);
        }
    };
    if let Err(error) = restricted.check_wal() {
        rollback(connection);
        return refuse_wal(commands, error);
    }
    #[cfg(test)]
    {
        if let Some(first) = commands.first() {
            crate::fault_tests::stall(first.key.as_str());
        }
        crate::fault_tests::cut("before-commit");
    }
    match connection.execute_batch("COMMIT") {
        Ok(()) => {
            #[cfg(test)]
            crate::fault_tests::cut("after-commit");
        }
        Err(error) => {
            let cause = Arc::new(error);
            rollback(connection);
            for (command, result) in commands.iter().zip(&mut results) {
                if result.is_ok() {
                    *result = Err(WriteError::OutcomeUnknown {
                        key: command.key.clone(),
                        cause: Some(Arc::clone(&cause)),
                    });
                }
            }
        }
    }
    results
}

fn refuse_wal(commands: &[&Command], error: WriteError) -> Vec<Staged> {
    match error {
        WriteError::RolledBack(error) => fail(commands, error),
        WriteError::WalPressure => commands
            .iter()
            .map(|_| Err(WriteError::WalPressure))
            .collect(),
        error => commands
            .iter()
            .map(|_| Err(WriteError::WalIo(std::io::Error::other(error.to_string()))))
            .collect(),
    }
}

fn stage(
    connection: &mut Connection,
    restricted: &Guard,
    commands: &[&Command],
) -> Result<Vec<Staged>, WriteError> {
    let mut results = Vec::with_capacity(commands.len());
    for command in commands {
        restricted.check_wal()?;
        connection
            .execute_batch("SAVEPOINT request")
            .map_err(WriteError::RolledBack)?;
        let result = match apply(connection, restricted, command) {
            Err(error @ (WriteError::WalPressure | WriteError::WalIo(_))) => return Err(error),
            result => result,
        };
        if result.is_err() {
            if connection.is_autocommit() {
                // A storage failure or conflict policy may have rolled back
                // the entire transaction, including its savepoints.
                return Err(WriteError::RolledBack(original_error(
                    result,
                    rusqlite::Error::InvalidQuery,
                )));
            }
            if let Err(error) = connection.execute_batch("ROLLBACK TO request; RELEASE request") {
                return Err(WriteError::RolledBack(original_error(result, error)));
            }
        } else {
            connection
                .execute_batch("RELEASE request")
                .map_err(WriteError::RolledBack)?;
        }
        results.push(result);
    }
    Ok(results)
}

fn original_error(result: Staged, fallback: rusqlite::Error) -> rusqlite::Error {
    match result {
        Err(WriteError::RolledBack(cause)) => cause,
        _ => fallback,
    }
}

fn rollback(connection: &Connection) {
    if !connection.is_autocommit() {
        // If rollback cannot end the transaction, the next BEGIN is refused.
        // The connection can never publish or commit any of this group's work.
        let _ = connection.execute_batch("ROLLBACK");
    }
}

fn fail(commands: &[&Command], error: rusqlite::Error) -> Vec<Staged> {
    let error = Arc::new(error);
    commands
        .iter()
        .map(|_| Err(WriteError::BatchAborted(Arc::clone(&error))))
        .collect()
}

fn apply(connection: &mut Connection, restricted: &Guard, command: &Command) -> Staged {
    let digest = command.digest();
    if let Some((prior, receipt)) =
        lookup(connection, &command.key).map_err(WriteError::RolledBack)?
    {
        return if prior == digest {
            Receipt::decode(&receipt)
                .map(|receipt| (receipt, false))
                .ok_or(WriteError::Receipt)
        } else {
            Err(WriteError::KeyConflict)
        };
    }
    let affected = {
        let _domain = restricted.enter();
        effects(connection, restricted, command)
    };
    if let Some(refusal) = restricted.take_refusal() {
        return Err(refusal);
    }
    let receipt = Receipt {
        affected_rows: affected?,
    };
    restricted.check_wal()?;
    connection
        .execute(
            "INSERT INTO run_store_receipt (request_key, digest, receipt) VALUES (?1, ?2, ?3)",
            params![command.key.as_str(), digest, receipt.encode()],
        )
        .map_err(WriteError::RolledBack)?;
    Ok((receipt, true))
}

fn effects(
    connection: &Connection,
    restricted: &Guard,
    command: &Command,
) -> Result<Vec<u64>, WriteError> {
    command
        .statements
        .iter()
        .map(|statement| {
            restricted.check_wal()?;
            check_write_plan(connection, &statement.sql, &statement.params)?;
            let actual = connection
                .execute(&statement.sql, params_from_iter(&statement.params))
                .map_err(WriteError::RolledBack)? as u64;
            if let Some(expected) = statement.expected {
                if actual != expected {
                    return Err(WriteError::RowCount { expected, actual });
                }
            }
            Ok(actual)
        })
        .collect()
}

pub(crate) fn lookup(
    connection: &Connection,
    key: &RequestKey,
) -> rusqlite::Result<Option<([u8; 32], Vec<u8>)>> {
    connection
        .query_row(
            "SELECT digest, receipt FROM run_store_receipt WHERE request_key=?1",
            [key.as_str()],
            |row| row.get(0).and_then(|digest| Ok((digest, row.get(1)?))),
        )
        .optional()
}
