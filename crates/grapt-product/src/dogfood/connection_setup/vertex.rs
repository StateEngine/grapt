//! Secret-free coordinates are validated before service-account authorization.
use super::*;

pub(super) struct Setup {
    pub base: String,
    pub model: String,
    pub listing: Vec<u8>,
    pub application_default: bool,
}
impl Setup {
    pub fn new(provider: KeyProvider, request: VertexProviderRequestDto) -> Result<Self, String> {
        let application_default = matches!(
            request.auth.as_present(),
            Some(grapt_api::u3a::VertexAuthRequestDto::ApplicationDefault)
        );
        let project = coordinate(
            request.project.as_str(),
            application_default,
            &[
                "GOOGLE_VERTEX_PROJECT",
                "GOOGLE_CLOUD_PROJECT",
                "GCP_PROJECT",
                "GCLOUD_PROJECT",
            ],
            None,
        )?;
        let location = coordinate(
            request.location.as_str(),
            application_default,
            &[
                "GOOGLE_VERTEX_LOCATION",
                "GOOGLE_CLOUD_LOCATION",
                "VERTEX_LOCATION",
            ],
            Some("global"),
        )?;
        if project.is_empty()
            || project.len() > 128
            || !project
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || location.is_empty()
            || location.len() > 63
            || !location
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            || location.starts_with('-')
            || location.ends_with('-')
        {
            return Err(
                "enter a Google Cloud project and Vertex location, such as global or us-central1"
                    .into(),
            );
        }
        let anthropic = provider.family() == grapt_connections::store::Family::VertexAnthropic;
        let host = if matches!(location.as_str(), "us" | "eu") {
            format!("aiplatform.{location}.rep.googleapis.com")
        } else if location == "global" {
            "aiplatform.googleapis.com".to_owned()
        } else {
            format!("{location}-aiplatform.googleapis.com")
        };
        let publisher = if anthropic {
            "anthropic/models"
        } else {
            "google"
        };
        let base = format!(
            "https://{host}/v1/projects/{project}/locations/{location}/publishers/{publisher}"
        );
        grapt_login::vault::ApiDestination::parse(&base)
            .map_err(|_| "Vertex destination is invalid")?;
        let model = request.model.into_string();
        let listing = provider.vertex_listing(&model)?;
        Ok(Self {
            base,
            model,
            listing,
            application_default,
        })
    }
}

fn coordinate(
    input: &str,
    application_default: bool,
    names: &[&str],
    fallback: Option<&str>,
) -> Result<String, String> {
    if !input.is_empty() || !application_default {
        return Ok(input.into());
    }
    for name in names {
        match std::env::var(name) {
            Ok(value) if !value.is_empty() => return Ok(value),
            Ok(_) | Err(std::env::VarError::NotPresent) => {}
            Err(_) => return Err("Google environment coordinate is not valid text".into()),
        }
    }
    fallback.map(str::to_owned).ok_or_else(|| "enter a Google Cloud project or export GOOGLE_VERTEX_PROJECT or GOOGLE_CLOUD_PROJECT to the service".into())
}
