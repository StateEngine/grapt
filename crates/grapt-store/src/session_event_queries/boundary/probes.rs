//! Closed structural probe vocabulary and its table-owned statements.
#[derive(Debug, Clone, Copy)]
pub enum Probe {
    Tool,
    Permission,
    ToolIdentity,
    ToolTerminal,
    ContextItem,
    Promoted,
    AgentMail,
    Mail,
    Event,
    EventBytes,
}

impl Probe {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Tool => "tool",
            Self::Permission => "permission",
            Self::ToolIdentity => "tool identity",
            Self::ToolTerminal => "tool terminal",
            Self::ContextItem => "context item",
            Self::Promoted => "promoted",
            Self::AgentMail => "agent mail",
            Self::Mail => "mail",
            Self::Event => "event",
            Self::EventBytes => "event data",
        }
    }

    pub(super) fn sql(self) -> &'static str {
        match self {
            Self::Tool => TOOL,
            Self::Permission => PERMISSION,
            Self::ToolIdentity => TOOL_IDENTITY,
            Self::ToolTerminal => TOOL_TERMINAL,
            Self::ContextItem => CONTEXT_ITEM,
            Self::Promoted => PROMOTED,
            Self::AgentMail => AGENT_MAIL,
            Self::Mail => MAIL,
            Self::Event => EVENT,
            Self::EventBytes => EVENT_BYTES,
        }
    }
}

const TOOL: &str = "SELECT (SELECT seq FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND (seq >= ?2 OR seq < 0)
                   AND type = 'session.content.appended.1'
                   AND CASE
                         WHEN json_valid(data) = 0 THEN 1
                         WHEN json_type(data, '$.item.kind') IS NULL THEN 1
                         WHEN json_type(data, '$.item.kind') != 'text' THEN 1
                         WHEN json_extract(data, '$.item.kind') IN ('text', 'reasoning', 'opaque')
                           THEN 0
                         ELSE 1
                       END = 1
                 ORDER BY seq LIMIT 1 OFFSET ?3)";

const PERMISSION: &str = "SELECT (SELECT seq FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND (seq >= ?2 OR seq < 0) AND type = 'permission.asked.2'
                 ORDER BY seq LIMIT 1 OFFSET ?3)";

const TOOL_IDENTITY: &str = "SELECT (SELECT seq FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND (seq >= ?2 OR seq < 0) AND type IN ('session.tool.input.started.1', 'session.tool.input.started.2')
                 ORDER BY seq LIMIT 1 OFFSET ?3)";

const TOOL_TERMINAL: &str = "SELECT (SELECT e.seq FROM event e
                 WHERE e.aggregate_family = 'session' AND e.aggregate_id = ?1
                   AND (e.seq >= ?2 OR e.seq < 0) AND e.type = 'session.tool.terminal.1'
                   AND NOT EXISTS (SELECT 1 FROM event n
                     WHERE n.aggregate_family = 'session' AND n.aggregate_id = e.aggregate_id
                       AND n.type = 'session.tool.nested.started.1' AND json_valid(n.data)
                       AND json_valid(e.data) AND json_extract(n.data, '$.attempt') = json_extract(e.data, '$.attempt')
                       AND json_extract(n.data, '$.call') = json_extract(e.data, '$.call'))
                 ORDER BY e.seq LIMIT 1 OFFSET ?3)";

const CONTEXT_ITEM: &str = "SELECT (SELECT seq FROM (
                    SELECT seq FROM session_message
                     WHERE session_id = ?1 AND type IN ('user', 'synthetic') AND (seq >= ?2 OR seq < 0)
                    UNION ALL
                    SELECT seq FROM event
                     WHERE aggregate_family = 'session' AND aggregate_id = ?1
                       AND (seq >= ?2 OR seq < 0) AND type = 'session.content.appended.1'
                       AND CASE WHEN json_valid(data) = 0 THEN 1
                            WHEN json_extract(data, '$.item.kind') IN ('text', 'opaque', 'tool') THEN 1
                            ELSE 0 END = 1
                    UNION ALL
                    -- One request-only boundary for each retained response with
                    -- emitted content. Count it where its first item appears,
                    -- even when the response opening precedes the retained cut.
                    SELECT min(seq) FROM event
                     WHERE aggregate_family = 'session' AND aggregate_id = ?1
                       AND (seq >= ?2 OR seq < 0) AND type = 'session.content.appended.1'
                       AND json_valid(data)
                       AND json_extract(data, '$.item.kind') IN ('text', 'opaque', 'tool')
                     GROUP BY json_extract(data, '$.message')
                    UNION ALL
                    SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
                     AND (seq>=?2 OR seq<0) AND type LIKE 'session.subagent-mail.presented.%'
                    UNION ALL
                    SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
                     AND (seq>=?2 OR seq<0) AND type LIKE 'session.mail.presented.%'
                    UNION ALL
                    SELECT e.seq FROM event e
                     WHERE e.aggregate_family = 'session' AND e.aggregate_id = ?1
                       AND (e.seq >= ?2 OR e.seq < 0) AND e.type = 'session.tool.terminal.1'
                       AND NOT EXISTS (SELECT 1 FROM event n
                         WHERE n.aggregate_family = 'session' AND n.aggregate_id = e.aggregate_id
                           AND n.type = 'session.tool.nested.started.1' AND json_valid(n.data)
                           AND json_valid(e.data) AND json_extract(n.data, '$.attempt') = json_extract(e.data, '$.attempt')
                           AND json_extract(n.data, '$.call') = json_extract(e.data, '$.call'))
                 ) ORDER BY seq LIMIT 1 OFFSET ?3)";

const PROMOTED: &str = "SELECT (SELECT seq FROM session_message
                 WHERE session_id = ?1 AND type IN ('user', 'synthetic') AND (seq >= ?2 OR seq < 0)
                 ORDER BY seq LIMIT 1 OFFSET ?3)";

const AGENT_MAIL: &str =
    "SELECT (SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
             AND (seq>=?2 OR seq<0) AND type LIKE 'session.subagent-mail.presented.%'
             ORDER BY seq LIMIT 1 OFFSET ?3)";

const MAIL: &str =
    "SELECT (SELECT seq FROM event WHERE aggregate_family='session' AND aggregate_id=?1
             AND (seq>=?2 OR seq<0) AND type LIKE 'session.mail.presented.%'
             ORDER BY seq LIMIT 1 OFFSET ?3)";

const EVENT: &str = "SELECT (SELECT seq FROM event
                 WHERE aggregate_family = 'session' AND aggregate_id = ?1
                   AND (seq >= ?2 OR seq < 0)
                 ORDER BY seq LIMIT 1 OFFSET ?3)";

const EVENT_BYTES: &str = "SELECT (SELECT seq FROM (
                    SELECT seq, SUM(length(CAST(data AS BLOB))) OVER (ORDER BY seq) AS total
                      FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
                       AND (seq >= ?2 OR seq < 0))
                 WHERE total > ?3 ORDER BY seq LIMIT 1)";
