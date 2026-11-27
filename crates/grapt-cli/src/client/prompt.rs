//! One non-idempotent prompt admission, including explicit prepared images.
use super::*;
use grapt_api::u3a::PromptInputFileAttachmentDto;
impl ApiClient {
    pub async fn prompt(
        &self,
        session: &SessionId,
        text: String,
    ) -> Result<PromptResponseDto, ApiClientError> {
        let text = PromptText::try_new(text).map_err(|_| ApiClientError::PromptBound)?;
        let body = PromptRequestDto::new(
            OptionalNullable::absent(),
            Required::new(text),
            Optional::absent(),
            Optional::absent(),
            Optional::absent(),
            OptionalNullable::absent(),
            OptionalNullable::absent(),
        );
        self.send_json(
            Method::POST,
            &format!("api/session/{session}/prompt"),
            Some(&body),
        )
        .await
    }

    pub async fn prompt_with_files(
        &self,
        session: &SessionId,
        text: String,
        files: &[PromptInputFileAttachmentDto],
    ) -> Result<PromptResponseDto, ApiClientError> {
        self.prompt_with_limits(session, text, files, None).await
    }

    pub async fn prompt_with_limits(
        &self,
        session: &SessionId,
        text: String,
        files: &[PromptInputFileAttachmentDto],
        maximum: Option<std::num::NonZeroU32>,
    ) -> Result<PromptResponseDto, ApiClientError> {
        self.prompt_with_input_id(session, text, files, maximum, None)
            .await
    }

    /// Submit once with an optional caller-owned identity. Transport uncertainty
    /// is returned to the caller, never resolved by automatically replaying input.
    pub async fn prompt_with_input_id(
        &self,
        session: &SessionId,
        text: String,
        files: &[PromptInputFileAttachmentDto],
        maximum: Option<std::num::NonZeroU32>,
        input_id: Option<&grapt_types::MessageId>,
    ) -> Result<PromptResponseDto, ApiClientError> {
        if files.is_empty() && maximum.is_none() && input_id.is_none() {
            return self.prompt(session, text).await;
        }
        if files.len() > 4 {
            return Err(ApiClientError::PromptBound);
        }
        let mut body = serde_json::json!({"text":text});
        if let Some(input_id) = input_id {
            body["id"] = serde_json::json!(input_id.as_str());
            body["delivery"] = serde_json::json!("queue");
            body["metadata"] = serde_json::json!({"origin":"seat-startup"});
        }
        if !files.is_empty() {
            body["files"] = serde_json::json!(files);
        }
        if let Some(maximum) = maximum {
            if input_id.is_some() {
                body["metadata"]["maxSteps"] = serde_json::json!(maximum.get());
            } else {
                body["metadata"] = serde_json::json!({"maxSteps":maximum.get()});
            }
        }
        let bytes = serde_json::to_vec(&body).map_err(|_| ApiClientError::Encode)?;
        grapt_api::u3a::decode_image_prompt(&bytes).map_err(|_| ApiClientError::PromptBound)?;
        let response = self
            .request(Method::POST, &format!("api/session/{session}/prompt"))?
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(bytes)
            .send()
            .await
            .map_err(ApiClientError::Transport)?;
        let response = json_response(response).await?;
        let bytes = bounded_body(response, MAX_RESPONSE_BODY_BYTES).await?;
        let accepted: PromptResponseDto =
            serde_json::from_slice(&bytes).map_err(|_| ApiClientError::Decode)?;
        if input_id.is_some_and(|id| {
            accepted.data().get().id().get().as_inner() != id
                || accepted.data().get().session_id().get().as_inner() != session
        }) {
            return Err(ApiClientError::Decode);
        }
        Ok(accepted)
    }
}
