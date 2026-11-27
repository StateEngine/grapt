//! The live-only facts a stream carries. A cut clears them, unless a resume
//! may continue that stream: then they are held aside whole, and a certified
//! resume puts back exactly what an uninterrupted subscriber still holds.
use super::*;

/// One view's live-only facts, moved out of it as one value.
#[derive(Debug)]
pub(crate) struct LiveQuiescence {
    progress: progress::Messages,
    stripped: Vec<progress::Stripped>,
    streamed_text_bytes: Option<usize>,
    idle: Option<bool>,
    idle_created: Option<u64>,
    idle_seq: Option<u64>,
    mail_attention: Option<grapt_api::u3a::MailAttentionInfo>,
    continuity_attention: Option<grapt_api::u3a::ContinuityAttentionInfo>,
    /// Only transient feedback is live; any other feedback stays on the view.
    feedback: Option<feedback::Feedback>,
}

impl SessionView {
    /// A stream cut: the connection that carried the live-only quiescence fact is gone,
    /// and the fact dies with it. The durable transcript and terminals stand; the screen
    /// returns to the honest unreported until a live edge or a hydration speaks again.
    pub fn invalidate_live_quiescence(&mut self) {
        self.suspend_live_quiescence();
    }

    /// The same clear as [`Self::invalidate_live_quiescence`], keeping what it
    /// cleared: the view shows the honest unreported while the facts are held.
    pub(crate) fn suspend_live_quiescence(&mut self) -> LiveQuiescence {
        let (progress, stripped) = self.clear_tool_progress();
        let transient = self
            .feedback
            .as_ref()
            .is_some_and(feedback::Feedback::transient);
        LiveQuiescence {
            progress,
            stripped,
            streamed_text_bytes: self.streamed_text_bytes.take(),
            idle: self.idle.take(),
            idle_created: self.idle_created.take(),
            idle_seq: self.idle_seq.take(),
            mail_attention: self.mail_attention.take(),
            continuity_attention: self.continuity_attention.take(),
            feedback: if transient {
                self.feedback.take()
            } else {
                None
            },
        }
    }

    /// Put back the facts a cut held aside, once a resume proved the stream
    /// that carried them continues.
    pub(crate) fn restore_live_quiescence(&mut self, facts: LiveQuiescence) {
        self.restore_tool_progress(facts.progress, facts.stripped);
        self.streamed_text_bytes = facts.streamed_text_bytes;
        self.idle = facts.idle;
        self.idle_created = facts.idle_created;
        self.idle_seq = facts.idle_seq;
        self.mail_attention = facts.mail_attention;
        self.continuity_attention = facts.continuity_attention;
        if facts.feedback.is_some() {
            self.feedback = facts.feedback;
        }
    }
}
