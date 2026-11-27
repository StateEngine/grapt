//! Secret-free IBM project and regional destination, selected in Connect.
use super::*;

pub(super) struct Setup {
    pub base: String,
    pub project: grapt_types::WatsonxProjectId,
    pub model: String,
    pub listing: Vec<u8>,
}
impl Setup {
    pub fn new(provider: KeyProvider, request: WatsonxProviderRequestDto) -> Result<Self, String> {
        let region = request.region.as_str();
        if region.is_empty()
            || region.len() > 63
            || region.starts_with('-')
            || region.ends_with('-')
            || !region
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err("enter an IBM watsonx region, such as us-south".into());
        }
        let project = grapt_types::WatsonxProjectId::new(request.project.into_string())
            .ok_or("enter an IBM project identifier")?;
        let model = request.model.into_string();
        let listing = provider.watsonx_listing(&model)?;
        Ok(Self {
            base: format!("https://{region}.ml.cloud.ibm.com"),
            project,
            model,
            listing,
        })
    }
}
