//! Effort is selected through the existing atomic session model route.
use super::*;

impl ApiClient {
    pub async fn select_reasoning_effort(
        &self,
        session: &SessionId,
        effort: &str,
    ) -> Result<(), ApiClientError> {
        let effort = (effort != "default")
            .then(|| grapt_types::ModelVariantId::new(effort).ok_or(ApiClientError::Decode))
            .transpose()?;
        let held = self.get_value(&format!("api/session/{session}")).await?;
        let selected = if let Some(model) = held
            .pointer("/data/model")
            .filter(|model| model.is_object())
        {
            model.clone()
        } else {
            let directory = held
                .pointer("/data/location/directory")
                .and_then(Value::as_str)
                .ok_or(ApiClientError::Decode)?;
            let default = self
                .get_value_query("api/model/default", &[("location[directory]", directory)])
                .await?;
            default
                .get("data")
                .filter(|model| model.is_object())
                .cloned()
                .ok_or(ApiClientError::Decode)?
        };
        let id = selected
            .get("id")
            .and_then(Value::as_str)
            .ok_or(ApiClientError::Decode)?;
        let provider = selected
            .get("providerID")
            .and_then(Value::as_str)
            .ok_or(ApiClientError::Decode)?;
        let mut body = serde_json::json!({"model":{"id":id,"providerID":provider}});
        if let Some(effort) = effort {
            body["model"]["variant"] = Value::from(effort.as_str());
        }
        self.send_no_content(Method::POST, &format!("api/session/{session}/model"), &body)
            .await
    }
}
