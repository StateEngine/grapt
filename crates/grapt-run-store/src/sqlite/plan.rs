use crate::command::MAX_SQL_BYTES;
use crate::error::WriteError;
use rusqlite::limits::Limit;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{params_from_iter, Connection, Row, StatementStatus};

const EXPLAIN: &str = "EXPLAIN ";
// Each slot can hold up to one SQLite length-limited value. Budget program
// sites, rather than observed callbacks, including repeated uses of a slot.
const MAX_VALUE_SLOTS: u32 = 8;
// SQLite includes the VDBE register bank in the prepared statement's memory.
const MAX_PROGRAM_BYTES: i32 = 128 * 1024;

/// Inspect the pinned SQLite bytecode with the same bindings as the write.
/// The query-plan summary can omit temporary state used by DML and triggers.
pub(crate) fn check_write_plan(
    connection: &Connection,
    sql: &str,
    params: &[Value],
) -> Result<(), WriteError> {
    let explanation = format!("{EXPLAIN}{sql}");
    let program = if explanation.len() > MAX_SQL_BYTES {
        // The prefix belongs to this probe. Restore the write limit even if
        // SQLite declines to prepare the explanation.
        let limit = Limit::SQLITE_LIMIT_SQL_LENGTH;
        let previous = connection
            .set_limit(limit, (MAX_SQL_BYTES + EXPLAIN.len()) as i32)
            .map_err(WriteError::RolledBack)?;
        let program = connection.prepare(&explanation);
        connection
            .set_limit(limit, previous)
            .map_err(WriteError::RolledBack)?;
        program
    } else {
        connection.prepare(&explanation)
    };
    let mut program = program.map_err(WriteError::RolledBack)?;
    let mut ops = program
        .query(params_from_iter(params))
        .map_err(WriteError::RolledBack)?;
    let mut slots = 0_u32;
    while let Some(row) = ops.next().map_err(WriteError::RolledBack)? {
        let ValueRef::Text(opcode) = row.get_ref(1).map_err(WriteError::RolledBack)? else {
            return Err(WriteError::RolledBack(rusqlite::Error::InvalidQuery));
        };
        if allocates_collection(opcode) {
            return Err(WriteError::TemporaryStorage);
        }
        slots = slots.saturating_add(value_slots(opcode, row).map_err(WriteError::RolledBack)?);
    }
    drop(ops);
    // Command admission already budgets bound values. Measure the program
    // and its registers after releasing the explanation’s parameter buffers.
    program.clear_bindings();
    if slots > MAX_VALUE_SLOTS || program.get_status(StatementStatus::MemUsed) > MAX_PROGRAM_BYTES {
        return Err(WriteError::HeapPressure);
    }
    Ok(())
}

fn value_slots(opcode: &[u8], row: &Row<'_>) -> rusqlite::Result<u32> {
    let p3 = || row.get::<_, u32>(4);
    Ok(match opcode {
        b"Function" | b"PureFunc" | b"Concat" | b"Cast" | b"Blob" | b"String" | b"String8"
        | b"Column" | b"RowData" | b"MakeRecord" => 1,
        // These instructions can fill a range of registers. A move can keep
        // the previous iteration's buffer live when the source is reused.
        b"Copy" => p3()?.saturating_add(1),
        b"Move" => p3()?,
        // A trigger or foreign-key action can retain multiple frame-local
        // register banks that cannot be counted once from the listing.
        b"Program" => MAX_VALUE_SLOTS + 1,
        _ => 0,
    })
}

fn allocates_collection(opcode: &[u8]) -> bool {
    matches!(
        opcode,
        b"SorterOpen"
            | b"OpenEphemeral"
            | b"OpenAutoindex"
            | b"RowSetAdd"
            | b"RowSetTest"
            | b"FilterAdd"
            | b"AggStep"
            | b"AggStep1"
            | b"VOpen"
    )
}
