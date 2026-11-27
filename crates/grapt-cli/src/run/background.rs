//! Submit to the existing execution owner without keeping a client attached.

use std::io::Write;

use grapt_types::{MessageId, SessionId};
use serde::Serialize;

use super::{RunError, Runner};
use crate::{AutoReply, BoundedOutput, OutputMode, SessionSelector};

/// Acceptance of a background prompt, separate from its eventual outcome.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BackgroundReceipt {
    pub schema: &'static str,
    #[serde(serialize_with = "super::serialize_session")]
    pub session: SessionId,
    #[serde(serialize_with = "serialize_message")]
    pub message: MessageId,
    pub queued: Option<String>,
}

impl Runner {
    /// Returns after durable admission. The service owns execution and permissions;
    /// a later client can open the Session while this client is gone.
    pub async fn start_background<O: Write, E: Write>(
        &self,
        selector: SessionSelector,
        prompt: String,
        output: &mut BoundedOutput<O, E>,
    ) -> Result<BackgroundReceipt, RunError> {
        if self.client.endpoint().mode() == crate::client::ClientMode::Embedded
            || self.auto != AutoReply::Off
        {
            return Err(RunError::Execution(
                "background runs need a persistent service and an attached client for permission replies"
                    .into(),
            ));
        }
        let (session, _) = self.select_for_run(selector, output.receipt()).await?;
        let accepted = self.admit_prompt(&session, prompt).await?;
        let message: &MessageId = accepted.data().get().id().get().as_inner();
        let receipt = BackgroundReceipt {
            schema: "grapt.cli.background-receipt.v1",
            session,
            message: message.clone(),
            queued: accepted
                .queued()
                .as_present()
                .map(|queued| queued.reason().get().as_str().to_owned()),
        };
        match output.mode() {
            OutputMode::Text => {
                output.text(&format!(
                    "Background prompt accepted: session {}, message {}",
                    receipt.session, receipt.message
                ))?;
                if let Some(reason) = &receipt.queued {
                    output.text(&format!("Not run yet: {reason}"))?;
                }
                output.text(
                    "Open this session in the TUI to follow output and answer permissions.",
                )?;
            }
            OutputMode::Json | OutputMode::Jsonl => output.machine(&receipt)?,
        }
        Ok(receipt)
    }
}

fn serialize_message<S: serde::Serializer>(
    message: &MessageId,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(message.as_str())
}
