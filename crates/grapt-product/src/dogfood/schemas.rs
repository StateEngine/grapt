//! A fresh store's owner schemas, installed in one commit.
//!
//! On a fresh profile, composition used to create each owner's tables in that
//! owner's own transaction: six commits, each a journal, directory and
//! database sync, all before the service could publish readiness. On a slow
//! or busy disk that alone outran the CLI's launch window. Here a store with
//! none of these tables gets all of them in ONE governed transaction, through
//! each owner's own `*_within` installer, so every owner still decides its
//! own schema. A crash before the commit leaves none of them, and the next
//! start simply runs this again.
//!
//! A store holding any of these tables is not fresh: it may need an owner's
//! upgrade, some of which run their own transaction. It keeps the owners'
//! separate installers, unchanged, and a settled store writes nothing.

use grapt_store::EventStore;

/// Every table an owner installer below creates on a fresh store.
const OWNED_TABLES: &[&str] = &[
    "instruction_blob",
    "instruction_entry",
    "instruction_state",
    "form_request",
    "shell_operation",
    "shell_artifact",
    "product_shell_binding",
    "session_pending",
    "session_message",
    "inbox_legacy_synthetic_audit",
    "session_operation_delivery_pending",
    "subagent_operation",
];

/// Install every owner schema in one transaction when the store has none of
/// them; otherwise do nothing and leave each owner to its own installer.
pub(super) fn install_fresh(store: &mut EventStore) -> Result<(), String> {
    let names = serde_json::to_string(OWNED_TABLES).map_err(|error| error.to_string())?;
    let present: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN (SELECT value FROM json_each(?1))",
            [names],
            |row| row.get(0),
        )
        .map_err(|error| format!("fresh schema probe failed: {error}"))?;
    if present != 0 {
        return Ok(());
    }
    let tx = store
        .governed_transaction()
        .map_err(|error| format!("fresh schema transaction refused: {error}"))?;
    grapt_instruction::InstructionService::ensure_tables_within(&tx)
        .map_err(|error| error.to_string())?;
    grapt_form::FormService::initialize_within(&tx).map_err(|error| error.to_string())?;
    grapt_shell::schema::initialize_within(&tx).map_err(|error| error.to_string())?;
    grapt_inbox::Inbox::ensure_tables(&tx).map_err(|error| error.to_string())?;
    grapt_subagent::SubagentService::ensure_schema_within(&tx)
        .map_err(|error| error.to_string())?;
    tx.commit()
        .map_err(|error| format!("fresh schema commit failed: {error}"))?
        .discard();
    Ok(())
}
