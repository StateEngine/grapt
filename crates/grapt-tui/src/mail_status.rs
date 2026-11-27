//! Debounce transport drops in presentation; never turn a stale count into a live fact.
use grapt_api::u3a::MailAttentionInfo;
use std::time::Duration;
const GRACE: Duration = Duration::from_secs(30);
#[derive(Default, Clone, PartialEq, Eq)]
pub struct Recovery {
    since: Option<Duration>,
    warned: bool,
    paused_since: Option<Duration>,
    paused_minutes: u64,
}
impl Recovery {
    pub fn advance(&mut self, attention: Option<MailAttentionInfo>, now: Duration) -> bool {
        let previous = (
            self.since,
            self.warned,
            self.paused_since,
            self.paused_minutes,
        );
        if attention == Some(MailAttentionInfo::Stopped) {
            let since = *self.paused_since.get_or_insert(now);
            self.paused_minutes = now.saturating_sub(since).as_secs() / 60;
        } else {
            self.paused_since = None;
            self.paused_minutes = 0;
        }
        if matches!(
            attention,
            Some(MailAttentionInfo::Unavailable | MailAttentionInfo::Retrying)
        ) {
            let since = *self.since.get_or_insert(now);
            self.warned = now.saturating_sub(since) >= GRACE;
        } else {
            self.since = None;
            self.warned = false;
        }
        previous
            != (
                self.since,
                self.warned,
                self.paused_since,
                self.paused_minutes,
            )
    }
    pub fn wait(&self, now: Duration) -> Option<Duration> {
        if let Some(since) = self.paused_since {
            let elapsed = now.saturating_sub(since);
            return Some(
                Duration::from_secs(60 - elapsed.as_secs() % 60)
                    .saturating_sub(Duration::from_nanos(u64::from(elapsed.subsec_nanos()))),
            );
        }
        self.since
            .filter(|_| !self.warned)
            .map(|since| GRACE.saturating_sub(now.saturating_sub(since)))
    }
    pub fn error(&self, attention: Option<MailAttentionInfo>) -> bool {
        matches!(
            attention,
            Some(MailAttentionInfo::RebindRequired | MailAttentionInfo::Stopped)
        ) || self.warned
    }
    pub fn label(&self, attention: Option<MailAttentionInfo>) -> Option<String> {
        match attention {
            Some(MailAttentionInfo::RebindRequired) => Some("mail offline".into()),
            Some(MailAttentionInfo::Stopped) => {
                Some(format!("mail paused {}m", self.paused_minutes))
            }
            Some(MailAttentionInfo::Unavailable | MailAttentionInfo::Retrying) if self.warned => {
                Some("mail offline".into())
            }
            _ => None,
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn paused_duration_advances_on_minute_boundaries_and_resets_on_recovery() {
        let mut state = Recovery::default();
        let stopped = Some(MailAttentionInfo::Stopped);
        state.advance(stopped, Duration::from_secs(10));
        assert_eq!(
            state.wait(Duration::from_secs(69)),
            Some(Duration::from_secs(1))
        );
        assert!(state.advance(stopped, Duration::from_secs(70)));
        assert_eq!(state.paused_minutes, 1);
        state.advance(None, Duration::from_secs(80));
        assert!(state.wait(Duration::from_secs(80)).is_none());
        state.advance(stopped, Duration::from_secs(90));
        assert_eq!(state.paused_minutes, 0);
    }
    #[test]
    fn transient_mail_drops_stay_neutral_and_each_recovery_resets_the_grace() {
        let mut state = Recovery::default();
        state.advance(Some(MailAttentionInfo::Retrying), Duration::ZERO);
        state.advance(
            Some(MailAttentionInfo::Unavailable),
            Duration::from_secs(29),
        );
        assert!(!state.error(Some(MailAttentionInfo::Unavailable)));
        assert_eq!(
            state.wait(Duration::from_secs(29)),
            Some(Duration::from_secs(1))
        );
        state.advance(
            Some(MailAttentionInfo::Unavailable),
            Duration::from_secs(30),
        );
        assert!(state.error(Some(MailAttentionInfo::Unavailable)));
        assert!(state.wait(Duration::from_secs(30)).is_none());
        state.advance(Some(MailAttentionInfo::Disabled), Duration::from_secs(31));
        state.advance(Some(MailAttentionInfo::Retrying), Duration::from_secs(32));
        assert!(!state.error(Some(MailAttentionInfo::Retrying)));
        assert!(state.error(Some(MailAttentionInfo::RebindRequired)));
    }
}
