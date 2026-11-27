//! One effective model selection for startup and connection changes.
use super::{invalid, Connections, DefaultModel};
use grapt_config::{
    ConfigBounds, ConfigFileName, ConfigSession, ConfigSourcePath, MaxAncestorDepth,
    MaxDocumentBytes, MaxDocumentNodes, MaxDocuments, MaxPathBytes, MaxValueDepth, Resolution,
    SourceBounds, StdSourceReader,
};
use grapt_types::{ModelId, ModelVariantId, ProviderId};
use std::{io, path::Path};

impl Connections {
    pub fn effective_selection(
        &self,
        resolution: &Resolution<'_>,
    ) -> io::Result<Option<DefaultModel>> {
        let nearest = |path: &[&str]| {
            resolution
                .contributions_at(path)
                .last()
                .map(|value| value.value())
        };
        if nearest(&["default_model"]).is_none() {
            return Ok(self.default_model.clone());
        }
        let text = |path: &[&str]| -> io::Result<Option<String>> {
            nearest(path)
                .map(|value| {
                    value
                        .as_str()
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                        .ok_or_else(|| invalid("default_model fields must be nonempty strings"))
                })
                .transpose()
        };
        let selected = DefaultModel {
            provider: ProviderId::new(
                text(&["default_model", "providerID"])?
                    .ok_or_else(|| invalid("default_model.providerID is absent"))?,
            )
            .ok_or_else(|| invalid("default_model.providerID is empty"))?,
            model: ModelId::new(
                text(&["default_model", "id"])?
                    .ok_or_else(|| invalid("default_model.id is absent"))?,
            )
            .ok_or_else(|| invalid("default_model.id is empty"))?,
            variant: text(&["default_model", "variant"])?
                .map(|value| {
                    ModelVariantId::new(value)
                        .ok_or_else(|| invalid("default_model.variant is empty"))
                })
                .transpose()?,
        };
        // A service-owned disconnect may remove the provider named by a
        // retained user preference. Use its published replacement default.
        if self
            .connections
            .iter()
            .any(|record| record.id == selected.provider)
        {
            Ok(Some(selected))
        } else {
            Ok(self.default_model.clone())
        }
    }
}

fn invalid_bound<T>(_: T) -> io::Error {
    invalid("connection configuration bounds are invalid")
}

pub(crate) fn profile(records: &Connections, profile: &Path) -> io::Result<Option<DefaultModel>> {
    let bounds = ConfigBounds::new(
        MaxDocumentBytes::try_new(64 * 1024).map_err(invalid_bound)?,
        MaxAncestorDepth::try_new(8).map_err(invalid_bound)?,
        MaxDocuments::try_new(9).map_err(invalid_bound)?,
        MaxValueDepth::try_new(16).map_err(invalid_bound)?,
        MaxDocumentNodes::try_new(4096).map_err(invalid_bound)?,
    );
    let path_bound = MaxPathBytes::try_new(MaxPathBytes::CEILING).map_err(invalid_bound)?;
    let source_bounds =
        SourceBounds::from_config_bounds(bounds, path_bound).map_err(invalid_bound)?;
    let start = ConfigSourcePath::new(profile, path_bound)
        .map_err(|_| invalid("profile path is invalid"))?;
    let spelling = grapt_login::profile_files::profile_spelling(profile)
        .map_err(|_| invalid("profile configuration is unreadable"))?
        .spelling;
    let name = ConfigFileName::new(spelling.config_jsonc())
        .ok_or_else(|| invalid("configuration file name is invalid"))?;
    let session = ConfigSession::load(
        grapt_config::DiscoveryPlan::new(start, None, name),
        &StdSourceReader,
        bounds,
        source_bounds,
    )
    .map_err(|_| invalid("profile configuration could not be resolved"))?;
    records.effective_selection(&grapt_config::resolve(session.configuration()))
}
