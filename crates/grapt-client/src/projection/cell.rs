//! Transcript cell data shared by the projection and renderers.

use super::{Mark, ToolSummary};

/// The content or lifecycle row presented by a transcript slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SliceKind {
    AssistantFooter,
    Text,
    Reasoning,
    /// The product's own account of what happened — including why an execution
    /// failed. It is not model output and must not be dressed as if it were.
    System,
    /// A durable row this client decoded but cannot present yet.
    ///
    /// It carries the union tag rather than content, so the transcript can say
    /// what is there instead of leaving a silent hole. A reader must be able to
    /// tell "the client will not draw this" from "there was nothing here".
    Unsupported,
    /// One tool call: its identity, what it was asked to do, and how it ended.
    ///
    /// The live contract carries no tool member, so a tool reaches the
    /// transcript only through the bounded re-list.
    Tool,
    /// A user message from the durable re-list.
    ///
    /// The live stream has no user-message member, so this kind only ever
    /// arrives from durable truth - which is exactly why the transcript needs
    /// the re-list rather than the stream alone.
    User,
    /// A wake or queued system input, with its durable synthetic provenance.
    Automated,
    /// A prepared-seat --prompt, explicitly labeled on admission.
    SeatStartup,
}

/// The tool lifecycle decoded by the protocol client.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPhase {
    Streaming,
    Running,
    Completed,
    Error,
}

/// One opened content block and whatever has accumulated into it.
#[derive(Debug, Clone, PartialEq)]
pub struct Slice {
    /// Typed compaction identity and lifecycle; full detail shares the slice byte bound.
    pub compaction: Option<Box<crate::wire::Compaction>>,
    pub footer: Option<Box<crate::wire::AssistantFooter>>,
    pub kind: SliceKind,
    /// Which tool call this slice presents, when it presents one.
    ///
    /// Kept TYPED rather than as text because it is compared, not printed: a
    /// re-list that repeats a page must UPDATE the call it already shows
    /// instead of appending a second row for the same invocation, and that
    /// deduplication is an identity comparison.
    pub tool_id: Option<crate::wire::ToolCallWireId>,
    /// Decoded lifecycle, so presentation never parses its own status text.
    pub tool_phase: Option<ToolPhase>,
    /// Bounded display metadata derived from the typed tool, never its rendered text.
    pub tool_summary: Option<ToolSummary>,
    /// Position in arrival order. Deltas cannot reorder an opened block.
    pub ordinal: usize,
    pub text: String,
    /// Byte boundary between a read summary and its optional source preview.
    /// Both share the existing slice byte bound; no second output cache.
    pub read_detail_start: Option<usize>,
    /// What each logical row of a diff presentation is, by row index; empty
    /// for every other slice. Indexed by row rather than byte so restating the
    /// tool's first row cannot move them.
    pub marks: Vec<Mark>,
    /// Whether the matching `*.ended` member has been observed.
    ///
    /// An unclosed slice at the end of a transcript is TRUTHFUL, not an error:
    /// it is what an interrupted or still-running execution actually looks
    /// like, and rendering it as closed would claim a completion that did not
    /// happen.
    pub closed: bool,
    /// Bytes refused by the per-slice byte bound.
    ///
    /// Non-zero means the user is seeing less than the engine sent, and the
    /// view says so rather than pretending otherwise.
    pub truncated_bytes: usize,
}
