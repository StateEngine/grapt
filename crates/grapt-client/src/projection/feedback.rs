//! Presentation of normalized service recovery facts; never retry policy.
use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Feedback {
    WaitingForCapacity {
        message: String,
    },
    WaitingForProviderSlot {
        message: String,
    },
    Retry {
        attempt: u64,
        message: String,
        action: Option<String>,
    },
    Failed {
        message: String,
    },
    Interrupted,
}

pub(super) fn bounded(text: &str, bounds: Bounds) -> String {
    let mut end = text.len().min(bounds.max_slice_bytes.min(2048));
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    if end == text.len() {
        text.to_owned()
    } else {
        format!("{}…", &text[..end])
    }
}
impl Feedback {
    // Apache-2.0, Copyright 2025 OpenAI.
    /// Recovery wording shared by clients; renderers own escaping and width.
    pub fn banner(&self, has_draft: bool, automated: bool) -> (String, &str, &str) {
        match self {
            Self::WaitingForCapacity { message } => (
                "Waiting for capacity".into(),
                message.as_str(),
                "Work resumes when capacity is free; Esc cancels.",
            ),
            Self::WaitingForProviderSlot { message } => (
                "Waiting for provider slot".into(),
                message.as_str(),
                "Work resumes when a slot is free; Esc cancels.",
            ),
            Self::Retry {
                attempt,
                message,
                action,
            } => (
                format!("Provider retrying · attempt {attempt}"),
                message.as_str(),
                action
                    .as_deref()
                    .unwrap_or("Wait automatically; Esc interrupts."),
            ),
            Self::Failed { message } if automated => (
                "Wake failed".into(),
                message.as_str(),
                "Your draft is unchanged. Check the service before continuing.",
            ),
            Self::Failed { message } => (
                if !has_draft {
                    "Execution failed"
                } else {
                    "Execution failed · draft kept"
                }
                .into(),
                message.as_str(),
                if !has_draft {
                    "Up recalls input; Enter sends it again."
                } else {
                    "Edit the draft; Enter sends it again."
                },
            ),
            Self::Interrupted => (
                if !has_draft {
                    "Execution interrupted"
                } else {
                    "Execution interrupted · draft kept"
                }
                .into(),
                "Work so far remains in this session.",
                "Stopped; review history, then follow up.",
            ),
        }
    }

    pub(crate) fn transient(&self) -> bool {
        matches!(
            self,
            Self::Retry { .. }
                | Self::WaitingForProviderSlot { .. }
                | Self::WaitingForCapacity { .. }
        )
    }

    pub(super) fn durable(
        outcome: &Outcome,
        error: Option<&crate::wire::DurableError>,
        bounds: Bounds,
    ) -> Option<Self> {
        match outcome {
            Outcome::Failed => Some(Self::Failed {
                message: bounded(
                    error
                        .map(|e| e.message.as_str())
                        .unwrap_or("The execution stopped before completing."),
                    bounds,
                ),
            }),
            Outcome::Interrupted => Some(Self::Interrupted),
            Outcome::Succeeded => None,
        }
    }
}
impl SessionView {
    pub(super) fn retry_feedback(&mut self, envelope: &EventEnvelope, bounds: Bounds) {
        let status = &envelope.data["status"];
        match status["type"].as_str() {
            Some("retry") => {
                // The common envelope validator already checked SessionStatusInfo.
                let action = status
                    .get("action")
                    .and_then(|action| action.get("message"))
                    .and_then(serde_json::Value::as_str)
                    .map(|text| bounded(text, bounds));
                if status["action"]["reason"].as_str() == Some("local_capacity") {
                    self.feedback = Some(Feedback::WaitingForCapacity {
                        message: bounded(status["message"].as_str().unwrap_or_default(), bounds),
                    });
                    return;
                }
                if status["action"]["reason"].as_str() == Some("provider_slot") {
                    self.feedback = Some(Feedback::WaitingForProviderSlot {
                        message: bounded(status["message"].as_str().unwrap_or_default(), bounds),
                    });
                    return;
                }
                self.feedback = Some(Feedback::Retry {
                    attempt: status["attempt"].as_u64().unwrap_or_default(),
                    message: bounded(status["message"].as_str().unwrap_or_default(), bounds),
                    action,
                });
            }
            Some("idle" | "busy") if self.feedback.as_ref().is_some_and(Feedback::transient) => {
                self.feedback = None
            }
            _ => {}
        }
    }
    pub(super) fn terminal_feedback(
        &mut self,
        envelope: &EventEnvelope,
        outcome: &Outcome,
        bounds: Bounds,
    ) {
        let error = envelope
            .data
            .get("error")
            .and_then(|error| error.get("message"))
            .and_then(serde_json::Value::as_str);
        self.feedback = match outcome {
            Outcome::Failed => Some(Feedback::Failed {
                message: bounded(
                    error.unwrap_or("The execution stopped before completing."),
                    bounds,
                ),
            }),
            Outcome::Interrupted => Some(Feedback::Interrupted),
            Outcome::Succeeded => None,
        };
    }
    /// Which terminal a rebuilt assistant record describes.
    ///
    /// `None` settles nothing and leaves the projection as it stands: a record
    /// carrying neither a finish nor an error is not evidence of any outcome,
    /// and inventing one from an absent field is the class of defect the
    /// `Option` on a settled execution id exists to prevent.
    ///
    /// The predicate, exactly:
    ///
    /// ```text
    /// finish ABSENT + error { type: "aborted", message: "Step interrupted" } -> Interrupted
    /// finish ABSENT + any other error                                        -> Failed
    /// finish ABSENT + no error                                               -> settles nothing
    /// finish PRESENT                                                         -> that finish maps
    /// ```
    ///
    /// BOTH ERROR HALVES ARE REQUIRED. `type` alone over-matches, because
    /// `aborted` is also minted for a declined permission, a suspended
    /// permission and a rejected question; `message` alone would accept any
    /// error that happens to read that way. The strings are the oracle's
    /// (`session/runner/llm.ts:107`), not wording invented here.
    ///
    /// A PRESENT `finish` DISQUALIFIES the interrupted shape rather than
    /// competing with it. The contract puts interruption in the structured
    /// error and leaves `finish` absent — `finish` carries the PROVIDER finish
    /// vocabulary, which has no interrupted member — so a record with both is
    /// inconsistent with itself, and this refuses to guess which half to
    /// believe instead of silently preferring one.
    ///
    pub(crate) fn durable_outcome(
        finish: Option<&crate::wire::DurableFinish>,
        error: Option<&crate::wire::DurableError>,
    ) -> Option<Outcome> {
        use crate::wire::DurableFinish;

        const INTERRUPTED_TYPE: &str = "aborted";
        const INTERRUPTED_MESSAGE: &str = "Step interrupted";

        if finish.is_none() {
            return match error {
                Some(error)
                    if error.kind == INTERRUPTED_TYPE && error.message == INTERRUPTED_MESSAGE =>
                {
                    Some(Outcome::Interrupted)
                }
                // An error with no finish is still a terminal: the turn ended
                // and it ended badly. It is simply not THIS terminal, so it
                // settles as the failure it is rather than borrowing the
                // interrupt's meaning.
                Some(_) => Some(Outcome::Failed),
                None => None,
            };
        }

        match finish {
            // A STEP THAT CALLED A TOOL HAS NOT ENDED THE TURN. `tool-calls`
            // settles NOTHING, so the live execution stands and the screen
            // keeps saying `running` while the tool the user just authorised
            // actually runs.
            //
            // Reading it as a terminal is what put `durable succeeded` in the
            // footer of a frame whose permission was still pending — the
            // producer emits it for a CONTINUING step.
            Some(DurableFinish::ToolCalls) => None,
            // `unknown` is the producer saying it has NO terminal for this
            // message, which is what every in-flight assistant message carries.
            // Reading absence of information as success is the same error as
            // reading an absent count as zero.
            Some(DurableFinish::Unknown) => None,
            Some(DurableFinish::Stop) => Some(Outcome::Succeeded),
            Some(DurableFinish::Error) => Some(Outcome::Failed),
            Some(DurableFinish::Other) => Some(Outcome::Succeeded),
            None => None,
        }
    }
}

#[cfg(test)]
mod tests;
