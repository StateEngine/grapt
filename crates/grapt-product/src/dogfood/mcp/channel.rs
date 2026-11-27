//! Application subscription binding for held MCP connections.
pub(crate) use grapt_mcp_host::channel::{
    ChannelError, McpChannel, ProgressObserver, ServerRequestHandler,
};
pub(crate) use grapt_mcp_host::connection::ConnectionSlot;
pub(crate) type HeldIo<T> = grapt_mcp_host::connection::HeldIo<T, super::subscriptions::State>;

impl grapt_mcp_host::connection::ConnectionResources for super::subscriptions::State {
    fn close(&self) {
        super::subscriptions::State::close(self);
    }
}

#[cfg(test)]
#[path = "channel_write_tests.rs"]
mod write_tests;
