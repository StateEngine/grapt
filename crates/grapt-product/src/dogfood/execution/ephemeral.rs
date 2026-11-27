//! Fail closed on executable integrations before volatile execution planning.
use super::*;

impl LiveExecutor {
    pub(super) fn require_memory_inbox(inbox: &Mutex<Inbox>) -> Result<(), String> {
        let held = inbox.lock().map_err(|_| "ephemeral Inbox is unavailable")?;
        let connection = held.store().connection();
        let persistent: i64 = connection
            .query_row(
                "SELECT COUNT(*) FROM pragma_database_list \
                 WHERE file <> '' OR name NOT IN ('main', 'temp')",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?;
        let journal: String = connection
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if persistent != 0 || journal != "memory" {
            return Err(
                "ephemeral execution requires an in-memory Inbox without attached databases".into(),
            );
        }
        connection
            .pragma_update(None, "temp_store", 2_i64)
            .map_err(|error| error.to_string())?;
        let temporary: i64 = connection
            .pragma_query_value(None, "temp_store", |row| row.get(0))
            .map_err(|error| error.to_string())?;
        if temporary != 2 {
            return Err("ephemeral execution requires memory-only SQLite temporary storage".into());
        }
        Ok(())
    }
}

impl LiveLocation {
    pub(super) fn require_ephemeral_config(
        &self,
        session: &SessionId,
        record: &grapt_session::SessionRecord,
    ) -> Result<(), String> {
        if !self.ephemeral {
            return Ok(());
        }
        if record.parent.is_some() {
            return Err("ephemeral sessions do not support existing or child sessions".into());
        }
        {
            let inbox = self
                .inbox
                .lock()
                .map_err(|_| "ephemeral Inbox is unavailable")?;
            let entries = grapt_instruction::InstructionService::entries(inbox.store(), session)
                .map_err(|error| error.to_string())?;
            if entries.iter().any(|entry| {
                matches!(
                    entry.key.as_str(),
                    "stateengine-identity" | "stateengine-packet"
                )
            }) {
                return Err("ephemeral sessions do not support StateEngine continuity or native mail bindings".into());
            }
        }
        let directory = record.placement.directory();
        let sources = super::super::session_config::sources(
            &self.inbox,
            session,
            &self.service_root,
            Path::new(directory),
        )?;
        let mcp = super::super::connections::McpSnapshot::read(&sources, directory)?;
        if !mcp.declared.is_empty() || !mcp.blocked.is_empty() || !mcp.codemode_servers.is_empty() {
            return Err("ephemeral sessions do not support configured MCP servers".into());
        }
        let plugins = super::super::plugins::resolve_sources(&sources)?;
        if !plugins.active.is_empty()
            || !plugins.blocked.is_empty()
            || self.plugins.generation().is_some()
        {
            return Err("ephemeral sessions do not support configured plugins".into());
        }
        // The volatile loader checks declarations without constructing a command
        // environment. Retain no hook runtime; no lifecycle callback is attached.
        self.load_hooks(Path::new(directory))?;
        Ok(())
    }
}
