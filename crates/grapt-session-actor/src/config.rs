use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct ActorConfig {
    /// Commands admitted but not yet started; more are refused.
    pub mailbox: usize,
    /// State changes retained for slow subscribers before they lag.
    pub changes: usize,
    /// Submit outcomes answered from memory on retry.
    pub recent_keys: usize,
    pub write_timeout: Duration,
    pub read_timeout: Duration,
}

impl Default for ActorConfig {
    fn default() -> Self {
        Self {
            mailbox: 64,
            changes: 256,
            recent_keys: 256,
            write_timeout: Duration::from_secs(10),
            read_timeout: Duration::from_secs(5),
        }
    }
}
